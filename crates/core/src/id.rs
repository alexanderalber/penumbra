//! Ids.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A node's identity inside one network. Never reused within a network and stable across
/// renames and edits, so caches, thumbnails and history entries can refer to it. The project
/// file refers to nodes by path instead; ids are runtime identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub u64);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Groups history entries: every edit carrying the same group id as the entry on top of the
/// history merges into it (one parameter drag, one action by Claude spanning several commands).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GroupId(pub u64);

impl fmt::Display for GroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "g{}", self.0)
    }
}
