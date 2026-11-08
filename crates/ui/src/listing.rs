//! Lists that take long to make (devices, files), made on a thread of their own when a menu
//! opens, so the window keeps drawing meanwhile. Listing cameras opens every camera, which can
//! take a second.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use egui::{RichText, Ui};

use crate::theme;

/// A menu opened again this soon after its list landed shows that list instead of making a new
/// one: closing and opening a menu quickly started a camera listing each time.
const FRESH: Duration = Duration::from_secs(1);

/// The list of one menu at a time (`key`): made when the menu opens, kept while it stays open,
/// made again on its next opening. Until the first list lands the menu says "looking...".
pub struct Listing<T> {
    key: Option<String>,
    /// The newest list and when it landed.
    list: Option<(T, Instant)>,
    /// The list being made.
    pending: Option<Receiver<T>>,
    /// Why no list could be made.
    error: Option<String>,
    /// The menu was open last frame.
    open: bool,
}

impl<T> Default for Listing<T> {
    fn default() -> Self {
        Self { key: None, list: None, pending: None, error: None, open: false }
    }
}

impl<T: Send + 'static> Listing<T> {
    /// The list for the menu `key`, drawn open now: a menu that just opened starts `make` on a
    /// thread. Draws "looking..." into `ui` while a list is being made, and the newest list there
    /// is meanwhile (none before the first lands). Text drawn into `ui` after this call does not
    /// wrap.
    pub fn get(
        &mut self,
        ui: &mut Ui,
        key: &str,
        name: &str,
        make: impl FnOnce() -> T + Send + 'static,
    ) -> Option<&T> {
        if self.key.as_deref() != Some(key) {
            *self = Self { key: Some(key.to_string()), ..Self::default() };
        }
        if !self.open {
            self.open = true;
            let fresh = self.list.as_ref().is_some_and(|(_, at)| at.elapsed() < FRESH);
            if self.pending.is_none() && !fresh {
                self.start(name, make);
            }
        }
        self.poll();
        // The menu is placed by its size while it said "looking...": without this, the list
        // that lands later wraps every entry into that narrow width.
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        if self.pending.is_some() {
            ui.label(RichText::new("looking...").color(theme::TEXT_WEAK));
            ui.ctx().request_repaint_after(Duration::from_millis(30));
        }
        if let Some(e) = &self.error {
            ui.colored_label(theme::WARNING, e);
        }
        self.list.as_ref().map(|(list, _)| list)
    }

    /// The menu `key` was drawn closed: its next opening lists anew.
    pub fn closed(&mut self, key: &str) {
        if self.key.as_deref() == Some(key) {
            self.open = false;
        }
    }

    fn start(&mut self, name: &str, make: impl FnOnce() -> T + Send + 'static) {
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name(format!("list {name}")).spawn(move || {
            tx.send(make()).ok();
        });
        match spawned {
            Ok(_) => {
                self.pending = Some(rx);
                self.error = None;
            }
            Err(e) => self.error = Some(format!("cannot list the {name}: {e}")),
        }
    }

    fn poll(&mut self) {
        let Some(rx) = &self.pending else { return };
        match rx.try_recv() {
            Ok(list) => {
                self.list = Some((list, Instant::now()));
                self.pending = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                self.error = Some("the listing stopped without a result".into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame of a menu drawn open.
    fn frame(
        ctx: &egui::Context,
        listing: &mut Listing<u32>,
        key: &str,
        make: fn() -> u32,
    ) -> (Option<u32>, bool) {
        let mut out = (None, false);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            out.0 = listing.get(ui, key, "numbers", make).copied();
            out.1 = listing.pending.is_some();
        });
        // Nothing is painted here.
        output.textures_delta.clear();
        out
    }

    fn wait_for(ctx: &egui::Context, listing: &mut Listing<u32>, key: &str, make: fn() -> u32) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let (Some(v), false) = frame(ctx, listing, key, make) {
                return v;
            }
            assert!(Instant::now() < deadline, "the list never landed");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn a_list_is_made_off_the_ui_thread_and_again_on_the_next_opening() {
        let ctx = egui::Context::default();
        let mut listing = Listing::default();
        let slow = || {
            std::thread::sleep(Duration::from_millis(200));
            1
        };
        let started = Instant::now();
        assert_eq!(frame(&ctx, &mut listing, "a", slow), (None, true), "looking at first");
        assert!(started.elapsed() < Duration::from_millis(150), "the frame did not wait for the list");
        assert_eq!(wait_for(&ctx, &mut listing, "a", slow), 1);
        // Open: kept, no new list.
        assert_eq!(frame(&ctx, &mut listing, "a", || 2), (Some(1), false));
        // Closed and opened at once: the list is fresh enough.
        listing.closed("a");
        assert_eq!(frame(&ctx, &mut listing, "a", || 2), (Some(1), false));
        // Opened again later: the old list shows while the new one is made.
        listing.closed("a");
        if let Some((_, at)) = &mut listing.list {
            *at = Instant::now().checked_sub(FRESH * 2).unwrap_or(*at);
        }
        let later = || {
            std::thread::sleep(Duration::from_millis(200));
            2
        };
        assert_eq!(frame(&ctx, &mut listing, "a", later), (Some(1), true));
        assert_eq!(wait_for(&ctx, &mut listing, "a", later), 2);
        // Another menu starts over.
        assert_eq!(wait_for(&ctx, &mut listing, "b", || 3), 3);
    }
}
