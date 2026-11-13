//! `table` nodes: rows of text cells on the CPU. `table.text` holds its text in a parameter,
//! `table.file` reads a file off the cook thread and reloads it when it changes on disk.
//! `chan.from_table` turns a table of numbers into channels, so cells can drive params. Its
//! `key` picks one row by the text in its first cell; bound to a choice, each entry of the
//! choice reads its own row of numbers in the same frame, with no GPU and no readback.
//! `table.from_chan` goes the other way: channels become cells, so numbers that change every
//! frame can place things a table places.

use std::collections::BTreeSet;

use penumbra_core::chan::MAX_CHANNELS;
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
            "Turns a table of numbers into channels, so its cells can drive params through bindings. `key` picks one row by its first cell, such as the name of a look",
        )
        .param(
            ParamSpec::choice("layout", &["columns", "rows"])
                .doc("Which cells make up one channel")
                .choice_doc("columns", "each column is a channel, its samples read top to bottom")
                .choice_doc("rows", "each row is a channel, its samples read left to right"),
        )
        .param(ParamSpec::bool("names", true).doc(
            "On, the first cell of each column (or row) is the channel's name; off, the channels are called chan0, chan1, ... and every cell is a value. On, a column of words alone (a \
             label per row) makes no channel",
        ))
        .param(ParamSpec::int("row", -1).clamp(-1.0, 1e9).range(-1.0, 20.0).special(-1.0, "all").doc(
            "Only this row, counted from 0 under the names: one value per channel. Bind it to the \
             step of a chan.sequence, and each cue reads its own row of a cue sheet. Past the last \
             row: the last. With layout rows: this column. -1: all",
        ))
        .param(ParamSpec::text("key", "").doc(
            "Only the row whose first cell is this text, case and spaces aside: one value per \
             channel. Bind it to a choice, such as parent.look inside a component, and each entry \
             reads its own row in the same frame. A key that no row has gives the first row and a \
             warning. Empty: row decides. With layout rows: the column whose top cell is the key",
        ))
        .input("in", DataKind::Table)
        .output("out", DataKind::Chan),
        || Box::new(FromTable) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            FROM_CHAN,
            "Turns channels into a table of numbers, so channels can place what a table places: the bodies of a physics.bodies, the copies of a geo.instance",
        )
        .param(
            ParamSpec::choice("layout", &["columns", "rows", "numbered"])
                .doc("Which cells one channel fills")
                .choice_doc("columns", "each channel is a column, its samples from top to bottom: channels with one value each give one row")
                .choice_doc("rows", "each channel is a row, its samples from left to right")
                .choice_doc("numbered", "the number at the end of a channel's name picks its row: x1, y1, x2, y2 give the columns x and y with a row for 1 and a row for 2. A channel without a number fills its column in every row. This is the layout for the slots of a tracker"),
        )
        .param(ParamSpec::bool("names", true).doc(
            "On, the first cell of each column (or row) is the channel's name, as most nodes that read a table expect; off, every cell is a value",
        ))
        .param(
            ParamSpec::text("keep", "")
                .applies_when("layout", &["numbered"])
                .doc("The name of a column. Rows with 0 in it are left out, and the rows after them move up: `id` leaves out the slots of a tex.blob_track that hold no blob. Empty: every row stays"),
        )
        .input("in", DataKind::Chan)
        .output("out", DataKind::Table)
        .describe_output("out", "The channels' values as cells, written so that they read back as the same numbers"),
        || Box::new(FromChan) as Box<dyn Operator>,
    ))?;
    Ok(())
}

/// The kind of the node that turns a table into channels.
pub const FROM_TABLE: &str = "chan.from_table";

/// The kind of the node that turns channels into a table.
pub const FROM_CHAN: &str = "table.from_chan";

/// Most rows and most columns `table.from_chan` writes.
const MAX_SIDE: usize = 4096;

/// Most cells `table.from_chan` writes: a table of more would cost its time on every frame.
const MAX_FROM_CHAN_CELLS: usize = 1 << 18;

