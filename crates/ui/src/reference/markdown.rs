//! The markdown the hand-written docs use, turned into HTML for the single-page documentation:
//! headings, paragraphs, nested lists, tables, fenced code, inline code, emphasis, links and
//! images. Not a general markdown parser; what the docs do not use is not handled.

use std::collections::{BTreeSet, HashMap};

/// Text with `&`, `<`, `>` and `"` escaped, for element content and attribute values alike.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// The anchor GitHub gives a heading: lower case, punctuation dropped (but `-` and `_`),
/// spaces as `-`. Links between the docs use these.
pub fn slug(text: &str) -> String {
    text.trim()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

/// A heading of a rendered document.
#[derive(Clone, Debug)]
pub struct Heading {
    pub level: usize,
    /// Its text without markup.
    pub text: String,
    pub id: String,
}

/// What [`render`] makes of a document.
pub struct Rendered {
    pub html: String,
    pub headings: Vec<Heading>,
    /// Inline code naming a known entry, with the index of the heading it sits under.
    pub mentions: Vec<(String, Option<usize>)>,
}

/// How a document is rendered.
pub struct Options<'a> {
    /// Prefix of every heading's id, so headings of different documents cannot collide.
    pub id_prefix: &'a str,
    /// Added to every heading's level (a document's `#` becomes an `h2` with 1).
    pub shift: usize,
    /// The ids of the entries (node kinds, commands): inline code naming one links to it and
    /// counts as a mention.
    pub entries: &'a BTreeSet<String>,
    /// Rewrites a link target (another document becomes an anchor in the page).
    pub link: &'a dyn Fn(&str) -> String,
}

/// Renders `markdown` to HTML.
pub fn render(markdown: &str, options: &Options<'_>) -> Rendered {
    let mut r = Renderer {
        options,
        headings: Vec::new(),
        mentions: Vec::new(),
        used: HashMap::new(),
        in_link: false,
    };
    let lines: Vec<String> = markdown.lines().map(|l| l.replace('\t', "    ")).collect();
    let mut html = String::new();
    r.blocks(&lines, false, &mut html);
    Rendered { html, headings: r.headings, mentions: r.mentions }
}

/// Inline markdown of one line or paragraph to HTML, without headings or mentions.
pub fn inline(text: &str, options: &Options<'_>) -> String {
    let mut r = Renderer {
        options,
        headings: Vec::new(),
        mentions: Vec::new(),
        used: HashMap::new(),
        in_link: false,
    };
    r.inline(text)
}

/// The text of inline markdown without its markup: code spans, emphasis and links as plain
/// text.
pub fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if c == '['
            && let Some((label, _, after)) = link_parts(rest)
        {
            out.push_str(&plain(label));
            rest = after;
            continue;
        }
        if c == '\\'
            && let Some(next) = rest[1..].chars().next().filter(char::is_ascii_punctuation)
        {
            out.push(next);
            rest = &rest[1 + next.len_utf8()..];
            continue;
        }
        if c != '`' && c != '*' {
            out.push(c);
        }
        rest = &rest[c.len_utf8()..];
    }
    out
}

struct Renderer<'a, 'b> {
    options: &'b Options<'a>,
    headings: Vec<Heading>,
    mentions: Vec<(String, Option<usize>)>,
    /// How often each heading id was given, to number repeats as GitHub does.
    used: HashMap<String, usize>,
    /// Inside a link's label, where code naming an entry must not become a link of its own.
    in_link: bool,
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

fn is_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

/// `#` to `######` and a space: the level and the text.
fn heading(line: &str) -> Option<(usize, &str)> {
    let t = line.trim_start();
    if indent(line) > 3 {
        return None;
    }
    let level = t.len() - t.trim_start_matches('#').len();
    let rest = &t[level..];
    ((1..=6).contains(&level) && (rest.is_empty() || rest.starts_with(' ')))
        .then(|| (level, rest.trim().trim_end_matches('#').trim_end()))
}

/// A list item's start: its indent, whether it is numbered (with the number), and where its
/// text starts.
struct Marker {
    indent: usize,
    number: Option<u64>,
    content: usize,
}

fn marker(line: &str) -> Option<Marker> {
    let ind = indent(line);
    let t = &line[ind..];
    let (number, len) = if t.starts_with("- ") || t.starts_with("* ") {
        (None, 1)
    } else {
        let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 || digits > 9 || !t[digits..].starts_with(". ") {
            return None;
        }
        (t[..digits].parse().ok(), digits + 1)
    };
    let spaces = t[len..].len() - t[len..].trim_start_matches(' ').len();
    let spaces = if spaces > 4 { 1 } else { spaces.max(1) };
    Some(Marker { indent: ind, number, content: ind + len + spaces })
}

fn is_table_row(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

fn is_table_separator(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// The cells of a table row, split at `|` that are not escaped, with `\|` as `|`.
fn cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = if t.ends_with('|') && !t.ends_with("\\|") { &t[..t.len() - 1] } else { t };
    let mut out = vec![String::new()];
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                chars.next();
                if let Some(last) = out.last_mut() {
                    last.push('|');
                }
            }
            '|' => out.push(String::new()),
            c => {
                if let Some(last) = out.last_mut() {
                    last.push(c);
                }
            }
        }
    }
    out.into_iter().map(|c| c.trim().to_string()).collect()
}

