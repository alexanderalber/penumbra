//! Table files in the engine: `out.table` nodes write through
//! [`TableOuts`](penumbra_ops::out::table::TableOuts), a resource of the cooker.
//!
//! Commands: `table_out.write`, `table_out.status`. Writing is a request to the node: the node
//! writes at its next cook, on a job, and the command waits for the answer ([`crate::file_out`]).

use std::sync::Arc;

use penumbra_ops::out::table::{OUT_TABLE, Status};
use serde_json::Value as Json;

use crate::command::{CommandError, Registry};
use crate::commands::engine_commands;
use crate::engine::Engine;
use crate::file_out::{self, Kind};

const KIND: Kind = Kind { node: OUT_TABLE, status: "table_out.status", list: "tables" };

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "table_out.write",
        "Write an out.table's input to its file now, in either mode; {path}; answers with the status once written, fails with the node's error",
        write,
    );
    add(
        "table_out.status",
        "Table files: node, file, mode, written_seconds_ago, rows, cols, bytes, writing, error; {path?}",
        status,
    );
}

fn write(e: &Engine, p: &Json) -> Result<Json, CommandError> {
    file_out::write::<Status>(e, p, &KIND)
}

fn status(e: &Engine, p: &Json) -> Result<Json, CommandError> {
    file_out::status::<Status>(e, p, &KIND)
}
