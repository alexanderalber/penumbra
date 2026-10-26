//! `docs/penumbra.html`: the whole documentation in one self-contained page for the browser,
//! which the `?` on a node opens at that node's entry. The guide (`docs/*.md`), every node kind
//! and every command, each an entry that folds open, with a filter box. No external resources:
//! it must work from the release zip, offline, over `file://`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

use penumbra_graph::{NodeDesc, NodeTypes};

use super::markdown::{self, Options, escape};
use crate::theme::kind_name;

/// A hand-written document: its file stem (`scanning`) and its markdown.
pub struct Guide {
    pub stem: String,
    pub text: String,
}

/// What the page is made from besides the node types.
pub struct Sources<'a> {
    /// `docs/README.md`, the introduction with the list of guides.
    pub readme: &'a str,
    /// The other `docs/*.md`.
    pub guides: &'a [Guide],
    /// Longer texts for single node kinds, `docs/nodes/<kind>.md`, by kind.
    pub node_pages: &'a BTreeMap<String, String>,
    /// Every command with its summary, in the order to list them.
    pub commands: &'a [(String, String)],
}

/// The id of the section with every node kind.
const NODES: &str = "nodes";
/// The id of the section with every command.
const COMMANDS: &str = "commands";
/// The id of the introduction (`docs/README.md`).
const INTRO: &str = "intro";

fn family_id(family: &str) -> String {
    format!("family-{family}")
}

fn group_id(group: &str) -> String {
    format!("group-{group}")
}

fn command_group(id: &str) -> &str {
    id.split_once('.').map_or("general", |(g, _)| g)
}

/// Where a link in a document goes in the page. `base` is the document's directory below
/// `docs/` (empty, or `nodes/`), `prefix` its heading ids' prefix for links within it.
fn rewrite(
    target: &str,
    base: &str,
    prefix: &str,
    stems: &BTreeSet<String>,
    kinds: &HashMap<String, String>,
) -> String {
    if target.contains("://") || target.starts_with("mailto:") {
        return target.to_string();
    }
    let (path, fragment) = target.split_once('#').unwrap_or((target, ""));
    if path.is_empty() {
        return format!("#{prefix}{fragment}");
    }
    // Resolved against `docs/`.
    let mut resolved = format!("{base}{path}");
    while let Some(i) = resolved.find("/../") {
        let start = resolved[..i].rfind('/').map_or(0, |s| s + 1);
        resolved.replace_range(start..i + 4, "");
    }
    if let Some(rest) = resolved.strip_prefix("nodes/../") {
        resolved = rest.to_string();
    }
    let Some(stem) = resolved.strip_suffix(".md") else { return resolved };
    match stem {
        "README" => format!("#{INTRO}"),
        "reference/nodes" => match kinds.get(fragment) {
            Some(kind) => format!("#{kind}"),
            None if fragment.is_empty() => format!("#{NODES}"),
            None => format!("#{}", family_id(fragment)),
        },
        "reference/commands" if fragment.is_empty() => format!("#{COMMANDS}"),
        "reference/commands" => format!("#{}", group_id(fragment)),
        s if stems.contains(s) && fragment.is_empty() => format!("#{s}"),
        s if stems.contains(s) => format!("#{s}-{fragment}"),
        _ => resolved,
    }
}

/// Where a guide mentions an entry: the text of the link and its target id.
struct Mention {
    label: String,
    id: String,
}

/// The guides in the order the README lists them, any others after them by name.
fn ordered<'a>(readme: &str, guides: &'a [Guide]) -> Vec<&'a Guide> {
    let mut out: Vec<&Guide> = Vec::new();
    for piece in readme.split("](").skip(1) {
        let target = piece.split(')').next().unwrap_or("");
        let stem = target.split('#').next().unwrap_or("").trim_end_matches(".md");
        if let Some(g) = guides.iter().find(|g| g.stem == stem)
            && !out.iter().any(|o| o.stem == g.stem)
        {
            out.push(g);
        }
    }
    let mut rest: Vec<&Guide> = guides.iter().filter(|g| !out.iter().any(|o| o.stem == g.stem)).collect();
    rest.sort_by(|a, b| a.stem.cmp(&b.stem));
    out.extend(rest);
    out
}