/// `table.from_chan`: the reverse of [`FromTable`]. Numbers are written so that they read back
/// exactly. A cell without a channel behind it is empty.
struct FromChan;

impl Operator for FromChan {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let chans = cx.input_as::<Chans>(0).ok_or_else(|| waiting(cx))?;
        let named = cx.bool("names")?;
        let mut notes: Vec<String> = Vec::new();
        let rows = match cx.text("layout")? {
            "rows" => by_rows(chans, named, &mut notes),
            "numbered" => numbered(chans, named, cx.text("keep")?.trim(), &mut notes),
            _ => by_columns(chans, named, &mut notes),
        };
        if !notes.is_empty() {
            cx.warn(notes.join("; "));
        }
        out(Table::new(rows)?)
    }
}

/// A number as a cell.
fn number(v: f64) -> String {
    // No "-0".
    if v == 0.0 { "0".to_string() } else { v.to_string() }
}

/// How many of `lines` lines of `cells` cells each fit into what `table.from_chan` writes, with
/// a note when not all do.
fn fitting(lines: usize, cells: usize, what: &str, notes: &mut Vec<String>) -> usize {
    let room = (MAX_FROM_CHAN_CELLS / cells.max(1)).clamp(1, MAX_SIDE);
    if lines > room {
        notes.push(format!("{lines} {what}: the first {room} are kept"));
    }
    lines.min(room)
}

/// A column per channel, a row per sample.
fn by_columns(chans: &Chans, named: bool, notes: &mut Vec<String>) -> Vec<Vec<String>> {
    let channels = chans.channels();
    let width = channels.len().min(MAX_SIDE);
    if channels.len() > width {
        notes.push(format!("{} channels: the first {width} are kept", channels.len()));
    }
    let Some(channels) = channels.get(..width).filter(|c| !c.is_empty()) else { return Vec::new() };
    let len = fitting(chans.len(), width, "samples", notes);
    let mut rows = Vec::with_capacity(len + 1);
    if named {
        rows.push(channels.iter().map(|c| c.name.as_str().to_string()).collect());
    }
    for k in 0..len {
        rows.push(
            channels.iter().map(|c| c.samples.get(k).copied().map(number).unwrap_or_default()).collect(),
        );
    }
    rows
}

/// A row per channel, a column per sample.
fn by_rows(chans: &Chans, named: bool, notes: &mut Vec<String>) -> Vec<Vec<String>> {
    let len = chans.len().min(MAX_SIDE);
    if chans.len() > len {
        notes.push(format!("{} samples: the first {len} are kept", chans.len()));
    }
    let count = fitting(chans.channels().len(), len + usize::from(named), "channels", notes);
    chans
        .channels()
        .iter()
        .take(count)
        .map(|c| {
            let name = named.then(|| c.name.as_str().to_string());
            name.into_iter().chain(c.samples.iter().take(len).copied().map(number)).collect()
        })
        .collect()
}

/// A channel's name without the number at its end, and that number. None for a name without
/// one, for a name that is all number and for a number too long to count with.
pub(crate) fn numbered_name(name: &str) -> Option<(&str, u64)> {
    let stem = name.trim_end_matches(|c: char| c.is_ascii_digit());
    let digits = name.get(stem.len()..)?;
    if stem.is_empty() || digits.is_empty() {
        return None;
    }
    digits.parse().ok().map(|n| (stem, n))
}

