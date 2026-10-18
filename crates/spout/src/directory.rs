//! Spout's directory of senders: registering one, listing them, reading one's description.
//! The maps and locks are [`crate::d3d`]'s; the bytes are [`crate::protocol`]'s.

use crate::d3d::{Map, NamedMutex, Semaphore, setting};
use crate::protocol::{
    DEFAULT_MAX_SENDERS, INFO_LEN, Info, NAME_LEN, check_name, free_name, read_names, text, write_names,
};

const NAMES: &str = "SpoutSenderNames";
const ACTIVE: &str = "ActiveSenderName";

/// Most senders the list may hold, from Spout's registry setting.
fn max_senders() -> usize {
    setting("MaxSenders").map_or(DEFAULT_MAX_SENDERS, |v| (v as usize).clamp(1, 4096))
}

/// A sender as the directory lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    pub name: String,
    pub info: Info,
}

/// The senders registered now, sorted by name. Names whose description is gone (a program
/// that ended without cleaning up) are left out.
pub fn senders() -> Vec<Listing> {
    let Some(map) = Map::open(NAMES) else { return Vec::new() };
    let names = map.with(|b| read_names(b, b.len() / NAME_LEN)).unwrap_or_default();
    names.into_iter().filter_map(|name| info(&name).map(|info| Listing { name, info })).collect()
}

/// The sender a receiver without a name takes: the one the user picked last in a Spout
/// program, else the newest. `None` without one.
pub fn active() -> Option<String> {
    let map = Map::open(ACTIVE)?;
    let name = map.with(|b| text(b.get(..NAME_LEN).unwrap_or(b)))?;
    (!name.is_empty()).then_some(name)
}

/// The description of sender `name`, `None` if there is no such sender.
pub fn info(name: &str) -> Option<Info> {
    if name.is_empty() {
        return None;
    }
    let map = Map::open(name)?;
    map.with(|b| Info::from_bytes(b))?
}

/// The active sender if it is still there, else the first one listed.
pub fn default_sender() -> Option<String> {
    active().filter(|n| info(n).is_some()).or_else(|| senders().into_iter().next().map(|l| l.name))
}

/// A sender of this process in the directory, removed when dropped.
pub struct Registration {
    name: String,
    names: Map,
    info: Map,
    /// Kept so the active-sender map lives as long as this sender (maps vanish with their last
    /// handle).
    active: Option<Map>,
    access: NamedMutex,
    frames: Semaphore,
}

impl Registration {
    /// Registers a sender named `base`, or `base_1`, ... if another program has that name, and
    /// makes it the active sender (as Spout's senders do).
    pub fn new(base: &str, info: &Info) -> Result<Self, String> {
        check_name(base)?;
        let max = max_senders();
        let (names, _) = Map::open_or_create(NAMES, max * NAME_LEN)?;
        let name = names
            .with(|b| {
                let slots = (b.len() / NAME_LEN).min(max);
                let mut list = read_names(b, slots);
                // Names of programs that ended without removing theirs.
                list.retain(|n| Map::open(n).is_some());
                if list.len() >= slots {
                    return Err(format!("Spout's list of senders is full ({slots})"));
                }
                let name = free_name(base, &list);
                list.insert(name.clone());
                write_names(&list, b, slots);
                Ok(name)
            })
            .ok_or("Spout's list of senders is locked by another program")??;
        let registered = Self::complete(name.clone(), names, info);
        if registered.is_err() {
            // Not left in the list without a description.
            if let Ok((names, _)) = Map::open_or_create(NAMES, max * NAME_LEN) {
                remove(&names, &name);
            }
        }
        registered
    }

    fn complete(name: String, names: Map, info: &Info) -> Result<Self, String> {
        let (info_map, _) = Map::open_or_create(&name, INFO_LEN)?;
        info_map
            .with(|b| {
                if let Some(dst) = b.get_mut(..INFO_LEN) {
                    dst.copy_from_slice(&info.to_bytes());
                }
            })
            .ok_or("the sender's description is locked")?;
        let access = NamedMutex::open_or_create(&format!("{name}_SpoutAccessMutex"))?;
        let frames = Semaphore::open_or_create(&format!("{name}_Count_Semaphore"))?;
        let active = set_active(&name);
        Ok(Self { name, names, info: info_map, active, access, frames })
    }

    /// The name as registered (with a number if the one asked for was taken).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Writes a new description (a new texture: size, format or handle changed).
    pub fn update(&self, info: &Info) -> Result<(), String> {
        self.info
            .with(|b| {
                if let Some(dst) = b.get_mut(..INFO_LEN) {
                    dst.copy_from_slice(&info.to_bytes());
                }
            })
            .ok_or_else(|| "the sender's description is locked".to_string())
    }

    /// The texture's lock, which receivers take while they copy.
    pub fn access(&self) -> &NamedMutex {
        &self.access
    }

    /// Counts a frame sent.
    pub fn frame_sent(&self) {
        self.frames.increment();
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        remove(&self.names, &self.name);
        self.active = None;
    }
}

/// Makes `name` the active sender; the map, which must stay open for it to last.
fn set_active(name: &str) -> Option<Map> {
    let (map, _) = Map::open_or_create(ACTIVE, NAME_LEN).ok()?;
    map.with(|b| {
        b.fill(0);
        if let Some(dst) = b.get_mut(..name.len()) {
            dst.copy_from_slice(name.as_bytes());
        }
    })?;
    Some(map)
}

/// Takes `name` off the list; if it was the active sender, the first one left becomes active.
fn remove(names: &Map, name: &str) {
    let left = names.with(|b| {
        let slots = b.len() / NAME_LEN;
        let mut list = read_names(b, slots);
        list.remove(name);
        write_names(&list, b, slots);
        list
    });
    if let Some(left) = left
        && active().as_deref() == Some(name)
        && let Some(first) = left.first()
        && let Some(map) = Map::open(ACTIVE)
    {
        map.with(|b| {
            b.fill(0);
            if let Some(dst) = b.get_mut(..first.len()) {
                dst.copy_from_slice(first.as_bytes());
            }
        });
    }
}