/// A guide's title, the first heading; its short form ends before a colon (`Scanning`).
fn title(guide: &Guide) -> (String, String) {
    let full = guide
        .text
        .lines()
        .find_map(|l| l.strip_prefix("# "))
        .map(markdown::plain)
        .unwrap_or_else(|| guide.stem.clone());
    let short = full.split(':').next().unwrap_or(&full).trim().to_string();
    (full, short)
}

/// The page, or why it cannot be made (two entries or headings with the same id).
pub fn page(types: &NodeTypes, sources: &Sources<'_>) -> Result<String, String> {
    let sections = super::sections(types);
    let mut entries: BTreeSet<String> = types.kinds().map(str::to_string).collect();
    entries.extend(sources.commands.iter().map(|(id, _)| id.clone()));
    let stems: BTreeSet<String> = sources.guides.iter().map(|g| g.stem.clone()).collect();
    // nodes.md's anchors are the kinds without their dot.
    let kinds: HashMap<String, String> = types.kinds().map(|k| (k.replace('.', ""), k.to_string())).collect();

    let mut ids: Vec<String> = vec![INTRO.into(), NODES.into(), COMMANDS.into(), "guides".into()];
    let mut mentions: BTreeMap<String, Vec<Mention>> = BTreeMap::new();

    // The introduction and the guides first: they tell which entries they mention.
    let link = |target: &str| rewrite(target, "", &format!("{INTRO}-"), &stems, &kinds);
    let intro = markdown::render(
        sources.readme,
        &Options { id_prefix: &format!("{INTRO}-"), shift: 0, entries: &entries, link: &link },
    );
    ids.extend(intro.headings.iter().map(|h| h.id.clone()));
    let mut guides_html = String::new();
    let mut toc_guides = Vec::new();
    for guide in ordered(sources.readme, sources.guides) {
        let prefix = format!("{}-", guide.stem);
        let link = |target: &str| rewrite(target, "", &prefix, &stems, &kinds);
        let r = markdown::render(
            &guide.text,
            &Options { id_prefix: &prefix, shift: 1, entries: &entries, link: &link },
        );
        let (full, short) = title(guide);
        ids.push(guide.stem.clone());
        ids.extend(r.headings.iter().map(|h| h.id.clone()));
        for (name, under) in &r.mentions {
            let heading = under.and_then(|i| r.headings.get(i)).filter(|h| h.level > 1);
            let (label, id) = match heading {
                Some(h) => (format!("{short} \u{203a} {}", h.text), h.id.clone()),
                None => (short.clone(), guide.stem.clone()),
            };
            let list = mentions.entry(name.clone()).or_default();
            if !list.iter().any(|m| m.id == id) {
                list.push(Mention { label, id });
            }
        }
        // The first heading is the entry's own title.
        let body = match r.html.split_once("</h2>\n") {
            Some((head, rest)) if head.starts_with("<h2") => rest.to_string(),
            _ => r.html,
        };
        let _ = write!(
            guides_html,
            "<details class=\"entry guide\" id=\"{id}\"><summary><span class=\"name\">{t}</span></summary>\n\
             <div class=\"body\">\n{body}</div></details>\n",
            id = escape(&guide.stem),
            t = escape(&full),
        );
        toc_guides.push(format!("<a href=\"#{}\">{}</a>", escape(&guide.stem), escape(&short)));
    }

    // The node kinds by family.
    let plain_link = |target: &str| rewrite(target, "", "", &stems, &kinds);
    let inline_options = Options { id_prefix: "", shift: 0, entries: &entries, link: &plain_link };
    let inline = |text: &str| markdown::inline(text, &inline_options);
    let mut nodes_html = String::new();
    for section in &sections {
        ids.push(family_id(&section.id));
        let _ = write!(
            nodes_html,
            "<div class=\"group\" id=\"{}\">
<h3>{}</h3>
",
            family_id(&section.id),
            escape(&section.title)
        );
        if section.id == super::PORTS {
            let _ = writeln!(nodes_html, "<p>{}</p>", inline(super::PORTS_INTRO));
        }
        let list: Vec<String> =
            section.descs.iter().map(|d| format!("<a href=\"#{k}\">{k}</a>", k = escape(&d.kind))).collect();
        let _ = writeln!(nodes_html, "<p class=\"kinds\">{}</p>", list.join(" "));
        for desc in &section.descs {
            ids.push(desc.kind.clone());
            let page = sources.node_pages.get(&desc.kind).map(|text| {
                let prefix = format!("{}-", desc.kind);
                let link = |target: &str| rewrite(target, "nodes/", &prefix, &stems, &kinds);
                let r = markdown::render(
                    text,
                    &Options { id_prefix: &prefix, shift: 3, entries: &entries, link: &link },
                );
                ids.extend(r.headings.iter().map(|h| h.id.clone()));
                r.html
            });
            node_entry(&mut nodes_html, desc, &inline, page.as_deref(), mentions.get(&desc.kind));
        }
        nodes_html.push_str("</div>\n");
    }

    // The commands by group.
    let mut commands_html = String::new();
    let mut group = "";
    let mut toc_groups = Vec::new();
    for (id, summary) in sources.commands {
        let g = command_group(id);
        if g != group {
            if !group.is_empty() {
                commands_html.push_str("</div>\n");
            }
            group = g;
            ids.push(group_id(g));
            toc_groups.push(format!("<a href=\"#{}\">{}</a>", group_id(g), escape(g)));
            let _ = write!(
                commands_html,
                "<div class=\"group\" id=\"{}\">\n<h3>{}</h3>\n",
                group_id(g),
                escape(g)
            );
        }
        ids.push(id.clone());
        // The summary up to its params (`; {a, b?}`) in the folded line.
        let short = summary.split("; {").next().unwrap_or(summary);
        let _ = write!(
            commands_html,
            "<details class=\"entry command\" id=\"{e}\"><summary><code class=\"name\">{e}</code>\
             <span class=\"sum\">{s}</span></summary>\n<div class=\"body\">\n<p>{full}</p>\n",
            e = escape(id),
            s = inline(short),
            full = inline(summary),
        );
        mentioned(&mut commands_html, mentions.get(id));
        commands_html.push_str("</div></details>\n");
    }
    if !group.is_empty() {
        commands_html.push_str("</div>\n");
    }

    let mut seen = BTreeSet::new();
    let twice: BTreeSet<&String> = ids.iter().filter(|id| !seen.insert(*id)).collect();
    if !twice.is_empty() {
        let list: Vec<&str> = twice.iter().map(|s| s.as_str()).collect();
        return Err(format!("ids used twice in the page: {}", list.join(", ")));
    }

    let toc_families: Vec<String> = sections
        .iter()
        .map(|s| format!("<a href=\"#{}\">{}</a>", family_id(&s.id), escape(&s.title)))
        .collect();
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n");
    out.push_str(
        "<!-- Generated by `cargo xtask docs` from the code and docs/*.md; do not edit by hand. -->\n",
    );
    out.push_str("<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<title>penumbra documentation</title>\n<style>\n");
    out.push_str(CSS);
    out.push_str("</style>\n</head>\n<body>\n");
    out.push_str(
        "<header class=\"bar\"><a class=\"home\" href=\"#\">penumbra</a>\
         <input id=\"filter\" type=\"search\" placeholder=\"Filter: a node, a command, any words\" autocomplete=\"off\" spellcheck=\"false\">\
         <span id=\"count\"></span>\
         <button id=\"expand\" type=\"button\">Expand all</button>\
         <button id=\"collapse\" type=\"button\">Collapse all</button></header>\n<main>\n",
    );
    let _ = write!(out, "<section id=\"{INTRO}\">\n{}</section>\n", intro.html);
    let _ = write!(
        out,
        "<nav class=\"toc\">\n<p><b>Guides</b> {}</p>\n<p><b><a href=\"#{NODES}\">Nodes</a></b> {}</p>\n\
         <p><b><a href=\"#{COMMANDS}\">Commands</a></b> {}</p>\n</nav>\n",
        toc_guides.join(" "),
        toc_families.join(" "),
        toc_groups.join(" "),
    );
    let _ = write!(out, "<section id=\"guides\">\n<h2>Guides</h2>\n{guides_html}</section>\n");
    let _ = write!(
        out,
        "<section id=\"{NODES}\">\n<h2>Node reference</h2>\n<p>Every built-in node kind with its inputs, outputs and \
         params, by family, and the component ports of all families in a section of their own. A param under a heading such as <i>more</i> sits in that folded section of the param panel. \
         A soft range limits only the slider; typed values outside it are kept.</p>\n{nodes_html}</section>\n"
    );
    let _ = write!(
        out,
        "<section id=\"{COMMANDS}\">\n<h2>Command reference</h2>\n<p>Every command of the running app, as \
         <code>penumbra-cli ctl &lt;command&gt;</code> or a control-channel request sends it (see \
         <a href=\"#control\">Control</a>). <code>{{a, b?}}</code> lists the params, <code>?</code> marks optional \
         ones.</p>\n{commands_html}</section>\n"
    );
    out.push_str("</main>\n<script>\n");
    out.push_str(SCRIPT);
    out.push_str("</script>\n</body>\n</html>\n");
    Ok(out)
}

