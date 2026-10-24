//! Palette commands: `palette.list` (the built-in set and the project's own), `palette.set` and
//! `palette.remove` (the project's own, in [`palette::FILE`] beside `project.ron`). Nodes reread
//! the file when it changes, so a palette edited here shows at once wherever it is used.

use std::path::PathBuf;
use std::sync::Arc;

use penumbra_core::palette::{self, Palette};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry, args};
use crate::commands::{engine_commands, no_args};
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "palette.list",
        "Every palette a palette param can name, in list order (the place a bound channel picks): \
         the built-in set, then the project's own from palettes.txt; each with kind, source and hex colours",
        palette_list,
    );
    add(
        "palette.set",
        "Define or change one of the project's own palettes: {name, colours: \"31aaa9 f8e0a4 ...\" or [\"#31aaa9\", ...]}; \
         written to palettes.txt beside project.ron, used at once by every node naming it",
        palette_set,
    );
    add(
        "palette.remove",
        "Remove one of the project's own palettes from palettes.txt: {name}",
        palette_remove,
    );
}

fn file(e: &Engine) -> PathBuf {
    e.session().dir().join(palette::FILE)
}

fn describe(p: &Palette) -> Json {
    json!({"name": p.name, "kind": p.kind.key(), "source": p.source.key(), "colours": p.hex()})
}

fn palette_list(e: &Engine, p: &Json) -> CmdResult<Json> {
    no_args(p)?;
    let path = file(e);
    let (project, notes) = penumbra_ops::palette::load(&path);
    let all: Vec<Json> = palette::all(&project).iter().map(describe).collect();
    Ok(json!({"palettes": all, "file": path.display().to_string(), "notes": notes}))
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Colours {
    Text(String),
    List(Vec<String>),
}

/// The project's file as text, empty when there is none yet; refused when too large to be one.
fn read(path: &std::path::Path) -> CmdResult<String> {
    match std::fs::metadata(path) {
        Ok(m) if m.len() > palette::MAX_FILE_BYTES => {
            return Err(format!(
                "{} is larger than {} KB; fix it by hand",
                path.display(),
                palette::MAX_FILE_BYTES / 1024
            )
            .into());
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(format!("{}: {e}", path.display()).into()),
    }
    std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .map_err(|e| format!("{}: {e}", path.display()).into())
}

fn write(path: &std::path::Path, text: &str) -> CmdResult<()> {
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()).into())
}

fn check_name(name: &str) -> CmdResult<()> {
    if !palette::is_name(name) {
        return Err(format!(
            "`{name}` is no palette name: a letter, then letters, digits, _ or -, at most {} characters",
            palette::MAX_NAME
        )
        .into());
    }
    if palette::is_builtin(name) {
        return Err(format!("`{name}` is a built-in palette; give yours another name").into());
    }
    Ok(())
}

fn palette_set(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        colours: Colours,
    }
    let a: A = args(p)?;
    check_name(&a.name)?;
    let text = match a.colours {
        Colours::Text(t) => t,
        Colours::List(l) => l.join(" "),
    };
    let colours = palette::colours(&text).map_err(|e| format!("`colours`: {e}"))?;
    let path = file(e);
    let before = read(&path)?;
    let (project, _) = palette::parse_file(&before);
    if project.len() >= palette::MAX_PALETTES && !project.iter().any(|q| q.name.eq_ignore_ascii_case(&a.name))
    {
        return Err(format!("the project has {} palettes already", palette::MAX_PALETTES).into());
    }
    let created = !project.iter().any(|q| q.name.eq_ignore_ascii_case(&a.name));
    write(&path, &palette::edit_file(&before, &a.name, Some(&colours)))?;
    let set =
        Palette { name: a.name, kind: palette::Kind::Palette, source: palette::Source::Project, colours };
    Ok(json!({"palette": describe(&set), "created": created, "file": path.display().to_string()}))
}

fn palette_remove(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
    }
    let a: A = args(p)?;
    let path = file(e);
    let before = read(&path)?;
    let (project, _) = palette::parse_file(&before);
    if !project.iter().any(|q| q.name.eq_ignore_ascii_case(&a.name)) {
        let names: Vec<&str> = project.iter().map(|q| q.name.as_str()).collect();
        return Err(if palette::is_builtin(&a.name) {
            format!("`{}` is built in; only the project's own can be removed", a.name)
        } else if names.is_empty() {
            format!("the project defines no palette `{}` (it has none of its own)", a.name)
        } else {
            format!("the project defines no palette `{}`; its own: {}", a.name, names.join(", "))
        }
        .into());
    }
    write(&path, &palette::edit_file(&before, &a.name, None))?;
    Ok(json!({"removed": a.name, "file": path.display().to_string()}))
}
