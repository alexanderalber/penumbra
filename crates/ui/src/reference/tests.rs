use std::collections::{BTreeMap, BTreeSet};

use super::html::{self, Guide, Sources};
use super::markdown::{self, Options, escape, slug};

fn render(md: &str) -> markdown::Rendered {
    let entries: BTreeSet<String> = ["tex.scan", "scan.capture"].iter().map(|s| s.to_string()).collect();
    let link = |t: &str| format!("LINK({t})");
    markdown::render(md, &Options { id_prefix: "g-", shift: 1, entries: &entries, link: &link })
}

#[test]
fn escaping_and_slugs() {
    assert_eq!(escape(r#"<a href="x">&</a>"#), "&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;");
    assert_eq!(slug("5. Faces and objects: `scan.scene`"), "5-faces-and-objects-scanscene");
    assert_eq!(slug("Who is editing"), "who-is-editing");
    assert_eq!(markdown::plain("A [link `x`](a.md) and **bold** \\| pipe"), "A link x and bold | pipe");
}

#[test]
fn headings_paragraphs_and_inline_markup() {
    let r = render(
        "# Title\n\nSome *em* and **strong** text\nover two lines with `<code>` and a [link](other.md#x).\n\n## Same\n\n## Same\n",
    );
    assert!(r.html.contains("<h2 id=\"g-title\">Title</h2>"), "{}", r.html);
    assert!(
        r.html.contains(
            "<p>Some <em>em</em> and <strong>strong</strong> text over two lines with <code>&lt;code&gt;</code> \
             and a <a href=\"LINK(other.md#x)\">link</a>.</p>"
        ),
        "{}",
        r.html
    );
    // Repeated headings are numbered, as GitHub does.
    assert!(r.html.contains("id=\"g-same\"") && r.html.contains("id=\"g-same-1\""), "{}", r.html);
    assert_eq!(r.headings.len(), 3);
    // A lone star, and stars around spaces, stay stars.
    let r = render("2 * 3 and a * b *\n");
    assert_eq!(r.html, "<p>2 * 3 and a * b *</p>\n");
}

#[test]
fn lists_nest_and_continue() {
    let r = render(
        "1. First item\n   continued.\n2. Second:\n   - nested one\n     wrapped\n   - nested two\n\n   A paragraph of the second.\n\nAfter.\n",
    );
    let html = r.html;
    assert!(html.starts_with("<ol>\n<li>"), "{html}");
    assert!(html.contains("<p>First item continued.</p>"), "{html}");
    assert!(html.contains("<ul>\n<li>nested one wrapped\n</li>\n<li>nested two\n</li>\n</ul>"), "{html}");
    assert!(html.contains("<p>A paragraph of the second.</p>\n</li>\n</ol>\n<p>After.</p>"), "{html}");
    let r = render("- a\n- b\n");
    assert_eq!(r.html, "<ul>\n<li>a\n</li>\n<li>b\n</li>\n</ul>\n");
    let r = render("3. three\n4. four\n");
    assert!(r.html.starts_with("<ol start=\"3\">"), "{}", r.html);
}

#[test]
fn tables_code_and_images() {
    let r = render(
        "| kind | in WGSL |\n|---|---|\n| `choice(a\\|b)` | x \\| y |\n\n```wgsl\nlet a = 1 < 2;\n```\n\n![Alt *text*](../assets/s.png)\n",
    );
    let html = r.html;
    assert!(html.contains("<thead><tr><th>kind</th><th>in WGSL</th></tr></thead>"), "{html}");
    assert!(html.contains("<td><code>choice(a|b)</code></td><td>x | y</td>"), "{html}");
    assert!(html.contains("<pre><code class=\"lang-wgsl\">let a = 1 &lt; 2;\n</code></pre>"), "{html}");
    assert!(html.contains("<img src=\"LINK(../assets/s.png)\" alt=\"Alt text\">"), "{html}");
}

#[test]
fn code_naming_an_entry_links_and_counts_as_a_mention() {
    let r = render(
        "Intro `tex.scan`.\n\n## Capture\n\nRun `scan.capture {node}`, not `tex.other`; [see `tex.scan`](x.md).\n",
    );
    assert!(r.html.contains("<a class=\"ref\" href=\"#tex.scan\"><code>tex.scan</code></a>"), "{}", r.html);
    assert!(r.html.contains("<a class=\"ref\" href=\"#scan.capture\"><code>scan.capture {node}</code></a>"));
    assert!(r.html.contains("<code>tex.other</code>") && !r.html.contains("#tex.other"));
    // No link inside a link.
    assert!(r.html.contains("<a href=\"LINK(x.md)\">see <code>tex.scan</code></a>"), "{}", r.html);
    assert_eq!(
        r.mentions,
        vec![
            ("tex.scan".to_string(), None),
            ("scan.capture".to_string(), Some(0)),
            ("tex.scan".to_string(), Some(0))
        ]
    );
}

fn sample_page() -> String {
    let types = penumbra_ops::builtin_types().unwrap();
    let guides = vec![
        Guide {
            stem: "scanning".into(),
            text: "# Scanning: projecting onto objects\n\nSee [concepts](concepts.md#bindings), \
                   [the reference](reference/nodes.md#texscan), [commands](reference/commands.md) and \
                   [below](#faces).\n\n## Faces\n\n`tex.scan` and `node.set {path}`.\n"
                .into(),
        },
        Guide {
            stem: "concepts".into(),
            text: "# Concepts\n\n## Bindings\n\nText with <angle> & ampersand.\n".into(),
        },
    ];
    let mut pages = BTreeMap::new();
    pages.insert("tex.noise".to_string(), "Longer text, see [scanning](../scanning.md#faces).\n".to_string());
    let commands = vec![
        ("ping".to_string(), "Is it alive".to_string()),
        ("node.set".to_string(), "Set params; {path, params}".to_string()),
    ];
    let sources = Sources {
        readme: "# Docs\n\n- [Concepts](concepts.md)\n- [Scanning](scanning.md)\n",
        guides: &guides,
        node_pages: &pages,
        commands: &commands,
    };
    html::page(&types, &sources).unwrap()
}

#[test]
fn the_page_has_an_entry_per_kind_and_command() {
    let types = penumbra_ops::builtin_types().unwrap();
    let page = sample_page();
    for kind in types.kinds() {
        assert!(
            page.contains(&format!("<details class=\"entry node\" id=\"{kind}\">")),
            "no entry for {kind}"
        );
    }
    for id in ["ping", "node.set"] {
        assert!(
            page.contains(&format!("<details class=\"entry command\" id=\"{id}\">")),
            "no entry for {id}"
        );
    }
    assert!(page.contains("<details class=\"entry guide\" id=\"scanning\">"));
    // Guides in the README's order.
    assert!(page.find("id=\"concepts\"").unwrap() < page.find("id=\"scanning\"").unwrap());
    // Self-contained: nothing loaded from elsewhere.
    for outside in ["<link", "src=\"http", "@import", "url("] {
        assert!(!page.contains(outside), "the page loads {outside}");
    }
}

#[test]
fn links_between_documents_become_anchors() {
    let page = sample_page();
    for link in [
        "href=\"#concepts-bindings\"",
        "href=\"#tex.scan\"",
        "href=\"#commands\"",
        "href=\"#scanning-faces\"",
    ] {
        assert!(page.contains(link), "missing {link}");
    }
    // From docs/nodes/, a level down.
    let noise = page.split("id=\"tex.noise\"").nth(1).unwrap().split("</details>").next().unwrap();
    assert!(noise.contains("<div class=\"more\">") && noise.contains("href=\"#scanning-faces\""), "{noise}");
    assert!(page.contains("Text with &lt;angle&gt; &amp; ampersand."));
}

#[test]
fn entries_list_where_the_guides_mention_them() {
    let page = sample_page();
    let scan = page.split("id=\"tex.scan\"").nth(1).unwrap().split("</details>").next().unwrap();
    assert!(scan.contains("Mentioned in: <a href=\"#scanning-faces\">Scanning \u{203a} Faces</a>"), "{scan}");
    let set = page.split("id=\"node.set\"").nth(1).unwrap().split("</details>").next().unwrap();
    assert!(set.contains("<a href=\"#scanning-faces\">"), "{set}");
}

#[test]
fn params_show_their_sections_and_choices() {
    let page = sample_page();
    // Some built-in kind has a folded section and a choice with documented entries.
    assert!(page.contains("<tr class=\"section\"><th colspan=\"5\">more</th></tr>"));
    assert!(page.contains("<ul><li><code>"));
}

#[test]
fn ids_used_twice_are_an_error() {
    let types = penumbra_ops::builtin_types().unwrap();
    let guides = vec![Guide { stem: "tex.noise".into(), text: "# Clash\n".into() }];
    let sources = Sources { readme: "", guides: &guides, node_pages: &BTreeMap::new(), commands: &[] };
    let err = html::page(&types, &sources).unwrap_err();
    assert!(err.contains("tex.noise"), "{err}");
}
