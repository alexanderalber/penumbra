//! The project as text: the RON file format, the path-keyed content and the diff that loading, hot
//! reload and merging external edits share, the editing modes (user or Claude), and the session
//! that ties a document to its file with autosave.
//!
//! No GPU, no UI.

pub mod clipboard;
pub mod component;
pub mod content;
pub mod file;
pub mod gate;
pub mod session;

#[cfg(test)]
mod testing;

pub use content::{Content, NodeContent, PortRef, diff};
pub use file::{BrokenNode, FILE_NAME, FORMAT_VERSION, load, parse, print};
pub use gate::{Gate, GateStatus};
pub use session::{AUTOSAVE_DELAY, Actor, BACKUP_NAME, Session, SessionStatus};
