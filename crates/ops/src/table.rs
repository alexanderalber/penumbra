//! `table` nodes: rows of text cells on the CPU. `table.text` holds its text in a parameter,
//! `table.file` reads a file off the cook thread and reloads it when it changes on disk.
//! `chan.from_table` turns a table of numbers into channels, so cells can drive params.

use penumbra_core::{Channel, Chans, Error, Name, ParamSpec, SampleAxis, Table, table::Format};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDesc,
    NodeTypes, Operator,
};

use crate::{Null, waiting};

/// Largest file `table.file` reads.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Extensions the file picker offers; any text file reads.
const EXTENSIONS: &[&str] = &["csv", "tsv", "tab", "txt"];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            "table.text",
            "A table typed into the node: one row per line, cells split by tab or comma",
        )
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
        NodeDesc::new("table.file", "A table from a text, TSV or CSV file; updates when the file changes")
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
        NodeDesc::new(
            "table.null",
            "Passes its input on unchanged: a fixed point to refer to while the chain before it changes",
        )
        .input("in", DataKind::Table)
        .output("out", DataKind::Table),
        || Box::new(Null) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            FROM_TABLE,
            "Turns a table of numbers into channels, so its cells can drive params through bindings",
        )
        .param(
            ParamSpec::choice("layout", &["columns", "rows"])
                .doc("Which cells make up one channel")
                .choice_doc("columns", "each column is a channel, its samples read top to bottom")
                .choice_doc("rows", "each row is a channel, its samples read left to right"),
        )
        .param(ParamSpec::bool("names", true).doc(
            "On, the first cell of each column (or row) is the channel's name; off, the channels              are called chan0, chan1, ... and every cell is a value",
        ))
        .input("in", DataKind::Table)
        .output("out", DataKind::Chan),
        || Box::new(FromTable) as Box<dyn Operator>,
    ))?;
    Ok(())
}

/// The kind of the node that turns a table into channels.
pub const FROM_TABLE: &str = "chan.from_table";

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

/// `chan.from_table`: a channel per column (or row). The last value is the one a binding reads.
/// Empty cells and missing cells of a short row are 0; text that is no number is 0 too, with a
/// warning. Columns (or rows) without a single filled cell are left out, so a trailing comma
/// makes no channel.
struct FromTable;

impl Operator for FromTable {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let table = cx.input_as::<Table>(0).ok_or_else(|| waiting(cx))?;
        let by_rows = cx.text("layout")? == "rows";
        let named = cx.bool("names")?;
        let rate = cx.rate().as_f64();
        let (lines, cells) =
            if by_rows { (table.num_rows(), table.num_cols()) } else { (table.num_cols(), table.num_rows()) };
        // Cell `k` of channel `i`, with its 1-based row and column for messages.
        let cell = |i: usize, k: usize| {
            let (row, col) = if by_rows { (i, k) } else { (k, i) };
            (table.cell(row, col).trim(), row + 1, col + 1)
        };
        let first = usize::from(named);
        let len = cells.saturating_sub(first).max(1);
        let mut channels: Vec<Channel> = Vec::new();
        let mut not_numbers: Vec<String> = Vec::new();
        let mut twice: Vec<String> = Vec::new();
        for i in 0..lines {
            if (0..cells).all(|k| cell(i, k).0.is_empty()) {
                continue;
            }
            let name = if named { channel_name(cell(i, 0).0) } else { None };
            let name = match name {
                Some(n) => n,
                None => Name::new(&format!("chan{i}"))?,
            };
            if channels.iter().any(|c| c.name == name) {
                twice.push(name.as_str().to_string());
                continue;
            }
            let mut samples = vec![0.0; len];
            for (k, s) in (first..cells).zip(samples.iter_mut()) {
                let (text, row, col) = cell(i, k);
                if text.is_empty() {
                    continue;
                }
                match text.parse::<f64>() {
                    Ok(v) => *s = v,
                    Err(_) => not_numbers.push(format!("`{}` (row {row}, column {col})", short(text))),
                }
            }
            channels.push(Channel { name, samples });
        }
        let mut warnings = Vec::new();
        if named && cells < 2 && !channels.is_empty() {
            warnings.push("there are only names, no values: every channel is 0".to_string());
        }
        if let Some(t) = not_numbers.first() {
            let more = match not_numbers.len() {
                1 => String::new(),
                n => format!(" and {} more cells", n - 1),
            };
            warnings.push(format!("{t}{more} hold no number and count as 0"));
        }
        if !twice.is_empty() {
            warnings.push(format!("{} named twice, the first is kept", twice.join(", ")));
        }
        if !warnings.is_empty() {
            cx.warn(warnings.join("; "));
        }
        let axis = if len > 1 { SampleAxis::Index } else { SampleAxis::Time };
        let chans = Chans::new(rate, channels)?.with_axis(axis);
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, chans)]))
    }
}

