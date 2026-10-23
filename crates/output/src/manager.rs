//! Keeps the output windows in line with the projector nodes, on the event loop thread.
//!
//! A few times per second ([`Outputs::tick`]): enumerate the displays (winit for position,
//! size and refresh rate, the system crate for stable ids), resolve every active projector's
//! display, open a window for each display that has a projector and close the others, choose
//! the window that paces the show clock, and report everything to the [`OutputBoard`], where
//! the projector nodes and `output.list` read it. While windows are open the displays are kept
//! from sleeping.
//!
//! A projector whose display cannot be found for certain shows nothing: a show must not end up
//! on another wall. Its node gets the error, and every change of what a projector is shown on
//! (or why not) is logged once. A display that drops out for a moment (a glitch in the
//! enumeration) keeps its window for [`GRACE_TICKS`] while the window is still on it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_core::NodeId;
use penumbra_engine::Engine;
use penumbra_gpu::Gpu;
use penumbra_ops::keyboard::{Key, Source};
use penumbra_ops::out::{DisplayInfo, OutputBoard, ProjectorState};
use penumbra_system::{DisplayDetails, KeepAwake, Problem};
use winit::event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key as LogicalKey, ModifiersState, PhysicalKey};
use winit::monitor::MonitorHandle;
use winit::window::{Fullscreen, WindowId};

use crate::window::OutputWindow;

/// How often displays and projectors are looked at.
pub const TICK: Duration = Duration::from_millis(250);
/// A display may pace the show clock when its refresh rate is this close to the show rate.
const PACE_TOLERANCE: f64 = 0.01;
/// How many ticks a projector stays on its window after its display stopped resolving.
pub const GRACE_TICKS: u32 = 4;

/// Where a projector was last shown.
#[derive(Clone, Debug, PartialEq)]
struct Shown {
    /// The id as the projector stores it.
    stored: String,
    display: String,
    os_name: String,
    target: String,
    /// Ticks its display has not resolved since.
    lost: u32,
}

/// The output windows of the app.
pub struct Outputs {
    engine: Arc<Engine>,
    gpu: Option<(wgpu::Instance, Gpu)>,
    windows: Vec<OutputWindow>,
    last_tick: Option<Instant>,
    /// Displays whose window failed to open, with the error, so it is not retried every tick;
    /// cleared when the display set changes.
    failed: BTreeMap<String, String>,
    last_displays: Vec<String>,
    /// Where each projector was last shown, for the grace and the log.
    shown: BTreeMap<NodeId, Shown>,
    /// What was last reported per projector, so changes are logged once.
    reported: BTreeMap<NodeId, ProjectorState>,
    /// Keeps the displays from sleeping while windows are open: Windows would otherwise turn
    /// the projectors off after its idle timeout, in the middle of a show.
    awake: KeepAwake,
    /// The modifiers held over the output windows: a key pressed with Ctrl, Alt or the
    /// Windows key is a shortcut, not a key for the show.
    modifiers: ModifiersState,
}

impl Outputs {
    pub fn new(engine: Arc<Engine>) -> Self {
        engine.board().update(|s| s.managed = true);
        Self {
            engine,
            gpu: None,
            windows: Vec::new(),
            last_tick: None,
            failed: BTreeMap::new(),
            last_displays: Vec::new(),
            shown: BTreeMap::new(),
            reported: BTreeMap::new(),
            awake: KeepAwake::new(),
            modifiers: ModifiersState::empty(),
        }
    }

    /// The instance that created the shared device (eframe's), for the windows' surfaces.
    pub fn attach_gpu(&mut self, instance: wgpu::Instance, gpu: Gpu) {
        self.gpu = Some((instance, gpu));
    }

    fn board(&self) -> &OutputBoard {
        self.engine.board()
    }