fn mentioned(out: &mut String, mentions: Option<&Vec<Mention>>) {
    let Some(list) = mentions.filter(|l| !l.is_empty()) else { return };
    let links: Vec<String> =
        list.iter().map(|m| format!("<a href=\"#{}\">{}</a>", escape(&m.id), escape(&m.label))).collect();
    let _ = writeln!(out, "<p class=\"mentions\">Mentioned in: {}</p>", links.join(", "));
}

fn node_entry(
    out: &mut String,
    desc: &NodeDesc,
    inline: &dyn Fn(&str) -> String,
    page: Option<&str>,
    mentions: Option<&Vec<Mention>>,
) {
    let _ = write!(
        out,
        "<details class=\"entry node\" id=\"{k}\"><summary><code class=\"name\">{k}</code>\
         <span class=\"sum\">{s}</span></summary>\n<div class=\"body\">\n<p>{s}</p>\n",
        k = escape(&desc.kind),
        s = inline(&desc.summary),
    );
    let notes = super::node_notes(desc);
    if !notes.is_empty() {
        let _ = writeln!(out, "<p class=\"note\">{}</p>", inline(&notes.join(" ")));
    }
    let port = |name: &str, kind, doc: &str, extra: &str| {
        let doc = if doc.is_empty() { String::new() } else { format!(": {}", inline(doc)) };
        format!(
            "<li><code>{}</code> <span class=\"kind\">{}</span>{doc}{extra}</li>",
            escape(name),
            escape(kind_name(kind)),
        )
    };
    out.push_str("<h4>Inputs</h4>\n");
    if desc.inputs.is_empty() {
        out.push_str("<p>none</p>\n");
    } else {
        out.push_str("<ul>\n");
        for i in desc.listed_inputs() {
            let extra = match (i.previous, i.run) {
                (true, _) => " Sees the previous frame, so it may close a loop.".to_string(),
                (false, Some(len)) => format!(" {}.", super::run_note(len)),
                (false, None) => String::new(),
            };
            let _ = writeln!(out, "{}", port(&i.name, i.kind, &i.doc, &extra));
        }
        out.push_str("</ul>\n");
    }
    out.push_str("<h4>Outputs</h4>\n");
    if desc.outputs.is_empty() {
        out.push_str("<p>none</p>\n");
    } else {
        out.push_str("<ul>\n");
        for o in &desc.outputs {
            let _ = writeln!(out, "{}", port(&o.name, o.kind, &o.doc, ""));
        }
        out.push_str("</ul>\n");
    }
    if !desc.params.is_empty() {
        out.push_str(
            "<h4>Params</h4>\n<table class=\"params\">\n<thead><tr><th>param</th><th>type</th><th>default</th>\
             <th>range</th><th>meaning</th></tr></thead>\n<tbody>\n",
        );
        // The main rows first, then each folded section under its name, as the panel shows them.
        let mut groups: Vec<&str> = Vec::new();
        for p in &desc.params {
            if !groups.contains(&p.group.as_str()) {
                groups.push(&p.group);
            }
        }
        groups.sort_by_key(|g| !g.is_empty());
        for g in groups {
            if !g.is_empty() {
                let _ = writeln!(out, "<tr class=\"section\"><th colspan=\"5\">{}</th></tr>", escape(g));
            }
            for p in desc.params.iter().filter(|p| p.group == g) {
                let mut meaning = String::new();
                if !p.doc.is_empty() {
                    meaning.push_str(&inline(&p.doc));
                }
                let mut items: Vec<String> = Vec::new();
                if let penumbra_core::ParamKind::Choice(choices) = &p.kind {
                    for c in choices {
                        if let Some(d) = p.doc_of_choice(c) {
                            items.push(format!("<code>{}</code>: {}", escape(c), inline(d)));
                        }
                    }
                }
                items.extend(p.special_lines().iter().map(|s| inline(s)));
                if !items.is_empty() {
                    let _ = write!(
                        meaning,
                        "<ul>{}</ul>",
                        items.iter().map(|i| format!("<li>{i}</li>")).collect::<String>()
                    );
                }
                let _ = writeln!(
                    out,
                    "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{meaning}</td></tr>",
                    escape(&p.name),
                    escape(&p.type_label()),
                    inline(&super::param_default(p)),
                    escape(&super::param_range(p)),
                );
            }
        }
        out.push_str("</tbody>\n</table>\n");
    }
    if let Some(page) = page {
        let _ = write!(out, "<div class=\"more\">\n{page}</div>\n");
    }
    mentioned(out, mentions);
    out.push_str("</div></details>\n");
}

