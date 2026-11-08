//! The device hubs: what outlives a project and talks to hardware or the network on threads of
//! its own (OSC, HTTP, audio, DMX out and in, cameras, recordings, scans, the 3D mouse), and the
//! keyboard, mouse and node-body buttons the windows pass on.
//!
//! The engine drives every hub through [`DeviceHub`]: provided to each new cooker, told when the
//! network changes, told when a project closes, shut down when the app closes. [`Hubs`] holds
//! them as typed fields, for the commands and the UI, and lists them in [`Hubs::all`], which the
//! engine iterates for the lifecycle. A new hub is a field, its construction in [`Hubs::new`],
//! its entry in [`Hubs::all`] (the compiler insists) and an implementation of [`DeviceHub`].

use std::sync::Arc;

use penumbra_audio::Hub as AudioHub;
use penumbra_audio::Outputs as AudioOutputs;
use penumbra_camera::Hub as CameraHub;
use penumbra_dmx::Hub as DmxHub;
use penumbra_dmx::Inputs as DmxInputs;
use penumbra_graph::{Cooker, Network, NodeTypes};
use penumbra_http::Hub as HttpHub;
use penumbra_midi::Hub as MidiHub;
use penumbra_net::Hub as NetHub;
use penumbra_ops::audio::beat_grid::BeatFiles;
use penumbra_ops::audio::file::SoundFiles;
use penumbra_ops::chan::controls::Buttons;
use penumbra_ops::keyboard::Keyboard;
use penumbra_ops::mouse::Mouse;
use penumbra_ops::tex::scan::Scans;
use penumbra_record::Hub as RecordHub;
use penumbra_spacenav::SpaceNav;

use crate::engine::RECORD_FINISH;
use crate::osc::OscShared;

/// The lifecycle of a hub, as the engine drives it. Every method but [`DeviceHub::provide`]
/// does nothing unless the hub needs it.
pub trait DeviceHub: Send + Sync {
    /// Makes the hub a resource of a fresh cooker (one per opened project), for its nodes.
    fn provide(&self, cooker: &mut Cooker);

    /// The network changed: open or close what its nodes want. Called on the cook thread.
    fn network_changed(&self, _net: &Network, _types: &NodeTypes) {}

    /// The open project is about to close; another may follow.
    fn project_closing(&self) {}

    /// The app closes; the project's threads have stopped.
    fn shutdown(&self) {}
}

/// Every device hub of the engine. Cheap to share: the hubs are handles themselves.
pub struct Hubs {
    pub osc: Arc<OscShared>,
    /// HTTP and WebSocket in (`chan.http_in`); hands its messages to the OSC hub.
    pub http: HttpHub,
    /// MIDI devices (`chan.midi_in`, `out.midi`); hands its controls to the OSC hub's learn.
    pub midi: MidiHub,
    pub audio: AudioHub,
    /// The audio outputs: one bus per device the `out.audio` nodes play on.
    pub audio_out: AudioOutputs,
    pub dmx: DmxHub,
    pub dmx_in: DmxInputs,
    /// Serial ports, UDP and TCP (`chan.data_in`, `out.data`).
    pub net: NetHub,
    pub camera: CameraHub,
    /// The movie recordings of `out.video` nodes.
    pub record: RecordHub,
    /// The structured light scans of `tex.scan` nodes (`scan.run`).
    pub scans: Scans,
    /// The 3D mouse; its reader starts when the UI or a `chan.spacemouse_in` first asks for it.
    pub spacenav: SpaceNav,
    /// The keys the windows (and `keyboard.press`) pass on, for `chan.keyboard_in`.
    pub keyboard: Keyboard,
    /// The wheel the output windows pass on, and a fake pointer, for `chan.mouse_in`.
    pub mouse: Mouse,
    /// How the `audio.file` nodes stand: their files' codec, rate and length, their playheads.
    pub sound_files: SoundFiles,
    /// The beat analyses of sound files (`chan.beat_grid`, `beats.*`), kept in the data
    /// directory's `cache/beats`.
    pub beat_files: BeatFiles,
    /// The momentary `chan.button`s held in the editor or by `chan.press`.
    pub buttons: Buttons,
}

impl Hubs {
    pub fn new() -> Self {
        let keyboard = Keyboard::new();
        let spacenav = SpaceNav::new();
        let osc = penumbra_osc::Hub::new();
        Hubs {
            http: HttpHub::new(osc.clone()),
            midi: MidiHub::new(osc.clone()),
            osc: Arc::new(OscShared::new(osc, keyboard.clone(), spacenav.clone())),
            audio: AudioHub::new(),
            // The engine's own tests play into buses without a device, never on a speaker.
            audio_out: if cfg!(test) { AudioOutputs::offline(48_000, 2) } else { AudioOutputs::new() },
            dmx: DmxHub::new(),
            dmx_in: DmxInputs::new(),
            net: NetHub::new(),
            camera: CameraHub::new(),
            record: RecordHub::new(),
            scans: Scans::new(),
            spacenav,
            keyboard,
            mouse: Mouse::new(),
            sound_files: SoundFiles::new(),
            beat_files: BeatFiles::new(beat_cache()),
            buttons: Buttons::new(),
        }
    }