    /// Handles an event of an output window; `false` when `id` is not one.
    pub fn window_event(&mut self, id: WindowId, event: &WindowEvent) -> bool {
        let Some(w) = self.windows.iter().find(|w| w.window.id() == id) else { return false };
        match event {
            WindowEvent::Resized(size) => w.resized(size.width, size.height),
            // Alt+F4 on a projector during a show must not close it; projectors close by
            // deactivating or deleting their node.
            WindowEvent::CloseRequested => log::info!("output {}: close ignored", w.display),
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                let keyboard = self.engine.keyboard();
                if keyboard.wanted() {
                    let down = event.state == ElementState::Pressed;
                    let shortcut = self.modifiers.control_key()
                        || self.modifiers.alt_key()
                        || self.modifiers.super_key();
                    let key = if down && !shortcut { key_of(event) } else { None };
                    keyboard.key(Source::Output, &format!("{:?}", event.physical_key), key, down);
                }
            }
            // The wheel over a projector, for `chan.mouse_in` (position and buttons it polls).
            WindowEvent::MouseWheel { delta, .. } => {
                let mouse = self.engine.mouse();
                if mouse.wanted() {
                    mouse.wheel(match delta {
                        MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                        // Windows reports lines; a pixel delta is taken as 120 to the notch.
                        MouseScrollDelta::PixelDelta(p) => p.y / 120.0,
                    });
                }
            }
            // Keys held when the focus goes elsewhere never see their release here.
            WindowEvent::Focused(false) => {
                self.modifiers = ModifiersState::empty();
                self.engine.keyboard().release_all(Source::Output);
            }
            _ => {}
        }
        true
    }

    /// When the event loop should wake next for [`Outputs::tick`].
    pub fn next_tick(&self) -> Instant {
        self.last_tick.map_or_else(Instant::now, |t| t + TICK)
    }

    /// Reconciles windows with projectors, at most every [`TICK`].
    pub fn tick(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if self.last_tick.is_some_and(|t| now < t + TICK) {
            return;
        }
        self.last_tick = Some(now);
        let monitors: Vec<MonitorHandle> = event_loop.available_monitors().collect();
        let editor = self.board().state().editor;
        let (details, infos) = describe(&monitors, editor);
        let ids: Vec<String> = infos.iter().map(|d| d.id.clone()).collect();
        if ids != self.last_displays {
            log::info!("displays: {}", ids.join(", "));
            self.failed.clear();
            self.last_displays = ids;
        }

        // Which display each projector goes to.
        let frame = self.engine.output();
        let mut states: BTreeMap<NodeId, ProjectorState> = BTreeMap::new();
        let mut wanted: BTreeMap<String, Vec<NodeId>> = BTreeMap::new();
        for p in frame.projectors.iter().filter(|p| p.active && !p.display.is_empty()) {
            let mut state = ProjectorState::default();
            let resolved = penumbra_system::resolve(&p.display, &details);
            match resolved.problem(&p.display, &details) {
                Some(Problem::Error(e)) => state.error = Some(e),
                Some(Problem::Warning(w)) => state.warning = Some(w),
                None => {}
            }
            match resolved.display() {
                Some(d) if infos.iter().any(|i| i.id == d.id && i.editor) => {
                    state.error = Some(format!(
                        "`{}` shows the editor; move the editor to another display, or give this projector another display",
                        d.id
                    ));
                    self.shown.remove(&p.node);
                }
                Some(d) => {
                    state.display = Some(d.id.clone());
                    wanted.entry(d.id.clone()).or_default().push(p.node);
                    let now = Shown {
                        stored: p.display.clone(),
                        display: d.id.clone(),
                        os_name: d.os_name.clone(),
                        target: d.target.clone(),
                        lost: 0,
                    };
                    if let Some(before) = self.shown.insert(p.node, now)
                        && before.display == d.id
                        && before.target != d.target
                    {
                        log::info!(
                            "projector {}: `{}` moved from {} to {}, following it",
                            self.path(p.node),
                            d.id,
                            before.target,
                            d.target
                        );
                    }
                }
                None => match self.hold(p.node, &p.display, &monitors, &details) {
                    Some(display) => {
                        // The error is not shown yet: the display may be back next tick.
                        state = ProjectorState { display: Some(display.clone()), ..Default::default() };
                        wanted.entry(display).or_default().push(p.node);
                    }
                    None => {
                        self.shown.remove(&p.node);
                    }
                },
            }
            states.insert(p.node, state);
        }
        self.shown.retain(|node, _| states.contains_key(node));

        // Close windows nobody wants, whose display went away or moved, or whose thread died.
        let mut i = 0;
        while let Some(w) = self.windows.get(i) {
            let on_its_display =
                w.window.current_monitor().and_then(|m| m.name()).is_none_or(|n| n == w.os_name);
            let keep = wanted.contains_key(&w.display) && on_its_display && w.failed().is_none();
            if keep {
                i += 1;
                continue;
            }
            if let Some(e) = w.failed() {
                self.failed.insert(w.display.clone(), e);
            }
            log::info!("output {}: closing", w.display);
            let mut w = self.windows.remove(i);
            w.stop();
        }

        // Open the missing ones.
        for (display, nodes) in &wanted {
            if self.windows.iter().any(|w| &w.display == display) || self.failed.contains_key(display) {
                continue;
            }
            let Some(info) = infos.iter().find(|d| &d.id == display) else { continue };
            let Some(monitor) = monitors.iter().find(|m| m.name().as_deref() == Some(info.os_name.as_str()))
            else {
                continue;
            };
            match self.open(event_loop, monitor, info) {
                Ok(w) => {
                    log::info!("output {display}: open, {}x{}", info.size[0], info.size[1]);
                    w.set_nodes(nodes.clone());
                    self.windows.push(w);
                }
                Err(e) => {
                    log::error!("output {display}: {e}");
                    self.failed.insert(display.clone(), e);
                }
            }
        }
        for w in &self.windows {
            w.set_nodes(wanted.get(&w.display).cloned().unwrap_or_default());
        }
        for state in states.values_mut() {
            if let Some(e) = state.display.as_ref().and_then(|d| self.failed.get(d)) {
                state.error = Some(format!("the output window failed: {e}"));
            }
        }
        self.report(&frame.projectors, &states, &infos);

        // The window that paces: the first (by id) whose refresh rate matches the show rate.
        let rate = self.engine.view().time.map_or(60.0, |t| 1.0 / t.rate.period().as_secs_f64().max(1e-6));
        let pacer = self
            .windows
            .iter()
            .filter(|w| {
                infos.iter().any(|d| {
                    d.id == w.display
                        && d.refresh_hz.is_some_and(|hz| (hz / rate - 1.0).abs() <= PACE_TOLERANCE)
                })
            })
            .map(|w| w.display.clone())
            .min();
        for w in &self.windows {
            w.set_paces(Some(&w.display) == pacer.as_ref());
        }

        let awake = self.awake.update(self.windows.len());
        let windows = self.windows.iter().map(OutputWindow::info).collect();
        self.board().update(|s| {
            s.displays = infos;
            s.windows = windows;
            s.keeps_display_awake = awake;
            s.projectors = states;
        });
    }

    /// Whether projector `node`, whose display did not resolve this tick, keeps its window for
    /// now: within [`GRACE_TICKS`], with the id unchanged, its window still open on the same
    /// desktop display and that display not taken by another monitor. Returns the display id it
    /// keeps.
    fn hold(
        &mut self,
        node: NodeId,
        stored: &str,
        monitors: &[MonitorHandle],
        details: &[DisplayDetails],
    ) -> Option<String> {
        let shown = self.shown.get_mut(&node)?;
        shown.lost += 1;
        let shown = shown.clone();
        let window = self.windows.iter().find(|w| w.display == shown.display)?;
        let on_it =
            window.window.current_monitor().and_then(|m| m.name()).is_some_and(|n| n == shown.os_name);
        let listed = monitors.iter().any(|m| m.name().as_deref() == Some(shown.os_name.as_str()));
        // A display named by the system crate under the same OS name but with another id is
        // another monitor now; one named only by winit (the system crate failed) may be the same.
        let not_taken = details
            .iter()
            .find(|d| d.os_name == shown.os_name)
            .is_none_or(|d| d.id == shown.display || d.target_id.is_none());
        if shown.lost > GRACE_TICKS || shown.stored != stored || !on_it || !listed || !not_taken {
            return None;
        }
        if shown.lost == 1 {
            log::info!(
                "projector {}: `{}` not found, keeping its window for up to {} ms",
                self.path(node),
                shown.display,
                u128::from(GRACE_TICKS) * TICK.as_millis()
            );
        }
        Some(shown.display)
    }

    /// The node's path for log lines.
    fn path(&self, node: NodeId) -> String {
        let net = self.engine.session().doc().snapshot();
        net.path_of(node).map_or_else(|| format!("{node:?}"), |p| p.to_string())
    }

    /// Logs every projector whose state changed since the last tick, once.
    fn report(
        &mut self,
        projectors: &[penumbra_ops::out::ProjectorFrame],
        states: &BTreeMap<NodeId, ProjectorState>,
        infos: &[DisplayInfo],
    ) {
        for (node, state) in states {
            if self.reported.get(node) == Some(state) {
                continue;
            }
            let path = self.path(*node);
            let stored = projectors.iter().find(|p| p.node == *node).map_or("", |p| p.display.as_str());
            if let Some(e) = &state.error {
                log::warn!("projector {path} (display `{stored}`): {e}");
            } else if let Some(w) = &state.warning {
                log::warn!("projector {path} (display `{stored}`): {w}");
            }
            if let Some(d) = &state.display {
                let how = infos.iter().find(|i| &i.id == d).map_or(String::new(), |i| {
                    format!(" ({}, {}, {})", i.os_name, i.target, i.serial.as_deref().unwrap_or("no serial"))
                });
                log::info!("projector {path}: shown on `{d}`{how}");
            } else {
                log::info!("projector {path}: nothing shown");
            }
        }
        self.reported = states.clone();
    }

    fn open(
        &self,
        event_loop: &ActiveEventLoop,
        monitor: &MonitorHandle,
        info: &DisplayInfo,
    ) -> Result<OutputWindow, String> {
        let Some((instance, gpu)) = &self.gpu else {
            return Err("no GPU yet".into());
        };
        let attrs = winit::window::Window::default_attributes()
            .with_title(format!("penumbra output {}", info.id))
            .with_fullscreen(Some(Fullscreen::Borderless(Some(monitor.clone()))))
            .with_active(false);
        let window =
            Arc::new(event_loop.create_window(attrs).map_err(|e| format!("cannot open the window: {e}"))?);
        window.set_cursor_visible(false);
        OutputWindow::start(
            window,
            info.id.clone(),
            info.os_name.clone(),
            info.refresh_hz.unwrap_or(60.0),
            instance,
            gpu.clone(),
            Arc::clone(&self.engine),
        )
    }

    /// Stops every output thread and closes the windows (app exit).
    pub fn shutdown(&mut self) {
        for mut w in self.windows.drain(..) {
            w.stop();
        }
        self.awake.update(0);
        self.board().update(|s| {
            s.windows.clear();
            s.keeps_display_awake = false;
            s.managed = false;
        });
    }
}

