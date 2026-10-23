//! The device hubs: what outlives a project and talks to hardware or the network on threads of
//! its own (OSC, audio, DMX out and in, cameras, recordings, scans, the 3D mouse).
//!
//! The engine drives every hub through [`DeviceHub`]: provided to each new cooker, told when the
//! network changes, told when a project closes, shut down when the app closes. [`Hubs`] holds
//! them as typed fields, for the commands and the UI, and lists them in [`Hubs::all`], which the
//! engine iterates for the lifecycle. A new hub is a field, its construction in [`Hubs::new`],
//! its entry in [`Hubs::all`] (the compiler insists) and an implementation of [`DeviceHub`].

use std::sync::Arc;

use penumbra_audio::Hub as AudioHub;
use penumbra_camera::Hub as CameraHub;
use penumbra_dmx::Hub as DmxHub;
use penumbra_dmx::Inputs as DmxInputs;
use penumbra_graph::{Cooker, Network, NodeTypes};
use penumbra_ops::keyboard::Keyboard;
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
    pub audio: AudioHub,
    pub dmx: DmxHub,
    pub dmx_in: DmxInputs,
    pub camera: CameraHub,
    /// The movie recordings of `out.video` nodes.
    pub record: RecordHub,
    /// The structured light scans of `tex.scan` nodes (`scan.run`).
    pub scans: Scans,
    /// The 3D mouse; its reader starts when the UI first asks for it.
    pub spacenav: SpaceNav,
    /// The keys the windows (and `keyboard.press`) pass on, for `chan.keyboard_in`.
    pub keyboard: Keyboard,
}

impl Hubs {
    pub fn new() -> Self {
        let keyboard = Keyboard::new();
        Hubs {
            osc: Arc::new(OscShared::new(penumbra_osc::Hub::new(), keyboard.clone())),
            audio: AudioHub::new(),
            dmx: DmxHub::new(),
            dmx_in: DmxInputs::new(),
            camera: CameraHub::new(),
            record: RecordHub::new(),
            scans: Scans::new(),
            spacenav: SpaceNav::new(),
            keyboard,
        }
    }

    /// Every hub, in the order they shut down. Recordings come first: when the app closes, the
    /// cook thread has dropped its nodes and every recording is finishing its file, which the
    /// shutdown waits for.
    pub fn all(&self) -> [&dyn DeviceHub; 9] {
        // Destructured so that a field missing here does not compile.
        let Hubs { osc, audio, dmx, dmx_in, camera, record, scans, spacenav, keyboard } = self;
        [record, osc.as_ref(), audio, dmx, dmx_in, camera, scans, spacenav, keyboard]
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
    /// The 3D mouse is the UI's, not a resource of the nodes.
    fn provide(&self, _cooker: &mut Cooker) {}

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
        assert!(cooker.resource::<AudioHub>().is_some());
        assert!(cooker.resource::<DmxHub>().is_some());
        assert!(cooker.resource::<DmxInputs>().is_some());
        assert!(cooker.resource::<CameraHub>().is_some());
        assert!(cooker.resource::<RecordHub>().is_some());
        assert!(cooker.resource::<Scans>().is_some());
        assert!(cooker.resource::<Keyboard>().is_some());
        assert!(cooker.resource::<SpaceNav>().is_none());
        for hub in hubs.all() {
            hub.shutdown();
        }
    }
}
