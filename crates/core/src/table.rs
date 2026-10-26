//! Table data, what flows along a `table` connection: rows of text cells, like a TouchDesigner
//! DAT. Rows may differ in length; a missing cell reads as empty. A plain text is a table with
//! one cell per line.

use crate::Error;

/// Most cells in one table.
pub const MAX_CELLS: usize = 1 << 20;

/// How text becomes a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// One row per line, one cell per row.
    Lines,
    /// Cells separated by tabs.
    Tsv,
    /// Comma-separated values with RFC 4180 quoting (`"a, b"`, `""` for a quote, line breaks
    /// inside quotes).
    Csv,
}

impl Format {
    pub const NAMES: [&'static str; 3] = ["lines", "tsv", "csv"];

    pub fn from_name(name: &str) -> Option<Format> {
        match name {
            "lines" => Some(Format::Lines),
            "tsv" => Some(Format::Tsv),
            "csv" => Some(Format::Csv),
            _ => None,
        }
    }

    /// By file extension: `csv`, `tsv` (also `tab`), else lines.
    pub fn from_extension(ext: &str) -> Format {
        match ext.to_ascii_lowercase().as_str() {
            "csv" => Format::Csv,
            "tsv" | "tab" => Format::Tsv,
            _ => Format::Lines,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new(rows: Vec<Vec<String>>) -> Result<Self, Error> {
        let cells: usize = rows.iter().map(|r| r.len().max(1)).sum();
        if cells > MAX_CELLS {
            return Err(Error::new(format!("more than {MAX_CELLS} cells")));
        }
        Ok(Self { rows })
    }

    /// Parses `text`. A trailing line break does not make an empty last row; `\r\n` counts as
    /// one line break.
    pub fn parse(text: &str, format: Format) -> Result<Self, Error> {
        let rows = match format {
            Format::Lines => lines(text).map(|l| vec![l.to_string()]).collect(),
            Format::Tsv => lines(text).map(|l| l.split('\t').map(str::to_string).collect()).collect(),
            Format::Csv => csv(text),
        };
        Self::new(rows)
    }

    pub fn rows(&self) -> &[Vec<String>] {
        &self.rows
    }

    pub fn num_rows(&self) -> usize {
        self.rows.len()
    }

    /// The length of the longest row.
    pub fn num_cols(&self) -> usize {
        self.rows.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// The cell at `row`, `col`; empty when outside the table.
    pub fn cell(&self, row: usize, col: usize) -> &str {
        self.rows.get(row).and_then(|r| r.get(col)).map_or("", String::as_str)
    }

    /// The table as text in `format`, each row ended by `\n`; [`Table::parse`] reads it back.
    /// CSV quotes a cell holding a comma, a quote or a line break. TSV and lines have no
    /// quoting: a cell holding a tab (TSV) or a line break fails, and so does a row of more than
    /// one cell in lines. A table of one empty row reads back as no rows in TSV and lines.
    pub fn to_text(&self, format: Format) -> Result<String, Error> {
        let mut text = String::new();
        for (r, row) in self.rows.iter().enumerate() {
            if format == Format::Lines && row.len() > 1 {
                return Err(Error::new(format!(
                    "row {} has {} cells; lines holds one cell per row",
                    r + 1,
                    row.len()
                )));
            }
            for (c, cell) in row.iter().enumerate() {
                if c > 0 {
                    text.push(if format == Format::Csv { ',' } else { '\t' });
                }
                let (bad, what): (&[char], &str) = match format {
                    Format::Csv => {
                        if cell.contains([',', '"', '\n', '\r']) {
                            text.push('"');
                            text.push_str(&cell.replace('"', "\"\""));
                            text.push('"');
                        } else {
                            text.push_str(cell);
                        }
                        continue;
                    }
                    Format::Tsv => (&['\t', '\n', '\r'], "a tab or line break"),
                    Format::Lines => (&['\n', '\r'], "a line break"),
                };
                if cell.contains(bad) {
                    return Err(Error::new(format!(
                        "the cell in row {}, column {} holds {what}; write it as csv",
                        r + 1,
                        c + 1
                    )));
                }
                text.push_str(cell);
            }
            text.push('\n');
        }
        Ok(text)
    }
}

fn lines(text: &str) -> impl Iterator<Item = &str> {
    let text = text.strip_suffix('\n').unwrap_or(text);
    let empty = text.is_empty();
    text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).filter(move |_| !empty)
}

fn csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    // Whether anything of the current row has been read, so a trailing line break does not
    // add an empty row.
    let mut started = false;
    while let Some(c) = chars.next() {
        started = true;
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    cell.push('"');
                }
                '"' => quoted = false,
                _ => cell.push(c),
            }
            continue;
        }
        match c {
            '"' if cell.is_empty() => quoted = true,
            ',' => row.push(std::mem::take(&mut cell)),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                row.push(std::mem::take(&mut cell));
                rows.push(std::mem::take(&mut row));
                started = false;
            }
            _ => cell.push(c),
        }
    }
    if started {
        row.push(cell);
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(t: &Table) -> Vec<Vec<&str>> {
        t.rows().iter().map(|r| r.iter().map(String::as_str).collect()).collect()
    }

    #[test]
    fn formats() {
        let t = Table::parse("a\r\nb\n\nc\n", Format::Lines).unwrap();
        assert_eq!(rows(&t), [["a"], ["b"], [""], ["c"]]);
        assert_eq!(Table::parse("", Format::Lines).unwrap().num_rows(), 0);
        let t = Table::parse("name\tage\nalex\t3\nsolo\n", Format::Tsv).unwrap();
        assert_eq!(rows(&t), vec![vec!["name", "age"], vec!["alex", "3"], vec!["solo"]]);
        assert_eq!(t.num_cols(), 2);
        assert_eq!(t.cell(2, 1), "");
        assert_eq!(t.cell(9, 9), "");
        let t = Table::parse("a,\"b, c\",\"say \"\"hi\"\"\"\r\n\"two\nlines\",x\n", Format::Csv).unwrap();
        assert_eq!(rows(&t), vec![vec!["a", "b, c", "say \"hi\""], vec!["two\nlines", "x"]]);
        let t = Table::parse("a,b", Format::Csv).unwrap();
        assert_eq!(rows(&t), [["a", "b"]]);
        // An unterminated quote keeps what it has instead of failing.
        let t = Table::parse("\"open", Format::Csv).unwrap();
        assert_eq!(rows(&t), [["open"]]);
        assert_eq!(Format::from_extension("CSV"), Format::Csv);
        assert_eq!(Format::from_extension("txt"), Format::Lines);
    }

    fn table(rows: &[&[&str]]) -> Table {
        Table::new(rows.iter().map(|r| r.iter().map(|c| c.to_string()).collect()).collect()).unwrap()
    }

    #[test]
    fn written_text_reads_back() {
        let t = table(&[
            &["name", "say \"hi\"", ""],
            &["a, b", "two\nlines", "cr\r\nlf"],
            &["solo"],
            &[],
            &["\"lead", " spaced ", "x"],
        ]);
        let csv = t.to_text(Format::Csv).unwrap();
        assert_eq!(csv.lines().next(), Some("name,\"say \"\"hi\"\"\","));
        let back = Table::parse(&csv, Format::Csv).unwrap();
        // An empty row comes back as one empty cell.
        let mut want = t.rows().to_vec();
        want[3] = vec![String::new()];
        assert_eq!(back.rows(), want.as_slice());

        let t = table(&[&["name", "age"], &["alex", ""], &["solo"], &[""], &["a, \"b\""]]);
        for format in [Format::Csv, Format::Tsv] {
            let text = t.to_text(format).unwrap();
            assert_eq!(Table::parse(&text, format).unwrap(), t, "{format:?}: {text:?}");
        }
        assert_eq!(t.to_text(Format::Tsv).unwrap(), "name\tage\nalex\t\nsolo\n\na, \"b\"\n");
        let lines = table(&[&["one"], &["two, three"], &[""]]);
        assert_eq!(Table::parse(&lines.to_text(Format::Lines).unwrap(), Format::Lines).unwrap(), lines);
        assert_eq!(Table::default().to_text(Format::Csv).unwrap(), "");

        // No quoting in TSV and lines: what cannot be read back fails.
        assert!(table(&[&["a\tb"]]).to_text(Format::Tsv).unwrap_err().0.contains("row 1, column 1"));
        assert!(table(&[&["x", "a\nb"]]).to_text(Format::Tsv).unwrap_err().0.contains("column 2"));
        assert!(table(&[&["a\tb"]]).to_text(Format::Lines).is_ok());
        assert!(table(&[&["a\rb"]]).to_text(Format::Lines).is_err());
        assert!(table(&[&["a", "b"]]).to_text(Format::Lines).unwrap_err().0.contains("2 cells"));
    }

    #[test]
    fn cell_limit() {
        let text = "\n".repeat(MAX_CELLS + 1);
        assert!(Table::parse(&text, Format::Lines).is_err());
    }
}