/// A row per number at the end of the channels' names, a column per name without its number.
/// A channel without a number fills the cells of its column that no numbered channel filled.
/// Rows whose cell in the column `keep` is 0 or empty are left out.
fn numbered(chans: &Chans, named: bool, keep: &str, notes: &mut Vec<String>) -> Vec<Vec<String>> {
    let channels = chans.channels();
    let parts: Vec<(&str, Option<u64>)> = channels
        .iter()
        .map(|c| numbered_name(c.name.as_str()).map_or((c.name.as_str(), None), |(s, n)| (s, Some(n))))
        .collect();
    let mut columns: Vec<&str> = Vec::new();
    for (stem, _) in &parts {
        if !columns.contains(stem) {
            columns.push(stem);
        }
    }
    if columns.len() > MAX_SIDE {
        notes.push(format!("{} columns: the first {MAX_SIDE} are kept", columns.len()));
        columns.truncate(MAX_SIDE);
    }
    if columns.is_empty() {
        return Vec::new();
    }
    let numbers = || parts.iter().filter_map(|(_, n)| *n);
    // Without a single number: one row of every channel's value.
    let first = numbers().min().unwrap_or(0);
    let last = numbers().max().unwrap_or(0);
    let span = usize::try_from(last - first).unwrap_or(usize::MAX).saturating_add(1);
    let count = fitting(span, columns.len(), "rows between the lowest and the highest number", notes);
    let mut cells: Vec<Vec<Option<f64>>> = vec![vec![None; columns.len()]; count];
    let column_of = |stem: &str| columns.iter().position(|c| *c == stem);
    for (channel, (stem, n)) in channels.iter().zip(&parts) {
        let (Some(col), Some(n)) = (column_of(stem), n) else { continue };
        let row = usize::try_from(n - first).ok().and_then(|r| cells.get_mut(r));
        if let Some(cell) = row.and_then(|r| r.get_mut(col)) {
            *cell = Some(channel.value());
        }
    }
    for (channel, (stem, n)) in channels.iter().zip(&parts) {
        let (Some(col), None) = (column_of(stem), n) else { continue };
        for cell in cells.iter_mut().filter_map(|r| r.get_mut(col)) {
            cell.get_or_insert(channel.value());
        }
    }
    let kept = match (keep.is_empty(), column_of(keep)) {
        (true, _) => None,
        (false, Some(col)) => Some(col),
        (false, None) => {
            notes.push(format!(
                "`keep` names the column `{}`, which no channel makes: every row stays",
                short(keep)
            ));
            None
        }
    };
    let mut rows = Vec::with_capacity(count + 1);
    if named {
        rows.push(columns.iter().map(ToString::to_string).collect());
    }
    for row in cells {
        if kept.is_some_and(|col| row.get(col).copied().flatten().unwrap_or(0.0) == 0.0) {
            continue;
        }
        rows.push(row.into_iter().map(|v| v.map(number).unwrap_or_default()).collect());
    }
    rows
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

/// `chan.from_table`: a channel per column (or row). The last value is the one a binding reads.
/// Empty cells and missing cells of a short row are 0; text that is no number is 0 too, with a
/// warning. Columns (or rows) without a single filled cell are left out, so a trailing comma
/// makes no channel; so are named ones holding words alone, such as a label per row.
/// A row is picked by `key` (matched like a choice's entry: trimmed, ASCII case aside) or by
/// `row`; a missing key falls back to the first row, so a bound param still gets a number.
struct FromTable;

/// Names a `twice` warning lists before it only counts the rest.
const SHOWN_TWICE: usize = 8;

/// Keys a warning about a missing key lists before it only counts the rest.
const SHOWN_KEYS: usize = 8;

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
        // One sample of each channel, when `key` or `row` picks one.
        let key = cx.text("key")?.trim();
        let pick = if key.is_empty() {
            usize::try_from(cx.int("row")?).ok().map(|r| r.min(len - 1))
        } else {
            let found = (first..cells).find(|&k| cell(0, k).0.eq_ignore_ascii_case(key));
            if found.is_none() {
                cx.warn(missing_key(key, (first..cells).map(|k| cell(0, k).0), by_rows));
            }
            Some(found.map_or(0, |k| k - first))
        };
        let mut channels: Vec<Channel> = Vec::new();
        let mut names: BTreeSet<String> = BTreeSet::new();
        // The first cell that is no number, and how many there are.
        let mut not_numbers: (Option<String>, usize) = (None, 0);
        let mut twice: Vec<String> = Vec::new();
        let mut twice_count = 0usize;
        let mut cut = false;
        for i in 0..lines {
            if (0..cells).all(|k| cell(i, k).0.is_empty()) {
                continue;
            }
            if channels.len() >= MAX_CHANNELS {
                cut = true;
                break;
            }
            let name = if named { channel_name(cell(i, 0).0) } else { None };
            let name = match name {
                Some(n) => n,
                None => Name::new(&format!("chan{i}"))?,
            };
            if names.contains(name.as_str()) {
                twice_count += 1;
                if twice.len() < SHOWN_TWICE {
                    twice.push(name.as_str().to_string());
                }
                continue;
            }
            let mut samples = vec![0.0; len];
            let mut words: (Option<String>, usize) = (None, 0);
            let mut numbers = 0usize;
            for (k, s) in (first..cells).zip(samples.iter_mut()) {
                let (text, row, col) = cell(i, k);
                if text.is_empty() {
                    continue;
                }
                match text.parse::<f64>() {
                    Ok(v) => {
                        *s = v;
                        numbers += 1;
                    }
                    Err(_) => {
                        if words.0.is_none() {
                            words.0 = Some(format!("`{}` (row {row}, column {col})", short(text)));
                        }
                        words.1 += 1;
                    }
                }
            }
            // A named column of words alone (a label per row) gives no channel and no warning.
            if named && numbers == 0 && words.1 > 0 {
                continue;
            }
            if not_numbers.0.is_none() {
                not_numbers.0 = words.0;
            }
            not_numbers.1 += words.1;
            if let Some(k) = pick {
                samples = vec![samples.get(k).copied().unwrap_or(0.0)];
            }
            names.insert(name.as_str().to_string());
            channels.push(Channel { name, samples });
        }
        let mut warnings = Vec::new();
        if named && cells < 2 && !channels.is_empty() {
            warnings.push("there are only names, no values: every channel is 0".to_string());
        }
        if let (Some(t), n) = &not_numbers {
            let more = match n {
                0 | 1 => String::new(),
                n => format!(" and {} more cells", n - 1),
            };
            warnings.push(format!("{t}{more} hold no number and count as 0"));
        }
        if !twice.is_empty() {
            let more = match twice_count.saturating_sub(twice.len()) {
                0 => String::new(),
                n => format!(" and {n} more"),
            };
            warnings.push(format!("{}{more} named twice, the first is kept", twice.join(", ")));
        }
        if cut {
            let what = if by_rows { "rows" } else { "columns" };
            warnings.push(format!(
                "more than {MAX_CHANNELS} {what} with values: the first {MAX_CHANNELS} are kept"
            ));
        }
        if !warnings.is_empty() {
            cx.warn(warnings.join("; "));
        }
        let axis = if len > 1 && pick.is_none() { SampleAxis::Index } else { SampleAxis::Time };
        let chans = Chans::new(rate, channels)?.with_axis(axis);
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, chans)]))
    }
}

