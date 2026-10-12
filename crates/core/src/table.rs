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

    #[test]
    fn cell_limit() {
        let text = "\n".repeat(MAX_CELLS + 1);
        assert!(Table::parse(&text, Format::Lines).is_err());
    }
}
