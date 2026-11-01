//! The About window: the version and the commit it was built from, penumbra's own licence with
//! both texts, the third-party notices and the trademark line NDI's terms ask for. It opens from
//! the project menu (`project.about`); Escape or its close button closes it.
//!
//! The third-party notices are an HTML page of about 400 KB that `cargo xtask notices` writes
//! (`target/THIRD-PARTY-NOTICES.html` in a checkout, beside the exe in the release zip). The
//! window lists its overview (each licence with the number of crates under it) and opens the
//! page in the browser, so the binary does not carry it.
//!
//! `app.about {notices?}` answers the same on the control channel; `notices: true` opens the
//! page in the browser where this run may start one.
//!
//! Automation ids: `about` (the window), `about.mit` and `about.apache` (the licence texts, folded
//! at first), `about.notices` (the button that opens the page).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use egui::{RichText, Ui};
use serde_json::{Value as Json, json};

use crate::{automation, theme};

/// The file name of the notices page.
const NOTICES: &str = "THIRD-PARTY-NOTICES.html";

/// How many directories above the executable's own the page is looked for: beside the exe in
/// the release zip, `target/` from `target/<profile>/` or `target/<triple>/<profile>/deps`.
const SEARCH_DEPTH: usize = 4;

/// penumbra's licence, as its manifests state it.
pub const LICENCE: &str = env!("CARGO_PKG_LICENSE");

const LICENCE_MIT: &str = include_str!("../../../LICENSE-MIT");
const LICENCE_APACHE: &str = include_str!("../../../LICENSE-APACHE");

/// Where the source is.
pub const REPOSITORY: &str = "https://github.com/alexanderalber/penumbra";

/// Trademarks the app names: NDI's terms ask for their line in an About box. TouchDesigner's
/// trademark is acknowledged in the README.
pub const TRADEMARKS: &[&str] = &["NDI\u{ae} is a registered trademark of Vizrt NDI AB."];

/// The commit the app was built from, when its build knew it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Build {
    /// The short hash.
    pub commit: Option<String>,
    /// The commit's date, `YYYY-MM-DD`.
    pub date: Option<String>,
}

static BUILD: OnceLock<Build> = OnceLock::new();

/// Called once by the app at start with what its build script found out. A build outside a git
/// checkout passes `None` for both.
pub fn set_build(commit: Option<&str>, date: Option<&str>) {
    let clean = |s: Option<&str>| s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    BUILD.set(Build { commit: clean(commit), date: clean(date) }).ok();
}

fn build() -> Build {
    BUILD.get().cloned().unwrap_or_default()
}

/// The copyright line of the MIT text.
fn copyright() -> &'static str {
    LICENCE_MIT.lines().map(str::trim).find(|l| l.starts_with("Copyright")).unwrap_or("")
}

/// One licence of the notices' overview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Licence {
    pub name: String,
    pub crates: u32,
}

/// The notices page beside the executable or a few levels above it, else the checkout's.
pub fn find_notices() -> Result<PathBuf, String> {
    let mut looked = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe.ancestors().skip(1).take(SEARCH_DEPTH + 1) {
            let file = dir.join(NOTICES);
            if file.is_file() {
                return Ok(file);
            }
            looked.push(dir.display().to_string());
        }
    }
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target").join(NOTICES);
    if checkout.is_file() {
        return Ok(std::path::absolute(&checkout).unwrap_or(checkout));
    }
    Err(format!(
        "the third-party notices ({NOTICES}) are not beside the program or above it (looked in {}); \
         a checkout writes them with `cargo xtask notices`",
        if looked.is_empty() { "no directory".into() } else { looked.join(", ") }
    ))
}