const CSS: &str = r#":root {
  color-scheme: light dark;
  --bg: #fbfbfa; --fg: #1d1d1f; --weak: #66666b; --line: #d9d9dc; --panel: #f0f0f2;
  --accent: #6b4fd8; --code: #ecebf3; --mark: #fff2b3;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #18181b; --fg: #e4e4e7; --weak: #9a9aa3; --line: #34343a; --panel: #222227;
    --accent: #a993ff; --code: #2a2933; --mark: #4a4220;
  }
}
* { box-sizing: border-box; }
html { scroll-padding-top: 4rem; }
body { margin: 0; background: var(--bg); color: var(--fg);
  font: 15px/1.5 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; }
main { max-width: 64rem; margin: 0 auto; padding: 0 1rem 40vh; }
a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; }
code, pre { font-family: ui-monospace, Consolas, "Cascadia Mono", monospace; font-size: 0.9em; }
code { background: var(--code); padding: 0 0.25em; border-radius: 3px; }
pre { background: var(--code); padding: 0.6em 0.8em; overflow-x: auto; border-radius: 4px; }
pre code { background: none; padding: 0; }
img { max-width: 100%; }
table { border-collapse: collapse; margin: 0.5em 0; width: 100%; }
th, td { border: 1px solid var(--line); padding: 0.25em 0.5em; text-align: left; vertical-align: top; }
thead th { background: var(--panel); }
td ul { margin: 0.2em 0; padding-left: 1.2em; }
tr.section th { background: var(--panel); font-style: italic; font-weight: normal; }
.bar { position: sticky; top: 0; z-index: 1; display: flex; gap: 0.5rem; align-items: center;
  padding: 0.5rem 1rem; background: var(--panel); border-bottom: 1px solid var(--line); }
