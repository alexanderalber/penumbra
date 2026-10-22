//! `table` nodes: rows of text cells on the CPU. `table.text` holds its text in a parameter,
//! `table.file` reads a file off the cook thread and reloads it when it changes on disk.

use penumbra_core::{Error, ParamSpec, Table, table::Format};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDesc,
    NodeTypes, Operator,
};

use crate::Null;

/// Largest file `table.file` reads.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Extensions the file picker offers; any text file reads.
const EXTENSIONS: &[&str] = &["csv", "tsv", "tab", "txt"];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new("table.text", "A table typed in: one row per line, cells by tab or comma")
            .param(
                ParamSpec::text("text", "")
                    .multiline()
                    .doc("The table as text: one row per line, cells split as format says"),
            )
            .param(
                ParamSpec::choice("format", &Format::NAMES)
                    .doc("How a line is split into cells")
                    .choice_doc("lines", "not at all: one cell per row")
                    .choice_doc("tsv", "at tabs")
                    .choice_doc("csv", "at commas; \"a, b\" in quotes is one cell, \"\" a quote"),
            )
            .output("out", DataKind::Table),
        || Box::new(Text) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("table.file", "A text, TSV or CSV file; reloads when the file changes")
            .param(
                ParamSpec::file("file", EXTENSIONS).doc(
                    "The text file, relative to the project directory; saving it again reloads the node",
                ),
            )
            .param(
                ParamSpec::choice("format", &["auto", "lines", "tsv", "csv"])
                    .doc("How a line of the file is split into cells")
                    .choice_doc("auto", "by extension: csv, tsv (or tab), else lines")
                    .choice_doc("lines", "not at all: one cell per row")
                    .choice_doc("tsv", "at tabs")
                    .choice_doc("csv", "at commas; \"a, b\" in quotes is one cell, \"\" a quote"),
            )
            .output("out", DataKind::Table),
        || Box::new(File::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("table.null", "The input, unchanged: a fixed point to refer to")
            .input("in", DataKind::Table)
            .output("out", DataKind::Table),
        || Box::new(Null) as Box<dyn Operator>,
    ))?;
    Ok(())
}

fn format(name: &str) -> Result<Format, CookError> {
    Format::from_name(name).ok_or_else(|| CookError::failed(format!("unknown format `{name}`")))
}

fn out(table: Table) -> Result<Cooked, CookError> {
    Ok(Cooked::Outputs(vec![Data::new(DataKind::Table, table)]))
}

struct Text;

impl Operator for Text {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        out(Table::parse(cx.text("text")?, format(cx.text("format")?)?)?)
    }
}

#[derive(Default)]
struct File {
    slot: AsyncSlot<Table>,
    watch: Option<FileWatch>,
    /// The format the running or last job used, so a format change re-reads the file.
    format: Option<Format>,
}

impl Operator for File {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let fresh = self.slot.poll();
        let path = cx.resolve_path(cx.text("file")?)?;
        let fmt = match cx.text("format")? {
            "auto" => Format::from_extension(path.extension().and_then(|e| e.to_str()).unwrap_or("")),
            name => format(name)?,
        };
        let moved = self.watch.as_ref().is_none_or(|w| w.path() != path);
        let changed = self.watch.as_ref().is_some_and(FileWatch::take_changed);
        if moved || changed || self.format != Some(fmt) {
            if moved {
                self.watch = Some(cx.watch_file(&path));
            }
            self.format = Some(fmt);
            let p = path.clone();
            self.slot.start(cx.spawn(move |_job| {
                let name = p.display();
                let len = std::fs::metadata(&p).map_err(|e| format!("{name}: {e}"))?.len();
                if len > MAX_FILE_BYTES {
                    return Err(format!("{name}: {len} bytes, a table file may have {MAX_FILE_BYTES}"));
                }
                let bytes = std::fs::read(&p).map_err(|e| format!("{name}: {e}"))?;
                let text = String::from_utf8_lossy(&bytes);
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
                Table::parse(text, fmt).map_err(|e| format!("{name}: {e}"))
            }));
        }
        if let Some(e) = self.slot.error() {
            return Err(CookError::failed(e));
        }
        match (fresh, self.slot.latest()) {
            (true, Some(t)) => out(t.clone()),
            (_, None) => Err(CookError::Pending),
            (false, Some(_)) => Ok(Cooked::Unchanged),
        }
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.slot.status())
    }
}