/// The warning for a `key` that no row (or column) of the table starts with: the keys it has,
/// the first few of them, and what applies instead.
fn missing_key<'a>(key: &str, keys: impl Iterator<Item = &'a str>, by_rows: bool) -> String {
    let keys: Vec<&str> = keys.filter(|k| !k.is_empty()).collect();
    let what = if by_rows { "column" } else { "row" };
    let has = match keys.len() {
        0 => format!("the table has no {what}s"),
        n if n <= SHOWN_KEYS => format!("its keys: {}", keys.join(", ")),
        n => {
            let shown: Vec<&str> = keys.iter().take(SHOWN_KEYS).copied().collect();
            format!("its keys: {} and {} more", shown.join(", "), n - SHOWN_KEYS)
        }
    };
    format!("no {what} has the key `{}`, so the first {what} applies; {has}", short(key))
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
    fn a_wide_table_stops_at_the_channel_limit() {
        // Rows, unnamed: one channel per row, more rows than channels can be.
        use super::{MAX_CHANNELS, SHOWN_TWICE};
        let rows = MAX_CHANNELS + 100;
        let text: String = (0..rows).map(|i| format!("{i}\n")).collect();
        let (c, warning) = from_table(&text, &[set("/c.layout", "rows"), set("/c.names", false)]);
        assert_eq!(c.channels().len(), MAX_CHANNELS);
        assert!(warning.is_some_and(|w| w.contains(&format!("the first {MAX_CHANNELS} are kept"))));
        // The same name many times: listed a few times, the rest counted.
        let text = format!("{}\n{}\n", vec!["a"; 50].join(","), vec!["1"; 50].join(","));
        let (c, warning) = from_table(&text, &[]);
        assert_eq!(names(&c), ["a"]);
        let warning = warning.unwrap_or_default();
        assert!(warning.contains(&format!("and {} more named twice", 49 - SHOWN_TWICE)), "{warning}");
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
        // A column of words alone (a label per row) makes no channel and no warning.
        let (c, warning) = from_table("pos x,2nd,a,a,role,\n1,abc,2,3,intro,\n", &[]);
        assert_eq!(names(&c), ["pos_x", "a"]);
        assert_eq!(c.value(Some("a")), Some(2.0));
        assert_eq!(c.axis(), SampleAxis::Time, "one value per channel is a value now");
        let warning = warning.unwrap();
        assert!(!warning.contains("abc") && !warning.contains("intro"), "{warning}");
        let (c, warning) = from_table("pos x,2nd,a,a,\n1,abc,2,3,\n4,5,6,7,\n", &[]);
        assert_eq!(names(&c), ["pos_x", "_2nd", "a"]);
        assert_eq!(c.get("_2nd").unwrap().samples, [0.0, 5.0]);
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

        // One row: a cue sheet read at the cue playing; past the end the last row.
        let sheet = "cue,hour\n1,6\n2,12\n3,17";
        let (c, _) = from_table(sheet, &[set("/c.row", 1)]);
        assert_eq!(c.get("hour").unwrap().samples, [12.0]);
        assert_eq!(c.axis(), SampleAxis::Time);
        let (c, _) = from_table(sheet, &[set("/c.row", 9)]);
        assert_eq!(c.get("cue").unwrap().samples, [3.0]);
        let (c, _) = from_table("a,1,2\nb,3,4", &[set("/c.layout", "rows"), set("/c.row", 0)]);
        assert_eq!(c.get("b").unwrap().samples, [3.0], "with layout rows: a column");
    }

    /// Cooks `/out` (a `table.from_chan`) and returns its table and warning. Its channels come
    /// from the csv `text` through a `chan.from_table` at `/c`; `ops` come after.
    fn from_chan(text: &str, ops: &[Op]) -> (Table, Option<String>) {
        let types = Arc::new(builtin_types().unwrap());
        let wire = |from: &str, to: &str, input: u32| Op::Connect {
            from: from.parse().unwrap(),
            output: 0,
            to: to.parse().unwrap(),
            input,
        };
        let mut all = vec![
            create("/t", "table.text"),
            set("/t.text", text),
            set("/t.format", "csv"),
            create("/c", "chan.from_table"),
            wire("/t", "/c", 0),
            create("/out", "table.from_chan"),
            wire("/c", "/out", 0),
        ];
        all.extend_from_slice(ops);
        let net = Arc::new(Network::new().apply(&types, &all).unwrap().network);
        let mut cooker = Cooker::new(types, Arc::new(JobPool::new(1).unwrap()));
        let id = net.resolve(&"/out".parse().unwrap()).unwrap();
        let clock = ShowClock::new(Rate::FPS_60, Default::default());
        cooker.cook(&net, clock.now(), &[id], &[]);
        let status = cooker.status(id).unwrap();
        assert!(status.error.is_none(), "{:?}", status.error);
        let table = cooker.output(Port { node: id, output: 0 }).unwrap().get::<Table>().unwrap().clone();
        (table, status.warning)
    }

    fn csv(table: &Table) -> String {
        table.to_text(penumbra_core::table::Format::Csv).unwrap()
    }

    #[test]
    fn channels_become_a_table_and_read_back_as_they_were() {
        // A column per channel, a row per sample: what chan.from_table read comes out again.
        let text = "tx,ty,tz\n1,-2.5,0.4\n3,0,1e-7\n";
        let (t, warning) = from_chan(text, &[]);
        assert_eq!((csv(&t).as_str(), warning), ("tx,ty,tz\n1,-2.5,0.4\n3,0,0.0000001\n", None));
        // One value per channel: the names and one row, as a group of one body reads it.
        let (t, _) = from_chan("tx,tz\n-4,0.4\n", &[]);
        assert_eq!(t.rows(), [["tx", "tz"], ["-4", "0.4"]]);
        let (t, _) = from_chan("tx,tz\n-4,0.4\n", &[set("/out.names", false)]);
        assert_eq!(t.rows(), [["-4", "0.4"]]);
        // A row per channel.
        let (t, _) = from_chan(text, &[set("/out.layout", "rows")]);
        assert_eq!(csv(&t), "tx,1,3\nty,-2.5,0\ntz,0.4,0.0000001\n");
        let (t, _) = from_chan(text, &[set("/out.layout", "rows"), set("/out.names", false)]);
        assert_eq!(t.rows()[1], ["-2.5", "0"]);
        // No channels: no rows, and nothing to warn about.
        let (t, warning) = from_chan("", &[]);
        assert_eq!((t.num_rows(), warning), (0, None));
        let (t, warning) = from_chan("", &[set("/out.layout", "numbered"), set("/out.keep", "id")]);
        assert_eq!((t.num_rows(), warning), (0, None));
        // A column to keep by that no channel makes is said, and every row stays.
        let (t, warning) = from_chan("x1\n5\n", &[set("/out.layout", "numbered"), set("/out.keep", "id")]);
        assert_eq!(t.rows(), [["x"], ["5"]]);
        assert!(warning.unwrap().contains("`keep` names the column `id`"));
    }

    #[test]
    fn numbered_channels_become_a_row_per_number() {
        // The slots of a tracker: slot 2 is empty. `tz` has no number and fills every row.
        let slots = "id1,x1,y1,id2,x2,y2,id3,x3,y3,tz\n7,0.25,0.5,0,0,0,9,1,0,0.4\n";
        let (t, warning) = from_chan(slots, &[set("/out.layout", "numbered")]);
        assert_eq!(warning, None);
        assert_eq!(csv(&t), "id,x,y,tz\n7,0.25,0.5,0.4\n0,0,0,0.4\n9,1,0,0.4\n");
        // `keep` leaves out the rows of the empty slots.
        let kept = [set("/out.layout", "numbered"), set("/out.keep", " id ")];
        let (t, warning) = from_chan(slots, &kept);
        assert_eq!((csv(&t).as_str(), warning), ("id,x,y,tz\n7,0.25,0.5,0.4\n9,1,0,0.4\n", None));
        let (t, _) = from_chan("id1,id2\n0,0\n", &kept);
        assert_eq!(t.rows(), [["id"]], "no blob: the names and no row");
        // Numbers need not start at 1 nor be without gaps: a number nobody has is an empty
        // cell, and a channel with a number wins over the one without in its own row.
        let (t, _) = from_chan("a0,a2,b2,b\n1,2,3,4\n", &[set("/out.layout", "numbered")]);
        assert_eq!(csv(&t), "a,b\n1,4\n,4\n2,3\n");
        // No number anywhere: one row of every channel.
        let (t, _) = from_chan("tx,tz\n1,2\n", &[set("/out.layout", "numbered")]);
        assert_eq!(t.rows(), [["tx", "tz"], ["1", "2"]]);
        // A number far off does not make a table of that many rows.
        let far = [set("/out.layout", "numbered")];
        let (t, warning) = from_chan("x1,x4000000000,x99999999999999999999999\n1,2,3\n", &far);
        assert!(t.num_rows() <= 1 + super::MAX_SIDE && t.cell(1, 0) == "1", "{}", t.num_rows());
        assert_eq!(t.rows()[0], ["x", "x99999999999999999999999"], "a number too long is a name");
        assert!(warning.unwrap().contains("the first 4096 are kept"));
        assert_eq!(super::numbered_name("x12"), Some(("x", 12)));
        assert_eq!((super::numbered_name("x"), super::numbered_name("_7")), (None, Some(("_", 7))));
    }

    #[test]
    fn math_scales_the_slots_of_a_tracker_axis_by_axis() {
        // b has `x` and `y`: x1 and x2 meet x, y1 and y2 meet y. A name b has as it is wins.
        let ops = [
            create("/bt", "table.text"),
            set("/bt.text", "x,y,y2\n4,-3,100\n"),
            set("/bt.format", "csv"),
            create("/b", "chan.from_table"),
            create("/m", "chan.math"),
            set("/m.op", "multiply"),
            Op::Connect { from: "/bt".parse().unwrap(), output: 0, to: "/b".parse().unwrap(), input: 0 },
            Op::Connect { from: "/c".parse().unwrap(), output: 0, to: "/m".parse().unwrap(), input: 0 },
            Op::Connect { from: "/b".parse().unwrap(), output: 0, to: "/m".parse().unwrap(), input: 1 },
            Op::Connect { from: "/m".parse().unwrap(), output: 0, to: "/out".parse().unwrap(), input: 0 },
        ];
        let (t, _) = from_chan("x1,y1,x2,y2,id1\n0.5,0.5,-1,2,3\n", &ops);
        assert_eq!(csv(&t), "x1,y1,x2,y2,id1\n2,-1.5,-4,200,12\n");
    }

    #[test]
    fn a_key_picks_the_row_whose_first_cell_it_is() {
        let looks = "look,steps,full\nwindow,12,1\nsnowflake,32,0.49\ncoral,16,0.16";
        let (c, warning) = from_table(looks, &[set("/c.key", " SNOWFLAKE ")]);
        assert_eq!(names(&c), ["steps", "full"], "the column of keys makes no channel");
        assert_eq!(c.get("steps").unwrap().samples, [32.0]);
        assert_eq!(c.value(Some("full")), Some(0.49));
        assert_eq!(c.axis(), SampleAxis::Time);
        assert_eq!(warning, None);
        // The key wins over row; an empty key leaves the choice to row.
        let (c, _) = from_table(looks, &[set("/c.key", "coral"), set("/c.row", 0)]);
        assert_eq!(c.value(Some("steps")), Some(16.0));
        let (c, _) = from_table(looks, &[set("/c.key", ""), set("/c.row", 0)]);
        assert_eq!(c.value(Some("steps")), Some(12.0));

        // A key no row has: the first row, and a warning that lists the keys.
        let (c, warning) = from_table(looks, &[set("/c.key", "custom")]);
        assert_eq!(c.get("steps").unwrap().samples, [12.0]);
        assert_eq!(
            warning.as_deref(),
            Some("no row has the key `custom`, so the first row applies; its keys: window, snowflake, coral")
        );
        let many: String = (0..12).map(|i| format!("k{i},{i}\n")).collect();
        let (c, warning) = from_table(&format!("key,v\n{many}"), &[set("/c.key", "nope")]);
        assert_eq!(c.value(Some("v")), Some(0.0));
        assert!(warning.unwrap().ends_with("k6, k7 and 4 more"));
        let (c, warning) = from_table("look,steps", &[set("/c.key", "a")]);
        assert_eq!(c.value(Some("steps")), Some(0.0));
        assert!(warning.unwrap().contains("the table has no rows"));

        // Without names every row counts; with layout rows the key names a column.
        let (c, _) = from_table("a,1\nb,2", &[set("/c.names", false), set("/c.key", "b")]);
        assert_eq!(c.value(Some("chan1")), Some(2.0));
        let (c, warning) =
            from_table("look,window,fern\nsteps,12,32", &[set("/c.layout", "rows"), set("/c.key", "fern")]);
        assert_eq!(names(&c), ["steps"]);
        assert_eq!(c.value(Some("steps")), Some(32.0));
        assert_eq!(warning, None);
    }
}
