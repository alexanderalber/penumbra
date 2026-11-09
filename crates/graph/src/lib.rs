//! The node graph, headless: node types and their declarations, the network as immutable
//! snapshots, batched edits, linear history, the document that publishes snapshots, the cook
//! scheduler with dirty tracking, and asynchronous jobs.
//!
//! No GPU, no UI. The data flowing between nodes is type-erased ([`Data`]); the crates that
//! implement the families (textures in `gpu`, channels and tables in `ops`) give it meaning.

mod bound;
pub mod broken;
pub mod cook;
pub mod declared;
pub mod document;
pub mod follow;
pub mod history;
pub mod job;
pub mod network;
pub mod node;
pub mod rename;
pub mod replace;
pub mod watch;

#[cfg(test)]
mod testing;

pub use cook::{
    CookContext, CookReport, CookStats, Cooker, InputChoice, Kept, NodeSpent, NodeStatus, PassSpent, Profile,
    ResetOutcome, ThumbnailMode, ThumbnailPolicy, Waker,
};
pub use declared::{Exposed, PARAMS, Planned, Put, RESET, declared_in, declares, exposed_of, spec_in};
pub use document::{Document, EditOutcome};
pub use history::{Entry, History, Recorded, Source};
pub use job::{AsyncSlot, Job, JobContext, JobPool, JobStatus};
pub use network::{
    Applied, Bind, MAX_POS, MAX_THUMBNAIL_EVERY, Network, NodeDef, Op, PERSIST_STATE, Params, Port, SETTINGS,
    SettingKind, SettingSpec, THUMBNAIL_EVERY, THUMBNAILS, check_bind_read, check_param, check_pos,
    check_setting, natural_cmp,
};
pub use node::{
    Block, CookError, Cooked, Data, DataKind, Density, FnNodeType, Inactive, InputChooser, InputRun,
    ListedInput, MAX_RUN, NodeDesc, NodeType, NodeTypes, Operator, PortRole, Search, StateItem, WarmUp,
    name_found, reads, search_form,
};
pub use rename::rename;
pub use replace::{Replacement, replace};
pub use watch::FileWatch;