/// The displays winit sees, named by the system crate where it knows them (Windows) and by
/// winit's own name otherwise (Linux: the connector, e.g. `DP-1`).
/// The key a key event means: by what the layout puts on it (a letter as labelled), else by
/// its place (Shift+1 is still 1, a digit on a layout whose number row types other characters).
fn key_of(event: &KeyEvent) -> Option<Key> {
    let logical = match &event.logical_key {
        LogicalKey::Character(c) => Key::parse(c.as_str()),
        LogicalKey::Named(n) => Key::parse(&format!("{n:?}")),
        _ => None,
    };
    logical.or_else(|| match event.physical_key {
        PhysicalKey::Code(code) => Key::parse(&format!("{code:?}")),
        PhysicalKey::Unidentified(_) => None,
    })
}

fn describe(monitors: &[MonitorHandle], editor: Option<[i32; 4]>) -> (Vec<DisplayDetails>, Vec<DisplayInfo>) {
    let named = penumbra_system::displays().unwrap_or_else(|e| {
        log::warn!("display names: {e}");
        Vec::new()
    });
    let mut details = Vec::new();
    let mut infos = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, m) in monitors.iter().enumerate() {
        let os_name = m.name().unwrap_or_else(|| format!("display {}", i + 1));
        if !seen.insert(os_name.clone()) {
            continue;
        }
        let refresh = m.refresh_rate_millihertz().map(|mhz| f64::from(mhz) / 1000.0);
        let d = named.iter().find(|d| d.os_name == os_name).cloned().unwrap_or_else(|| DisplayDetails {
            id: os_name.clone(),
            os_name: os_name.clone(),
            monitor: os_name.clone(),
            connector: "other",
            refresh_hz: refresh,
            ..Default::default()
        });
        let (pos, size) = (m.position(), m.size());
        infos.push(DisplayInfo {
            id: d.id.clone(),
            monitor: d.monitor.clone(),
            edid: d.edid.clone(),
            serial: d.serial.clone(),
            target: d.target.clone(),
            connector: d.connector.to_string(),
            os_name,
            size: [size.width, size.height],
            position: [pos.x, pos.y],
            refresh_hz: d.refresh_hz.or(refresh),
            editor: false,
        });
        details.push(d);
    }
    if let Some(rect) = editor {
        mark_editor(&mut infos, rect);
    }
    (details, infos)
}