/// `[label](target)` at the start of `text`: the label, the target and the rest after it.
fn link_parts(text: &str) -> Option<(&str, &str, &str)> {
    let mut depth = 0usize;
    let mut close = None;
    let mut in_code = false;
    for (i, c) in text.char_indices() {
        match c {
            '`' => in_code = !in_code,
            '[' if !in_code => depth += 1,
            ']' if !in_code => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let after = text.get(close + 1..)?.strip_prefix('(')?;
    let end = after.find(')')?;
    let target = after[..end].trim();
    if target.contains(' ') {
        return None;
    }
    Some((&text[1..close], target, &after[end + 1..]))
}

/// Where `pat` next occurs in `text` outside code spans.
fn find_outside_code(text: &str, pat: &str) -> Option<usize> {
    let mut in_code = false;
    for (i, c) in text.char_indices() {
        if c == '`' {
            in_code = !in_code;
        } else if !in_code && text[i..].starts_with(pat) {
            return Some(i);
        }
    }
    None
}

impl Renderer<'_, '_> {
    fn blocks(&mut self, lines: &[String], tight: bool, out: &mut String) {
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i].as_str();
            if is_blank(line) {
                i += 1;
            } else if is_fence(line) {
                let lang = line.trim_start().trim_start_matches('`').trim();
                let base = indent(line);
                let mut code = String::new();
                i += 1;
                while i < lines.len() && !is_fence(&lines[i]) {
                    let l = &lines[i];
                    code.push_str(&l[base.min(indent(l))..]);
                    code.push('\n');
                    i += 1;
                }
                i += 1;
                let class =
                    if lang.is_empty() { String::new() } else { format!(" class=\"lang-{}\"", escape(lang)) };
                out.push_str(&format!("<pre><code{class}>{}</code></pre>\n", escape(&code)));
            } else if let Some((level, text)) = heading(line) {
                self.heading(level, text, out);
                i += 1;
            } else if is_table_row(line) && lines.get(i + 1).is_some_and(|l| is_table_separator(l)) {
                let head = cells(line);
                i += 2;
                out.push_str("<table>\n<thead><tr>");
                for c in &head {
                    out.push_str(&format!("<th>{}</th>", self.inline(c)));
                }
                out.push_str("</tr></thead>\n<tbody>\n");
                while i < lines.len() && is_table_row(&lines[i]) {
                    out.push_str("<tr>");
                    for c in cells(&lines[i]) {
                        out.push_str(&format!("<td>{}</td>", self.inline(&c)));
                    }
                    out.push_str("</tr>\n");
                    i += 1;
                }
                out.push_str("</tbody>\n</table>\n");
            } else if let Some(m) = marker(line) {
                i = self.list(lines, i, m, out);
            } else {
                let mut text = String::new();
                while i < lines.len() {
                    let l = lines[i].as_str();
                    let starts_block = is_fence(l)
                        || heading(l).is_some()
                        || marker(l).is_some()
                        || (is_table_row(l) && lines.get(i + 1).is_some_and(|n| is_table_separator(n)));
                    if is_blank(l) || (!text.is_empty() && starts_block) {
                        break;
                    }
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(l.trim());
                    i += 1;
                }
                let html = self.inline(&text);
                if tight {
                    out.push_str(&html);
                    out.push('\n');
                } else {
                    out.push_str(&format!("<p>{html}</p>\n"));
                }
            }
        }
    }

    fn heading(&mut self, level: usize, text: &str, out: &mut String) {
        let plain = plain(text);
        let base = format!("{}{}", self.options.id_prefix, slug(&plain));
        let n = self.used.entry(base.clone()).or_insert(0);
        let id = if *n == 0 { base.clone() } else { format!("{base}-{n}") };
        *n += 1;
        self.headings.push(Heading { level, text: plain, id: id.clone() });
        let h = (level + self.options.shift).min(6);
        let html = self.inline(text);
        out.push_str(&format!("<h{h} id=\"{}\">{html}</h{h}>\n", escape(&id)));
    }

    /// A list starting at line `start` with its first marker `first`; returns the line after it.
    fn list(&mut self, lines: &[String], start: usize, first: Marker, out: &mut String) -> usize {
        let ordered = first.number.is_some();
        let mut items: Vec<Vec<String>> = Vec::new();
        let mut loose = false;
        let mut i = start;
        let sibling =
            |l: &str| marker(l).filter(|m| m.indent == first.indent && m.number.is_some() == ordered);
        while let Some((line, m)) = lines.get(i).and_then(|l| Some((l, sibling(l)?))) {
            let content = m.content;
            items.push(vec![line.get(content..).unwrap_or("").to_string()]);
            i += 1;
            // The item's further lines.
            while let Some(line) = lines.get(i) {
                if is_blank(line) {
                    let next = lines[i..].iter().position(|l| !is_blank(l)).map(|k| i + k);
                    match next {
                        Some(n) if indent(&lines[n]) >= content => {
                            if let Some(item) = items.last_mut() {
                                item.push(String::new());
                            }
                            loose = true;
                            i += 1;
                        }
                        Some(n)
                            if marker(&lines[n]).is_some_and(|m| {
                                m.indent == first.indent && m.number.is_some() == ordered
                            }) =>
                        {
                            loose = true;
                            i = n;
                            break;
                        }
                        _ => break,
                    }
                } else if indent(line) >= content {
                    if let Some(item) = items.last_mut() {
                        item.push(line[content..].to_string());
                    }
                    i += 1;
                } else if marker(line).is_some()
                    || heading(line).is_some()
                    || is_fence(line)
                    || is_table_row(line)
                {
                    break;
                } else {
                    // A lazy continuation of the item's paragraph.
                    if let Some(item) = items.last_mut() {
                        item.push(line.trim().to_string());
                    }
                    i += 1;
                }
            }
            if lines.get(i).is_none_or(|l| is_blank(l)) {
                break;
            }
        }
        let tag = if ordered { "ol" } else { "ul" };
        match first.number {
            Some(n) if n != 1 => out.push_str(&format!("<ol start=\"{n}\">\n")),
            _ => out.push_str(&format!("<{tag}>\n")),
        }
        for item in items {
            out.push_str("<li>");
            self.blocks(&item, !loose, out);
            out.push_str("</li>\n");
        }
        out.push_str(&format!("</{tag}>\n"));
        i
    }

    fn inline(&mut self, text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(c) = rest.chars().next() {
            match c {
                '\\' => {
                    if let Some(next) = rest[1..].chars().next().filter(char::is_ascii_punctuation) {
                        out.push_str(&escape(&next.to_string()));
                        rest = &rest[1 + next.len_utf8()..];
                        continue;
                    }
                }
                '`' => {
                    let ticks = rest.len() - rest.trim_start_matches('`').len();
                    let fence = &rest[..ticks];
                    let after = &rest[ticks..];
                    if let Some(end) = after.find(fence) {
                        let raw = &after[..end];
                        let code = if raw.starts_with(' ') && raw.ends_with(' ') && raw.trim() != "" {
                            &raw[1..raw.len() - 1]
                        } else {
                            raw
                        };
                        out.push_str(&self.code(code));
                        rest = &after[end + ticks..];
                        continue;
                    }
                    out.push_str(fence);
                    rest = after;
                    continue;
                }
                '!' if rest.starts_with("![") => {
                    if let Some((alt, src, after)) = link_parts(&rest[1..]) {
                        out.push_str(&format!(
                            "<img src=\"{}\" alt=\"{}\">",
                            escape(&(self.options.link)(src)),
                            escape(&plain(alt))
                        ));
                        rest = after;
                        continue;
                    }
                }
                '[' => {
                    if let Some((label, target, after)) = link_parts(rest) {
                        let outer = std::mem::replace(&mut self.in_link, true);
                        let label = self.inline(label);
                        self.in_link = outer;
                        let href = (self.options.link)(target);
                        out.push_str(&format!("<a href=\"{}\">{label}</a>", escape(&href)));
                        rest = after;
                        continue;
                    }
                }
                '*' if rest.starts_with("**") => {
                    let inner = &rest[2..];
                    if !inner.starts_with(' ')
                        && let Some(end) = find_outside_code(inner, "**").filter(|e| *e > 0)
                    {
                        out.push_str(&format!("<strong>{}</strong>", self.inline(&inner[..end])));
                        rest = &inner[end + 2..];
                        continue;
                    }
                }
                '*' => {
                    let inner = &rest[1..];
                    if !inner.starts_with([' ', '*'])
                        && let Some(end) = find_outside_code(inner, "*")
                        && end > 0
                        && !inner[..end].ends_with(' ')
                    {
                        out.push_str(&format!("<em>{}</em>", self.inline(&inner[..end])));
                        rest = &inner[end + 1..];
                        continue;
                    }
                }
                _ => {}
            }
            out.push_str(&escape(&c.to_string()));
            rest = &rest[c.len_utf8()..];
        }
        out
    }

    /// Inline code; when it names an entry (`tex.scan`, or a command with its params after it),
    /// a link to that entry and a mention.
    fn code(&mut self, code: &str) -> String {
        let html = format!("<code>{}</code>", escape(code));
        let name = code.split(|c: char| c.is_whitespace() || c == '{').next().unwrap_or("");
        if !self.options.entries.contains(name) {
            return html;
        }
        let under = self.headings.len().checked_sub(1);
        if !self.mentions.iter().any(|(n, h)| n == name && *h == under) {
            self.mentions.push((name.to_string(), under));
        }
        if self.in_link {
            return html;
        }
        format!("<a class=\"ref\" href=\"#{}\">{html}</a>", escape(name))
    }
}