    /// Every hub, in the order they shut down. Recordings come first: when the app closes, the
    /// cook thread has dropped its nodes and every recording is finishing its file, which the
    /// shutdown waits for.
    pub fn all(&self) -> [&dyn DeviceHub; 17] {
        // Destructured so that a field missing here does not compile.
        let Hubs {
            osc,
            http,
            midi,
            audio,
            audio_out,
            dmx,
            dmx_in,
            net,
            camera,
            record,
            scans,
            spacenav,
            keyboard,
            mouse,
            sound_files,
            beat_files,
            buttons,
        } = self;
        [
            record,
            osc.as_ref(),
            http,
            midi,
            audio,
            audio_out,
            dmx,
            dmx_in,
            net,
            camera,
            scans,
            spacenav,
            keyboard,
            mouse,
            sound_files,
            beat_files,
            buttons,
        ]
    }
}

impl Default for Hubs {
    fn default() -> Self {
        Hubs::new()
    }
}

impl DeviceHub for OscShared {
    fn provide(&self, cooker: &mut Cooker) {
        cooker.provide(self.hub().clone());
    }

    fn network_changed(&self, net: &Network, types: &NodeTypes) {
        self.hub().set_wanted(&penumbra_ops::osc::wants(net, types));
    }

    fn shutdown(&self) {
        self.hub().shutdown();
    }
}

impl DeviceHub for HttpHub {
    fn provide(&self, cooker: &mut Cooker) {
        cooker.provide(self.clone());
    }

    fn network_changed(&self, net: &Network, types: &NodeTypes) {
        self.set_wanted(&penumbra_ops::http::wants(net, types));
    }

    fn shutdown(&self) {
        HttpHub::shutdown(self);
    }
}

impl DeviceHub for MidiHub {
    fn provide(&self, cooker: &mut Cooker) {
        cooker.provide(self.clone());
    }

    fn network_changed(&self, net: &Network, types: &NodeTypes) {
        let (inputs, outputs) = penumbra_ops::midi::wants(net, types);
        self.set_wanted(&inputs, &outputs);
    }

    fn shutdown(&self) {
        MidiHub::shutdown(self);
    }
}

impl DeviceHub for AudioHub {
    fn provide(&self, cooker: &mut Cooker) {
        cooker.provide(self.clone());
    }

    fn network_changed(&self, net: &Network, types: &NodeTypes) {
        AudioHub::set_wanted(self, &penumbra_ops::audio::wants(net, types));
    }

    fn shutdown(&self) {
        AudioHub::shutdown(self);
    }
}

impl DeviceHub for AudioOutputs {
    fn provide(&self, cooker: &mut Cooker) {
        cooker.provide(self.clone());
    }

    /// Opens the devices the `out.audio` nodes want; the others fade out and close.
    fn network_changed(&self, net: &Network, types: &NodeTypes) {
        self.set_wanted(&penumbra_ops::out::audio::wants(net, types));
    }

    /// The next project's nodes play anew; what plays now fades out.
    fn project_closing(&self) {
        self.set_wanted(&[]);
    }

    fn shutdown(&self) {
        AudioOutputs::shutdown(self);
    }
}

impl DeviceHub for SoundFiles {
    fn provide(&self, cooker: &mut Cooker) {
        cooker.provide(self.clone());
    }

    /// Node ids of the next project mean other nodes.
    fn project_closing(&self) {
        self.prune(|_| false);
    }
}

/// Where beat analyses are kept: `cache/beats` in the data directory. The engine's own tests
/// keep theirs apart, in the temp directory, in one place for every run: a sound file that stays
/// the same goes through the network once, not once per run.
fn beat_cache() -> Option<std::path::PathBuf> {
    if cfg!(test) {
        return Some(std::env::temp_dir().join("penumbra-engine-beats"));
    }
    crate::projects::data_dir().ok().map(|d| d.join("cache").join("beats"))
}

impl DeviceHub for BeatFiles {
    fn provide(&self, cooker: &mut Cooker) {
        cooker.provide(self.clone());
    }

    /// Analyses no node wants any more stop by themselves; the app's end stops the rest.
    fn shutdown(&self) {
        BeatFiles::shutdown(self);
    }
}

impl DeviceHub for DmxHub {
    fn provide(&self, cooker: &mut Cooker) {
        cooker.provide(self.clone());
    }

