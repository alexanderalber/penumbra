//! Mesh files in the engine: `out.mesh` nodes write through
//! [`MeshOuts`](penumbra_ops::out::mesh::MeshOuts), a resource of the cooker.
//!
//! Commands: `mesh_out.write`, `mesh_out.status`, as for table files ([`crate::file_out`]).

use std::sync::Arc;

use penumbra_ops::out::mesh::{OUT_MESH, Status};
use serde_json::Value as Json;

use crate::command::{CommandError, Registry};
use crate::commands::engine_commands;
use crate::engine::Engine;
use crate::file_out::{self, Kind};

const KIND: Kind = Kind { node: OUT_MESH, status: "mesh_out.status", list: "meshes" };

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "mesh_out.write",
        "Write an out.mesh's input to its file now, in either mode; {path}; answers with the status once written, fails with the node's error",
        write,
    );
    add(
        "mesh_out.status",
        "Mesh files: node, file, format, mode, written_seconds_ago, triangles, parts, copies, bytes, writing, error; {path?}",
        status,
    );
}

fn write(e: &Engine, p: &Json) -> Result<Json, CommandError> {
    file_out::write::<Status>(e, p, &KIND)
}

fn status(e: &Engine, p: &Json) -> Result<Json, CommandError> {
    file_out::status::<Status>(e, p, &KIND)
}