/// Marks the display with the largest share of the editor window (`x, y, w, h` in physical
/// desktop pixels).
fn mark_editor(infos: &mut [DisplayInfo], [x, y, w, h]: [i32; 4]) {
    let overlap = |d: &DisplayInfo| {
        let (dx, dy) = (i64::from(d.position[0]), i64::from(d.position[1]));
        let (dw, dh) = (i64::from(d.size[0]), i64::from(d.size[1]));
        let ox = (i64::from(x) + i64::from(w)).min(dx + dw) - i64::from(x).max(dx);
        let oy = (i64::from(y) + i64::from(h)).min(dy + dh) - i64::from(y).max(dy);
        ox.max(0) * oy.max(0)
    };
    let best = infos.iter().enumerate().map(|(i, d)| (overlap(d), i)).max();
    if let Some((area, i)) = best
        && area > 0
        && let Some(d) = infos.get_mut(i)
    {
        d.editor = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, x: i32, w: u32) -> DisplayInfo {
        DisplayInfo {
            id: id.into(),
            monitor: id.into(),
            os_name: id.into(),
            size: [w, 1080],
            position: [x, 0],
            refresh_hz: Some(60.0),
            ..DisplayInfo::default()
        }
    }

    #[test]
    fn the_editor_is_where_most_of_it_is() {
        let mut list = vec![info("a", 0, 3840), info("b", 3840, 1920)];
        mark_editor(&mut list, [3000, 100, 1600, 900]);
        assert_eq!(list.iter().map(|d| d.editor).collect::<Vec<_>>(), [true, false]);
        let mut list = vec![info("a", 0, 3840), info("b", 3840, 1920)];
        mark_editor(&mut list, [3700, 100, 1600, 900]);
        assert_eq!(list.iter().map(|d| d.editor).collect::<Vec<_>>(), [false, true]);
        let mut list = vec![info("a", 0, 3840)];
        mark_editor(&mut list, [-5000, 0, 100, 100]);
        assert!(!list[0].editor);
    }
}