.bar .home { font-weight: bold; color: var(--fg); }
.bar input { flex: 1; max-width: 30rem; font: inherit; padding: 0.2em 0.5em; color: var(--fg);
  background: var(--bg); border: 1px solid var(--line); border-radius: 4px; }
.bar button { font: inherit; color: var(--fg); background: var(--bg); border: 1px solid var(--line);
  border-radius: 4px; padding: 0.2em 0.6em; cursor: pointer; }
#count { color: var(--weak); font-size: 0.9em; min-width: 6em; }
.toc p { margin: 0.3em 0; }
.toc a, .kinds a { margin-right: 0.6em; white-space: nowrap; }
.kinds { font-size: 0.9em; }
h2 { margin-top: 2em; border-bottom: 1px solid var(--line); }
details.entry { border: 1px solid var(--line); border-radius: 4px; margin: 0.3em 0; background: var(--bg); }
details.entry > summary { cursor: pointer; padding: 0.3em 0.6em; display: flex; gap: 0.8em; align-items: baseline;
  list-style-position: inside; }
details.entry > summary::-webkit-details-marker { margin-right: 0.3em; }
details.entry > summary .name { flex: none; font-weight: 600; }
details.entry > summary .sum { color: var(--weak); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
details.entry[open] > summary { background: var(--panel); }
details.entry[open] > summary .sum { display: none; }
table.params td:nth-child(4) { white-space: nowrap; }
details.entry > .body { padding: 0 0.8em 0.6em; }
details.guide > summary .name { font-size: 1.1em; }
details.target { outline: 2px solid var(--accent); }
.note, .mentions, .kind { color: var(--weak); }
.kind { font-size: 0.9em; }
.mentions { font-size: 0.9em; }
.more { border-top: 1px dashed var(--line); margin-top: 0.6em; }
[hidden] { display: none !important; }
"#;

const SCRIPT: &str = r#"(function () {
  "use strict";
  var filter = document.getElementById("filter");
  var count = document.getElementById("count");
  var entries = Array.prototype.slice.call(document.querySelectorAll("details.entry"));
  var groups = Array.prototype.slice.call(document.querySelectorAll(".group, #guides"));
  var intro = [document.getElementById("intro"), document.querySelector("nav.toc")];
  var texts = entries.map(function (e) { return (e.id + " " + e.textContent).toLowerCase(); });

  function apply() {
    var terms = filter.value.toLowerCase().split(/\s+/).filter(Boolean);
    var shown = 0;
    entries.forEach(function (e, i) {
      var hit = terms.every(function (t) { return texts[i].indexOf(t) >= 0; });
      e.hidden = terms.length > 0 && !hit;
      if (terms.length > 0) { e.open = hit; }
      if (hit) { shown += 1; }
    });
    groups.forEach(function (g) {
      g.hidden = terms.length > 0 && !g.querySelector("details.entry:not([hidden])");
    });
    // The introduction and the contents make way for what was found.
    intro.forEach(function (el) { el.hidden = terms.length > 0; });
    count.textContent = terms.length > 0 ? shown + " found" : "";
  }

  // The entry or heading the address names: unfolded, shown and scrolled to.
  function reveal() {
    var id = "";
    try { id = decodeURIComponent(location.hash.slice(1)); } catch (err) { id = location.hash.slice(1); }
    if (!id) { return; }
    var el = document.getElementById(id);
    if (!el) { return; }
    if (filter.value) { filter.value = ""; apply(); }
    for (var p = el; p; p = p.parentElement) {
      if (p.tagName === "DETAILS") { p.open = true; }
    }
    entries.forEach(function (e) { e.classList.remove("target"); });
    if (el.tagName === "DETAILS") { el.classList.add("target"); }
    el.scrollIntoView({ block: "start" });
  }

  filter.addEventListener("input", apply);
  filter.addEventListener("keydown", function (ev) {
    if (ev.key === "Escape") { filter.value = ""; apply(); }
    if (ev.key === "Enter") {
      var first = entries.filter(function (e) { return !e.hidden; })[0];
      if (first) { first.scrollIntoView({ block: "start" }); }
    }
  });
  document.getElementById("expand").addEventListener("click", function () {
    entries.forEach(function (e) { if (!e.hidden) { e.open = true; } });
  });
  document.getElementById("collapse").addEventListener("click", function () {
    entries.forEach(function (e) { e.open = false; });
  });
  window.addEventListener("hashchange", reveal);
  reveal();
})();
"#;
