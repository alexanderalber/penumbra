//! `ui.note`: a comment on the network canvas. It holds text and a size, has no inputs and no
//! outputs and never cooks anything. The editor draws it as a box of text, without header,
//! ports or thumbnail ([`text_lines`] says how the text reads), and leaves it out of wire routing
//! and arrangement. Being a node, a note is created, edited, moved, copied and saved like any
//! other node, by the same commands.

use penumbra_core::{Error, ParamSpec, Value};
use penumbra_graph::{CookContext, CookError, Cooked, FnNodeType, NodeDef, NodeDesc, NodeTypes, Operator};

/// Kind of the note node.
pub const NOTE: &str = "ui.note";
/// The note's text.
pub const TEXT: &str = "text";
/// Width and height on the canvas, in graph units (a node is 150 wide).
pub const WIDTH: &str = "width";
pub const HEIGHT: &str = "height";
/// Size of a new note.
pub const DEFAULT_SIZE: [f64; 2] = [300.0, 140.0];
/// The smallest and largest size either side may have.
pub const MIN_SIZE: f64 = 60.0;
pub const MAX_SIZE: f64 = 4000.0;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            NOTE,
            "A comment on the canvas: text in a box you place and size freely. It has no ports and changes nothing in the show",
        )
        .param(ParamSpec::text(TEXT, "").lines(8).doc(
            "What the note says. A line starting with \"# \" is a heading, one starting with \"- \" a bullet. \
             A name in backticks stands out. Long lines wrap at the note's width",
        ))
        .param(
            ParamSpec::int(WIDTH, DEFAULT_SIZE[0] as i64)
                .clamp(MIN_SIZE, MAX_SIZE)
                .doc("Width of the note on the canvas, in the units of the network (a node is 150 wide). The handle at its lower right corner drags it"),
        )
        .param(
            ParamSpec::int(HEIGHT, DEFAULT_SIZE[1] as i64)
                .clamp(MIN_SIZE, MAX_SIZE)
                .doc("Height of the note on the canvas, in the units of the network. Text beyond it is cut off"),
        ),
        || Box::new(Note) as Box<dyn Operator>,
    ))
}

/// Whether `node` is a note.
pub fn is_note(node: &NodeDef) -> bool {
    node.kind == NOTE
}

/// The size of note `node` on the canvas, in graph units; `None` for other nodes. A value
/// missing or out of range in the file reads as the default or the nearest limit.
pub fn size(node: &NodeDef) -> Option<[f32; 2]> {
    if !is_note(node) {
        return None;
    }
    let side = |name: &str, default: f64| {
        let v = node.params.get(name).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(default);
        v.clamp(MIN_SIZE, MAX_SIZE) as f32
    };
    Some([side(WIDTH, DEFAULT_SIZE[0]), side(HEIGHT, DEFAULT_SIZE[1])])
}

/// The text of note `node`, empty when it has none.
pub fn text(node: &NodeDef) -> &str {
    node.params.get(TEXT).and_then(Value::as_str).unwrap_or("")
}

/// How one line of a note reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Line<'a> {
    /// `# text`: larger and brighter.
    Heading(&'a str),
    /// `- text`: indented behind a bullet.
    Bullet(&'a str),
    Plain(&'a str),
}

/// The lines of a note's text, each as it reads. The only markup: `# ` starts a heading,
/// `- ` (or `* `) a bullet, and within a line names in backticks stand out ([`spans`]).
/// Everything else is shown as written.
pub fn text_lines(text: &str) -> Vec<Line<'_>> {
    text.lines()
        .map(|l| {
            let l = l.trim_end_matches('\r');
            if let Some(h) = l.strip_prefix("# ") {
                Line::Heading(h.trim())
            } else if let Some(b) = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
                Line::Bullet(b.trim_start())
            } else {
                Line::Plain(l)
            }
        })
        .collect()
}

/// The pieces of one line: text, and names in backticks (`true`, the backticks dropped). A
/// backtick without a partner stays as it is.
pub fn spans(line: &str) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(len) = after.find('`') else { break };
        if open > 0 {
            out.push((&rest[..open], false));
        }
        out.push((&after[..len], true));
        rest = &after[len + 1..];
    }
    if !rest.is_empty() || out.is_empty() {
        out.push((rest, false));
    }
    out
}

struct Note;

impl Operator for Note {
    fn cook(&mut self, _cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        Ok(Cooked::Outputs(Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_read_as_headings_bullets_and_plain_text() {
        let lines = text_lines("# Title\n- one\n* two\nplain #not\n\n#nospace");
        assert_eq!(
            lines,
            [
                Line::Heading("Title"),
                Line::Bullet("one"),
                Line::Bullet("two"),
                Line::Plain("plain #not"),
                Line::Plain(""),
                Line::Plain("#nospace"),
            ]
        );
    }

    #[test]
    fn names_in_backticks_are_spans_of_their_own() {
        let expected = [("set ", false), ("amount", true), (" on ", false), ("sharpen", true)];
        assert_eq!(spans("set `amount` on `sharpen`"), expected);
        assert_eq!(spans("one ` alone"), [("one ` alone", false)]);
        assert_eq!(spans("`a` and ` b"), [("a", true), (" and ` b", false)]);
        assert_eq!(spans(""), [("", false)]);
    }

    #[test]
    fn the_declaration_is_valid() {
        let mut types = NodeTypes::new();
        register(&mut types).unwrap();
        types.desc(NOTE).unwrap().validate().unwrap();
    }
}
