//! The node graph, headless: node types and their declarations, the network as immutable
//! snapshots, batched edits, linear history, the document that publishes snapshots, the cook
//! scheduler with dirty tracking, and asynchronous jobs.
//!
//! No GPU, no UI. The data flowing between nodes is type-erased ([`Data`]); the crates that
//! implement the families (textures in `gpu`, channels and tables in `ops`) give it meaning.

pub mod broken;
pub mod cook;
pub mod document;
pub mod history;
pub mod job;
pub mod network;
pub mod node;
pub mod replace;
pub mod watch;

#[cfg(test)]
mod testing;

pub use cook::{
    CookContext, CookReport, CookStats, Cooker, InputChoice, Kept, NodeStatus, ResetOutcome, ThumbnailMode,
    ThumbnailPolicy, Waker,
};
pub use document::{Document, EditOutcome};
pub use history::{Entry, History, Recorded, Source};
pub use job::{AsyncSlot, Job, JobContext, JobPool, JobStatus};
pub use network::{
    Applied, EXPOSE, Exposed, Network, NodeDef, Op, PERSIST_STATE, Params, Port, SETTINGS, SettingSpec,
    check_param, check_setting, natural_cmp, parse_expose,
};
pub use node::{
    CookError, Cooked, Data, DataKind, Density, FnNodeType, Inactive, InputChooser, NodeDesc, NodeType,
    NodeTypes, Operator, PortRole, StateItem, WarmUp,
};
pub use replace::{Replacement, replace};
pub use watch::FileWatch;