/// A channel name made from a header cell: characters a name cannot hold become `_`, a leading
/// digit gets a `_` before it, the rest is cut at the longest name. `None` for an empty cell.
fn channel_name(text: &str) -> Option<Name> {
    if text.is_empty() {
        return None;
    }
    let mut name: String =
        text.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        name.insert(0, '_');
    }
    name.truncate(Name::MAX_LEN);
    Name::new(&name).ok()
}

/// At most 40 characters of a cell, for a message.
fn short(text: &str) -> String {
    match text.char_indices().nth(40) {
        Some((i, _)) => format!("{}...", &text[..i]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use crate::builtin_types;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use penumbra_core::{Chans, NodePath, ParamPath, Rate, SampleAxis, ShowClock, Table, Value};
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

        let dir = penumbra_core::scratch::ScratchDir::new("table").unwrap();
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
    }

    /// Cooks `/c` (a `chan.from_table` fed by a `table.text` at `/t`) and returns its channels
    /// and warning.
    fn from_table(text: &str, ops: &[Op]) -> (Chans, Option<String>) {
        let types = Arc::new(builtin_types().unwrap());
        let mut all = vec![
            create("/t", "table.text"),
            set("/t.text", text),
            set("/t.format", "csv"),
            create("/c", "chan.from_table"),
            Op::Connect { from: "/t".parse().unwrap(), output: 0, to: "/c".parse().unwrap(), input: 0 },
        ];
        all.extend_from_slice(ops);
        let net = Arc::new(Network::new().apply(&types, &all).unwrap().network);
        let mut cooker = Cooker::new(types, Arc::new(JobPool::new(1).unwrap()));
        let id = net.resolve(&"/c".parse().unwrap()).unwrap();
        let clock = ShowClock::new(Rate::FPS_60, Default::default());
        cooker.cook(&net, clock.now(), &[id], &[]);
        let status = cooker.status(id).unwrap();
        assert!(status.error.is_none(), "{:?}", status.error);
        let chans = cooker.output(Port { node: id, output: 0 }).unwrap().get::<Chans>().unwrap().clone();
        (chans, status.warning)
    }

    fn names(c: &Chans) -> Vec<&str> {
        c.names().map(|n| n.as_str()).collect()
    }

    #[test]
    fn chans_from_table() {
        // Columns, named by the first row; the last row is the value now.
        let (c, warning) = from_table("speed,size\n1,2\n3,4.5", &[]);
        assert_eq!(names(&c), ["speed", "size"]);
        assert_eq!(c.get("speed").unwrap().samples, [1.0, 3.0]);
        assert_eq!(c.value(Some("size")), Some(4.5));
        assert_eq!(c.axis(), SampleAxis::Index);
        assert_eq!(warning, None);

        // Rows, unnamed.
        let (c, _) = from_table("1,2\n3", &[set("/c.layout", "rows"), set("/c.names", false)]);
        assert_eq!(names(&c), ["chan0", "chan1"]);
        assert_eq!(c.get("chan1").unwrap().samples, [3.0, 0.0], "a short row ends in zeros");

        // Header cells become names; text is 0 with a warning; a trailing comma makes no channel;
        // a name twice keeps the first.
        let (c, warning) = from_table("pos x,2nd,a,a,\n1,abc,2,3,\n", &[]);
        assert_eq!(names(&c), ["pos_x", "_2nd", "a"]);
        assert_eq!(c.value(Some("_2nd")), Some(0.0));
        assert_eq!(c.value(Some("a")), Some(2.0));
        assert_eq!(c.axis(), SampleAxis::Time, "one value per channel is a value now");
        let warning = warning.unwrap();
        assert!(warning.contains("`abc` (row 2, column 2)"), "{warning}");
        assert!(warning.contains("a named twice"), "{warning}");

        // Only names: channels at 0, said so. An empty table (nothing typed yet): no channels.
        let (c, warning) = from_table("x,y", &[]);
        assert_eq!(names(&c), ["x", "y"]);
        assert_eq!(c.value(Some("y")), Some(0.0));
        assert!(warning.unwrap().contains("only names"));
        let (c, warning) = from_table("", &[]);
        assert!(c.is_empty());
        assert_eq!(warning, None);
    }
}