/// The overview of the notices page: its lines `<li><a href="#ID">NAME</a> (COUNT)</li>`.
/// Lines of another shape are skipped, so a changed template yields a shorter list.
pub fn overview(html: &str) -> Vec<Licence> {
    let mut out = Vec::new();
    for line in html.lines() {
        let Some(rest) = line.trim().strip_prefix("<li><a href=\"#") else { continue };
        let Some((_, rest)) = rest.split_once("\">") else { continue };
        let Some((name, rest)) = rest.split_once("</a> (") else { continue };
        let Some(count) = rest.strip_suffix(")</li>") else { continue };
        let Ok(crates) = count.trim().parse() else { continue };
        out.push(Licence { name: unescape(name), crates });
    }
    out
}

/// The few entities cargo-about's escaping writes.
fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The notices page and its overview, or why there is none.
fn notices() -> Result<(PathBuf, Vec<Licence>), String> {
    let file = find_notices()?;
    let html = std::fs::read_to_string(&file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    Ok((file, overview(&html)))
}

/// Opens the notices page in the browser.
fn open_notices(file: &Path) -> Result<(), String> {
    crate::help::open_with_system(file)
}

/// What `app.about` answers.
pub fn info() -> Json {
    let build = build();
    let notices = match notices() {
        Ok((file, licences)) => json!({
            "file": file.display().to_string(),
            "url": crate::help::file_url(&file),
            "licences": licences.iter().map(|l| json!({"name": l.name, "crates": l.crates})).collect::<Vec<_>>(),
        }),
        Err(e) => json!({"error": e}),
    };
    json!({
        "name": "penumbra",
        "version": env!("CARGO_PKG_VERSION"),
        "commit": build.commit,
        "commit_date": build.date,
        "licence": LICENCE,
        "copyright": copyright(),
        "repository": REPOSITORY,
        "notices": notices,
        "trademarks": TRADEMARKS,
    })
}

/// The `app.about` command: `{notices?}`; `notices: true` opens the notices page in the browser
/// where `browser` allows it.
pub fn command(params: &Json, browser: bool) -> Result<Json, String> {
    let wanted = match params.get("notices") {
        None | Some(Json::Null) => false,
        Some(Json::Bool(b)) => *b,
        Some(_) => return Err("`notices` must be true or false".into()),
    };
    if let Some(map) = params.as_object()
        && let Some(other) = map.keys().find(|k| k.as_str() != "notices")
    {
        return Err(format!("unknown param `{other}`; app.about takes only `notices`"));
    }
    let mut reply = info();
    let mut opened = false;
    if wanted && browser {
        let file = find_notices()?;
        open_notices(&file)?;
        opened = true;
    }
    if let Some(map) = reply.as_object_mut() {
        map.insert("opened".into(), json!(opened));
    }
    Ok(reply)
}

/// The window's state.
#[derive(Default)]
pub struct About {
    pub open: bool,
    /// The notices, read when the window opens.
    notices: Option<Result<(PathBuf, Vec<Licence>), String>>,
    /// What the last click on the notices button said, when it failed.
    message: Option<String>,
}

impl About {
    /// Opens the window and reads the notices afresh.
    pub fn open(&mut self) {
        self.open = true;
        self.notices = None;
        self.message = None;
    }

    /// Draws the window while it is open. `browser`: this run may start the browser. Draw it
    /// after the other windows, so a dialog on top of it takes Escape first.
    pub fn show(&mut self, ctx: &egui::Context, browser: bool, auto: &mut automation::Registry) {
        if !self.open {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.open = false;
            return;
        }
        let mut open = true;
        let r = egui::Window::new("About penumbra")
            .id(egui::Id::new("about_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(440.0)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                ui.set_width(440.0);
                self.body(ui, browser, auto);
            });
        if let Some(r) = r {
            auto.add("about", r.response.rect, "About penumbra");
        }
        self.open = open;
    }

    fn body(&mut self, ui: &mut Ui, browser: bool, auto: &mut automation::Registry) {
        let build = build();
        ui.label(RichText::new(format!("penumbra {}", env!("CARGO_PKG_VERSION"))).heading());
        let built = match (&build.commit, &build.date) {
            (Some(c), Some(d)) => format!("Built from commit {c} of {d}."),
            (Some(c), None) => format!("Built from commit {c}."),
            _ => "Built outside a git checkout: the commit is unknown.".to_string(),
        };
        ui.label(RichText::new(built).color(theme::TEXT_WEAK));
        ui.hyperlink_to(RichText::new(REPOSITORY).color(theme::TEXT_WEAK), REPOSITORY);

        ui.add_space(6.0);
        ui.label(format!(
            "{}. Licensed under the MIT licence or the Apache licence 2.0, at your choice.",
            copyright()
        ));
        for (id, title, text) in [
            ("about.mit", "MIT licence", LICENCE_MIT),
            ("about.apache", "Apache licence 2.0", LICENCE_APACHE),
        ] {
            let r = egui::CollapsingHeader::new(title).id_salt(id).show(ui, |ui| licence_text(ui, id, text));
            auto.add(id, r.header_response.rect, title);
        }

        ui.add_space(6.0);
        ui.label("penumbra is built from open source crates, each under its own licence:");
        let notices = self.notices.get_or_insert_with(notices);
        match notices {
            Ok((file, licences)) => {
                if !licences.is_empty() {
                    let list = licences
                        .iter()
                        .map(|l| format!("{} ({})", l.name, l.crates))
                        .collect::<Vec<_>>()
                        .join(", ");
                    ui.label(RichText::new(list).color(theme::TEXT_WEAK));
                }
                let b = ui
                    .add_enabled(browser, egui::Button::new("Third-party notices"))
                    .on_hover_text(format!("Opens {} in the browser", file.display()))
                    .on_disabled_hover_text("This run opens no browser");
                auto.add("about.notices", b.rect, "Third-party notices");
                if b.clicked() {
                    self.message = open_notices(file).err();
                }
            }
            Err(e) => {
                ui.label(RichText::new(e.as_str()).color(theme::WARNING));
            }
        }
        if let Some(m) = &self.message {
            ui.label(RichText::new(m.as_str()).color(theme::ERROR));
        }

        ui.add_space(6.0);
        for line in TRADEMARKS {
            ui.label(RichText::new(*line).color(theme::TEXT_WEAK));
        }
    }
}

/// A licence text, scrollable, in the small monospace font.
fn licence_text(ui: &mut Ui, id: &str, text: &str) {
    egui::ScrollArea::vertical().id_salt(id).max_height(220.0).show(ui, |ui| {
        ui.label(RichText::new(text).monospace().small());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_overview_reads_name_and_count_and_skips_the_rest() {
        let html = "<ul>\n  <li><a href=\"#MIT\">MIT License</a> (352)</li>\n  <li><a href=\"#BSD-3-Clause\">BSD 3-Clause &quot;New&quot; License</a> (9)</li>\n  <li><a href=\"#x\">broken</a> (many)</li>\n<li>other</li>\n</ul>";
        assert_eq!(
            overview(html),
            vec![
                Licence { name: "MIT License".into(), crates: 352 },
                Licence { name: "BSD 3-Clause \"New\" License".into(), crates: 9 },
            ]
        );
        assert!(overview("").is_empty());
    }

    #[test]
    fn the_command_names_version_licence_and_the_ndi_trademark() {
        let reply = command(&json!({}), false).unwrap();
        assert_eq!(reply["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(reply["licence"], "MIT OR Apache-2.0");
        assert!(reply["copyright"].as_str().unwrap().starts_with("Copyright"), "{reply}");
        assert!(reply["trademarks"][0].as_str().unwrap().contains("Vizrt NDI AB"), "{reply}");
        assert_eq!(reply["opened"], false);
        // Either the page with its overview or why there is none.
        let notices = &reply["notices"];
        assert!(notices["file"].is_string() || notices["error"].is_string(), "{reply}");
        // Headless runs open nothing, even when asked.
        assert_eq!(command(&json!({"notices": true}), false).unwrap()["opened"], false);
        assert!(command(&json!({"notices": "yes"}), false).is_err());
        assert!(command(&json!({"notice": true}), false).is_err());
    }
}