    fn shutdown(&self) {
        DmxHub::shutdown(self);
    }
}

impl DeviceHub for DmxInputs {
    fn provide(&self, cooker: &mut Cooker) {
        cooker.provide(self.clone());
    }

    fn shutdown(&self) {
        DmxInputs::shutdown(self);
    }
}

impl DeviceHub for NetHub {
    fn provide(&self, cooker: &mut Cooker) {
        cooker.provide(self.clone());
    }

    fn shutdown(&self) {
        NetHub::shutdown(self);
    }
}

impl DeviceHub for CameraHub {
    fn provide(&self, cooker: &mut Cooker) {
        cooker.provide(self.clone());
    }

    fn network_changed(&self, net: &Network, types: &NodeTypes) {
        CameraHub::set_wanted(self, &penumbra_ops::tex::video_in::wants(net, types));
    }

    fn shutdown(&self) {
        CameraHub::shutdown(self);
    }
}

impl DeviceHub for RecordHub {
    fn provide(&self, cooker: &mut Cooker) {
        cooker.provide(self.clone());
    }

    fn shutdown(&self) {
        RecordHub::shutdown(self, RECORD_FINISH);
    }
}

impl DeviceHub for Scans {
    fn provide(&self, cooker: &mut Cooker) {
        cooker.provide(self.clone());
    }

    /// Node ids of the next project mean other nodes; a scan going on stops.
    fn project_closing(&self) {
        self.clear();
    }
}

impl DeviceHub for SpaceNav {
    /// The viewer and `chan.spacemouse_in` read the same reader.
    fn provide(&self, cooker: &mut Cooker) {
        cooker.provide(self.clone());
    }

    /// A `chan.spacemouse_in` opens the device (headless runs too); it stays open.
    fn network_changed(&self, net: &Network, _types: &NodeTypes) {
        if penumbra_ops::spacemouse::wanted(net) {
            self.start();
        }
    }

    fn shutdown(&self) {
        SpaceNav::shutdown(self);
    }
}

impl DeviceHub for Keyboard {
    fn provide(&self, cooker: &mut Cooker) {
        cooker.provide(self.clone());
    }

    fn network_changed(&self, net: &Network, _types: &NodeTypes) {
        self.set_wanted(penumbra_ops::keyboard::wanted(net));
    }

    /// The next project starts with no key pressed, so its `last` channels are 0.
    fn project_closing(&self) {
        self.reset();
    }
}

impl DeviceHub for Buttons {
    fn provide(&self, cooker: &mut Cooker) {
        cooker.provide(self.clone());
    }

    fn network_changed(&self, net: &Network, _types: &NodeTypes) {
        let button = penumbra_ops::chan::controls::BUTTON;
        self.prune(|id| net.node(id).is_some_and(|n| n.kind == button));
    }

    /// The next project starts with every button up.
    fn project_closing(&self) {
        self.reset();
    }
}

impl DeviceHub for Mouse {
    fn provide(&self, cooker: &mut Cooker) {
        cooker.provide(self.clone());
    }

    fn network_changed(&self, net: &Network, _types: &NodeTypes) {
        self.set_wanted(penumbra_ops::mouse::wanted(net));
    }

    /// The next project's `wheel` starts at 0.
    fn project_closing(&self) {
        self.reset();
    }
}

#[cfg(test)]
mod tests {
    use penumbra_graph::JobPool;

    use super::*;

    #[test]
    fn every_hub_the_nodes_read_is_provided() {
        let hubs = Hubs::new();
        let types = Arc::new(penumbra_ops::builtin_types().unwrap());
        let mut cooker = Cooker::new(types, Arc::new(JobPool::new(1).unwrap()));
        for hub in hubs.all() {
            hub.provide(&mut cooker);
        }
        assert!(cooker.resource::<penumbra_osc::Hub>().is_some());
        assert!(cooker.resource::<HttpHub>().is_some());
        assert!(cooker.resource::<NetHub>().is_some());
        assert!(cooker.resource::<MidiHub>().is_some());
        assert!(cooker.resource::<AudioHub>().is_some());
        assert!(cooker.resource::<AudioOutputs>().is_some());
        assert!(cooker.resource::<DmxHub>().is_some());
        assert!(cooker.resource::<DmxInputs>().is_some());
        assert!(cooker.resource::<CameraHub>().is_some());
        assert!(cooker.resource::<RecordHub>().is_some());
        assert!(cooker.resource::<Scans>().is_some());
        assert!(cooker.resource::<Keyboard>().is_some());
        assert!(cooker.resource::<SpaceNav>().is_some());
        assert!(cooker.resource::<Mouse>().is_some());
        for hub in hubs.all() {
            hub.shutdown();
        }
    }
}