#[cfg(test)]
mod tests {
    use crate::builtin_types;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use penumbra_core::{NodePath, ParamPath, Rate, ShowClock, Table, Value};
    use penumbra_graph::{Cooker, JobPool, Network, Op, Port};

    fn create(path: &str, kind: &str) -> Op {
        Op::Create { path: NodePath::parse(path).unwrap(), kind: kind.into(), params: Default::default() }
    }

    fn set(p: &str, v: impl Into<Value>) -> Op {
        Op::Set { param: ParamPath::parse(p).unwrap(), value: v.into() }
    }

    /// Cooks `/t` until it has a result or an error (jobs finish off the cook thread).
    fn cook(net: &Arc<Network>, cooker: &mut Cooker) -> Result<Table, String> {
        let id = net.resolve(&"/t".parse().unwrap()).unwrap();
        let clock = ShowClock::new(Rate::FPS_60, Default::default());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            cooker.cook(net, clock.now(), &[id], &[]);
            let status = cooker.status(id).unwrap();
            if let Some(e) = status.error {
                return Err(e);
            }
            if let Some(t) = cooker.output(Port { node: id, output: 0 }).and_then(|d| d.get::<Table>()) {
                return Ok(t.clone());
            }
            assert!(Instant::now() < deadline, "no result");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn text_and_file() {
        let types = Arc::new(builtin_types().unwrap());
        let net = Network::new()
            .apply(&types, &[create("/t", "table.text"), set("/t.text", "a,b\nc"), set("/t.format", "csv")])
            .unwrap()
            .network;
        let mut cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(1).unwrap()));
        let t = cook(&Arc::new(net), &mut cooker).unwrap();
        assert_eq!(t.rows(), [vec!["a".to_string(), "b".into()], vec!["c".into()]]);

        let dir = std::env::temp_dir().join(format!("penumbra-table-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("wall.tsv");
        std::fs::write(&file, "\u{feff}name\tmessage\nalex\thi\n").unwrap();
        let file_s = file.to_string_lossy().to_string();
        let net = Arc::new(
            Network::new()
                .apply(&types, &[create("/t", "table.file"), set("/t.file", file_s.as_str())])
                .unwrap()
                .network,
        );
        let mut cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(1).unwrap()));
        let t = cook(&net, &mut cooker).unwrap();
        assert_eq!(t.cell(0, 0), "name", "the BOM is dropped");
        assert_eq!(t.cell(1, 1), "hi");
        // Lines on request.
        let net = Arc::new(net.apply(&types, &[set("/t.format", "lines")]).unwrap().network);
        let deadline = Instant::now() + Duration::from_secs(5);
        while cook(&net, &mut cooker).unwrap().num_cols() != 1 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        // A null passes the table on.
        let net = Network::new()
            .apply(
                &types,
                &[
                    create("/a", "table.text"),
                    set("/a.text", "x"),
                    create("/t", "table.null"),
                    Op::Connect {
                        from: "/a".parse().unwrap(),
                        output: 0,
                        to: "/t".parse().unwrap(),
                        input: 0,
                    },
                ],
            )
            .unwrap()
            .network;
        let mut null_cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(1).unwrap()));
        assert_eq!(cook(&Arc::new(net), &mut null_cooker).unwrap().cell(0, 0), "x");
        let missing = dir.join("nope.csv").to_string_lossy().to_string();

        let net = Arc::new(
            Network::new()
                .apply(&types, &[create("/t", "table.file"), set("/t.file", missing.as_str())])
                .unwrap()
                .network,
        );
        let mut cooker = Cooker::new(types, Arc::new(JobPool::new(1).unwrap()));
        assert!(cook(&net, &mut cooker).unwrap_err().contains("nope.csv"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
