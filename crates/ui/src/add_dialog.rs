//! The "add node" dialog, after TouchDesigner's OP Create Dialog: a search field on top, a tab
//! per group of node families below it, all types of the active group in columns (each family
//! from a new column in its colour, alphabetical, top to bottom, then the next column), and the
//! summary of the type under the pointer at the bottom. Small families share a tab (nine tabs
//! were too many); the colours tell them apart, and a heading over each family's columns says
//! what it holds, since the colours alone left one guessing which block was which.
//!
//! Within a family the common types come first, the family's component ports after them behind a
//! line. The live inputs (`in`) and, in `tex`, the scan workflow (`scan and shapes`) follow in
//! columns of their own under a heading ([`penumbra_graph::Block`]), so the types placed by hand
//! every day are not spread out between them. The content library has a tab of its own,
//! `library`, in the components' colour. It shows one shelf at a time, because nobody reads
//! through dozens of names. The shelves come from the components' own `shelf` param, one or two
//! levels; this file knows no topic. The user's and the project's components are all on `mine`
//! as well. Under the pointer the foot of the tab shows the component's `about` line and its
//! picture, if it has one.
//!
//! Typing does not hide types: those whose name does not contain the text are greyed out, so
//! the grid keeps its shape and the eye its places. Spaces, underscores and dots do not count.
//! The library tab is the exception: there typing shows the matches of all shelves in one list,
//! found by name, shelf or `about` line, since greying one shelf would not find the others.
//! A type is also found by a tag that starts with the text: a TouchDesigner name (`chop to`
//! finds `tex.from_chan`) or a file name extension it reads (`fbx` finds `geo.file`). The
//! summary names the tag. Families with a match are marked in their tab (bright text, a bar
//! underneath), the others dimmed. Enter takes the first match of the active tab: the type
//! named exactly so, then one whose name starts with the text, then any other found by its
//! name, then one found by a tag. Tab switches to the next tab, Shift+Tab to the one before;
//! while searching, both pass over tabs without a match. A TouchDesigner family word in the
//! text (`noise top`, `lfo chop`) searches that family alone for the rest of the text, when the
//! whole text finds nothing, and opens its tab. Otherwise the dialog never switches the tab by
//! itself. Escape or a click outside closes. Opened by double-click on empty canvas or Tab in
//! the editor, and by "Replace node..." in the node menu, where choosing replaces that node.
//!
//! The component ports (`tex.in`, `chan.out`, ...) are greyed out and cannot be chosen at the
//! top level of the project: there they have no component whose sockets they would be.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{
    Align2, Color32, CornerRadius, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};
use penumbra_graph::{Block, NodeTypes};

use crate::icons::{self, Icon};
use crate::{automation, theme};

/// Families always offered, by tab and in this order, even before they have node types; others
/// found in the registry follow alphabetically, one tab each.
const GROUPS: [&[&str]; 7] = [
    &["tex"],
    &["chan"],
    &["audio"],
    &["table"],
    &["geo", "mat", "light", "view", "vol"],
    &["out"],
    &["script", "comp", "ui"],
];
/// The tab of the component files on offer: penumbra's library, the user's and the project's
/// `components/`. Not a family of node types; it takes the colour of `comp`.
pub const LIBRARY: &str = "library";
/// Names of the tabs holding several families, by their first family; a tab of one family is named
/// by it.
const NAMES: [(&str, &str); 2] = [("geo", "rendering"), ("script", "system")];
/// The headings over the families of a shared tab; a family missing here is headed by its name.
const HEADINGS: [(&str, &str); 8] = [
    ("geo", "geometry"),
    ("mat", "materials"),
    ("light", "lights"),
    ("view", "cameras"),
    ("vol", "volumes"),
    ("script", "scripts"),
    ("comp", "components"),
    ("ui", "notes"),
];
/// Height of the heading row.
const HEADING: f32 = 18.0;
/// Types per column before the next column starts.
const ROWS: usize = 12;
const CELL: egui::Vec2 = vec2(150.0, 22.0);
const MIN_WIDTH: f32 = 4.0 * 150.0;
/// The shelf of everything that is the user's or the project's, whatever its own shelf says.
const MINE: &str = "mine";
/// The shelf of components that name none, and of those beside a second level.
const OTHER: &str = "other";
/// Height of a row of shelves.
const SHELF_ROW: f32 = 22.0;
/// Height of the library tab's foot, and the room for a picture in it.
const FOOT: egui::Vec2 = vec2(164.0, 92.0);
/// Fewest characters the search looks for in shelves and `about` lines: shorter ones match
/// nearly every line.
const TEXT_SEARCH_MIN: usize = 3;
/// Largest preview file read.
const MAX_PREVIEW_BYTES: u64 = 16 << 20;
/// Largest preview side decoded.
const MAX_PREVIEW_SIDE: u32 = 8192;
/// Most memory a preview may take while it is decoded.
const MAX_PREVIEW_ALLOC: u64 = 256 << 20;

pub struct AddDialog {
    /// Where the node goes, in graph units.
    pub at: Pos2,
    /// Where the dialog opens, in points.
    screen: Pos2,
    filter: String,
    focus: bool,
    /// A family of the active tab; the editor opens the next dialog on it.
    pub family: String,
    /// What choosing does, beside the search field, when it is not adding a node ("replace
    /// noise1 with").
    pub title: Option<String>,
    /// Whether the node goes into a component, where the component ports can be chosen.
    pub in_component: bool,
    /// The shelf of the library tab; the editor opens the next dialog on it.
    pub shelf: ShelfChoice,
    /// The pictures shown so far by component name; `None` for one that has no usable picture.
    previews: BTreeMap<String, Option<egui::TextureHandle>>,
    /// The search text whose family word last opened its family.
    jumped: String,
}

/// Which shelf the library tab shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShelfChoice {
    pub top: String,
    /// The second level shown last, by first level.
    pub sub: BTreeMap<String, String>,
}

/// A first-level shelf: its name, how many components stand on it and its second level, each
/// with its count. Empty when nothing on it names a second level.
#[derive(Clone, Debug, PartialEq)]
struct Shelf {
    name: String,
    count: usize,
    subs: Vec<(String, usize)>,
}

/// The one or two levels of a `shelf` text. Further levels are ignored.
fn levels(shelf: &str) -> (Option<&str>, Option<&str>) {
    let mut parts = shelf.split('/').map(str::trim).filter(|p| !p.is_empty());
    let top = parts.next();
    (top, parts.next())
}

/// The shelves `o` stands on: its own, and for anything not shipped also `mine`, where its own
/// first level is the second level.
fn places_of(o: &Offered) -> Vec<(&str, Option<&str>)> {
    let (top, sub) = levels(&o.shelf);
    if o.place == "penumbra" {
        return vec![(top.unwrap_or(OTHER), sub)];
    }
    let mut places = vec![(MINE, top)];
    if let Some(top) = top {
        places.push((top, sub));
    }
    places
}

/// The shelves of `library`: `mine` first, `other` last, the rest by name.
fn shelves(library: &[Offered]) -> Vec<Shelf> {
    let mut found: BTreeMap<&str, (usize, BTreeMap<Option<&str>, usize>)> = BTreeMap::new();
    for o in library {
        for (top, sub) in places_of(o) {
            let shelf = found.entry(top).or_default();
            shelf.0 += 1;
            *shelf.1.entry(sub).or_default() += 1;
        }
    }
    let mut out: Vec<Shelf> = found
        .into_iter()
        .map(|(name, (count, by_sub))| {
            let loose = by_sub.get(&None).copied().unwrap_or(0);
            let mut subs: Vec<(String, usize)> =
                by_sub.iter().filter_map(|(s, n)| Some(((*s)?.to_string(), *n))).collect();
            if !subs.is_empty() && loose > 0 {
                subs.push((OTHER.to_string(), loose));
            }
            Shelf { name: name.to_string(), count, subs }
        })
        .collect();
    let rank = |name: &str| match name {
        MINE => 0,
        OTHER => 2,
        _ => 1,
    };
    out.sort_by_key(|s| (rank(&s.name), s.name.to_lowercase()));
    out
}

/// The shelf `choice` means among `shelves`: what it names, else the first; and the second level
/// likewise, if the shelf has one.
fn active_shelf<'a>(shelves: &'a [Shelf], choice: &ShelfChoice) -> Option<(&'a Shelf, Option<&'a str>)> {
    let shelf = shelves.iter().find(|s| s.name == choice.top).or_else(|| shelves.first())?;
    let wanted = choice.sub.get(&shelf.name);
    let sub = shelf.subs.iter().find(|(n, _)| Some(n) == wanted).or_else(|| shelf.subs.first());
    Some((shelf, sub.map(|(n, _)| n.as_str())))
}

/// The components the library tab lists: while searching the matches of all shelves, those
/// found by name first; else the active shelf, by name.
fn listed(
    library: &[Offered],
    shelves: &[Shelf],
    choice: &ShelfChoice,
    usable: Usable<'_>,
    filter: &str,
) -> Vec<Offered> {
    let by_name = |o: &Offered| o.name.to_lowercase();
    if !filter.is_empty() {
        let mut hits: Vec<Offered> = library
            .iter()
            .filter(|o| usable.hit(&format!("{LIBRARY_MARK}{}", o.name), filter))
            .cloned()
            .collect();
        hits.sort_by_key(|o| (!matches(&o.name, filter), by_name(o)));
        return hits;
    }
    let Some((shelf, sub)) = active_shelf(shelves, choice) else { return Vec::new() };
    let stands_here = |o: &&Offered| {
        places_of(o)
            .iter()
            .any(|(t, s)| *t == shelf.name && (sub.is_none() || Some(s.unwrap_or(OTHER)) == sub))
    };
    let mut on: Vec<Offered> = library.iter().filter(stands_here).cloned().collect();
    on.sort_by_key(by_name);
    on
}

/// A component's picture as an image, smaller than the file. `None` for a missing, oversized or
/// broken file: the foot then shows no picture.
fn read_preview(path: &Path) -> Option<egui::ColorImage> {
    if std::fs::metadata(path).ok()?.len() > MAX_PREVIEW_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_PREVIEW_SIDE);
    limits.max_image_height = Some(MAX_PREVIEW_SIDE);
    limits.max_alloc = Some(MAX_PREVIEW_ALLOC);
    reader.limits(limits);
    let picture = reader.decode().ok()?.thumbnail(2 * FOOT.x as u32, 2 * FOOT.y as u32).to_rgba8();
    let size = [picture.width() as usize, picture.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, picture.as_raw()))
}

pub enum Outcome {
    Open,
    Close,
    Chosen(String),
    /// Take the component file of this name out of the library (`comp.remove`); the dialog
    /// stays open.
    Remove(String),
}

/// The tabs: the families of each.
/// What a tab says: its name, else its families.
fn tab_name(families: &[String]) -> String {
    let first = families.first().map_or("", String::as_str);
    NAMES.iter().find(|(f, _)| *f == first).map_or_else(|| families.join(" "), |(_, n)| (*n).to_string())
}

fn groups(types: &NodeTypes) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> =
        GROUPS.iter().map(|g| g.iter().map(|f| (*f).to_string()).collect()).collect();
    let mut extra: Vec<String> = types
        .kinds()
        .filter(|k| *k != penumbra_graph::broken::BROKEN_KIND)
        .filter_map(|k| k.split_once('.').map(|(f, _)| f.to_string()))
        .filter(|f| !out.iter().flatten().any(|g| g == f))
        .collect();
    extra.sort();
    extra.dedup();
    out.push(vec![LIBRARY.to_string()]);
    out.extend(extra.into_iter().map(|f| vec![f]));
    out
}

/// A type in the grid: its column and row, and its family.
struct Cell {
    col: usize,
    row: usize,
    kind: String,
    family: String,
}

/// Columns under one heading: a family's common types or one of its blocks.
struct Section {
    family: String,
    block: Option<Block>,
    first: usize,
    last: usize,
}

/// The types of a tab, each family and each block of it from a new column; empty ones take no
/// room.
fn cells(types: &NodeTypes, group: &[String], library: &[Offered]) -> (Vec<Cell>, Vec<Section>) {
    let all: Vec<(&String, Option<Block>, Vec<String>)> = group
        .iter()
        .flat_map(|f| sections_of(types, f, library).into_iter().map(move |(b, k)| (f, b, k)))
        .collect();
    let rows = all.iter().map(|(_, _, kinds)| rows_for(kinds)).max().unwrap_or(ROWS);
    let mut out = Vec::new();
    let mut sections = Vec::new();
    let mut col = 0;
    for (family, block, kinds) in all {
        let columns = kinds.len().div_ceil(rows);
        if columns == 0 {
            continue;
        }
        for (i, kind) in kinds.into_iter().enumerate() {
            out.push(Cell { col: col + i / rows, row: i % rows, kind, family: family.clone() });
        }
        sections.push(Section { family: family.clone(), block, first: col, last: col + columns - 1 });
        col += columns;
    }
    (out, sections)
}

/// Rows a section needs: [`ROWS`], or a few more where the line and the component ports would
/// otherwise spill into a column of their own.
fn rows_for(kinds: &[String]) -> usize {
    let types = kinds.iter().position(|k| k == GAP).unwrap_or(kinds.len());
    let columns = types.div_ceil(ROWS).max(1);
    kinds.len().div_ceil(columns).max(ROWS)
}

/// Everything `family` offers, in grid order.
fn kinds_of(types: &NodeTypes, family: &str, library: &[Offered]) -> Vec<String> {
    sections_of(types, family, library).into_iter().flat_map(|(_, kinds)| kinds).collect()
}

/// The kinds of `family` by section: its common types sorted by their short name (`comp.base`
/// first) and its component ports (`tex.in`, `tex.out`) after them behind a gap ([`GAP`]), so
/// they do not read as a source and a sink; then each [`Block`], empty or not. The `library`
/// tab holds the component files it is given as `@name`, in the order given.
fn sections_of(types: &NodeTypes, family: &str, library: &[Offered]) -> Vec<(Option<Block>, Vec<String>)> {
    if family == LIBRARY {
        return vec![(None, library.iter().map(|o| format!("{LIBRARY_MARK}{}", o.name)).collect())];
    }
    let in_family = |k: &&str| k.split_once('.').is_some_and(|(f, _)| f == family);
    let block_of = |k: &str| types.desc(k).and_then(|d| d.block);
    let is_port = |k: &str| types.desc(k).is_some_and(|d| d.port.is_some());
    let listed: Vec<&str> = types
        .kinds()
        .filter(in_family)
        // Only loading a project makes a placeholder of a node that did not load.
        .filter(|k| *k != penumbra_graph::broken::BROKEN_KIND)
        .collect();
    let sorted = |keep: &dyn Fn(&str) -> bool| {
        let mut kinds: Vec<String> = listed.iter().filter(|k| keep(k)).map(|k| (*k).to_string()).collect();
        kinds.sort_by_key(|k| (*k != penumbra_ops::comp::COMP_KIND, short(k).to_lowercase()));
        kinds
    };
    let mut common = sorted(&|k| !is_port(k) && block_of(k).is_none());
    let ports = sorted(&|k| is_port(k));
    if !ports.is_empty() {
        if !common.is_empty() {
            common.push(GAP.to_string());
        }
        common.extend(ports);
    }
    let mut out = vec![(None, common)];
    for block in Block::ALL {
        out.push((Some(block), sorted(&|k| !is_port(k) && block_of(k) == Some(block))));
    }
    out
}

/// A component file on offer and where it lies: `project` (the project's `components/`),
/// `library` (the user's library) or `penumbra` (shipped).
#[derive(Clone, Debug, PartialEq)]
pub struct Offered {
    pub name: String,
    pub place: &'static str,
    /// Its place in the library, as its `shelf` param says; may be empty.
    pub shelf: String,
    /// What it does, in one line; may be empty.
    pub about: String,
    pub preview: Option<PathBuf>,
}

/// The empty cell before a family's component ports: a line, not a choice.
const GAP: &str = "";

/// Marks a component file among the kinds of the `library` tab.
pub const LIBRARY_MARK: char = '@';

fn short(kind: &str) -> &str {
    if let Some(name) = kind.strip_prefix(LIBRARY_MARK) {
        return name;
    }
    kind.split_once('.').map_or(kind, |(_, s)| s)
}

/// Enter's choice among `kinds` (in grid order) for the trimmed, lower-case `filter`: the type
/// named exactly so, then one whose name starts with it, then any found by its name, then one
/// found by a tag. So `noise` takes `tex.noise`, not `tex.denoise` above it.
fn enter_choice<'k>(
    kinds: impl Iterator<Item = &'k str> + Clone,
    usable: Usable<'_>,
    filter: &str,
) -> Option<String> {
    let hits = kinds.filter(|k| usable.hit(k, filter));
    let by_name = hits.clone().filter(|k| usable.tag(k, filter).is_none());
    // Spaces, underscores and dots do not count, as in the search itself.
    let plain =
        |t: &str| t.chars().filter(|c| !matches!(c, ' ' | '_' | '.')).collect::<String>().to_lowercase();
    let wanted = plain(filter);
    let name = |k: &&str| plain(short(k));
    by_name
        .clone()
        .find(|k| name(k) == wanted)
        .or_else(|| by_name.clone().find(|k| name(k).starts_with(&wanted)))
        .or_else(|| by_name.clone().next())
        .or_else(|| hits.clone().next())
        .map(str::to_string)
}

/// The tab Tab (or with `back` Shift+Tab) moves to from `active`: the next one that `wanted`
/// takes, all the way round to `active` itself. When none is wanted, the next one.
fn next_tab<T>(groups: &[T], active: usize, back: bool, wanted: impl Fn(&T) -> bool) -> usize {
    let n = groups.len();
    if n == 0 {
        return 0;
    }
    let step = |i: usize| if back { (active + n - i % n) % n } else { (active + i) % n };
    (1..=n).map(step).find(|&g| groups.get(g).is_some_and(&wanted)).unwrap_or_else(|| step(1))
}

fn matches(kind: &str, filter: &str) -> bool {
    kind != GAP && penumbra_graph::name_found(kind, filter)
}

/// What can be chosen where the node goes: everything inside a component, everything but the
/// component ports at the top level.
#[derive(Clone, Copy)]
struct Usable<'a> {
    types: &'a NodeTypes,
    in_component: bool,
    library: &'a [Offered],
    /// The only family the search finds in, named by a TouchDesigner family word in the text
    /// ([`penumbra_graph::Search`]).
    family: Option<&'a str>,
}

impl Usable<'_> {
    fn allows(self, kind: &str) -> bool {
        self.in_component || self.types.desc(kind).is_none_or(|d| d.port.is_none())
    }

    /// A choice for the search: usable here, and containing the text in its name, starting a
    /// tag with it or, for a component file, naming it in its shelf or `about` line.
    fn hit(self, kind: &str, filter: &str) -> bool {
        self.allows(kind)
            && self.family.is_none_or(|f| kind.split_once('.').is_some_and(|(k, _)| k == f))
            && (matches(kind, filter) || self.tag(kind, filter).is_some() || self.says(kind, filter))
    }

    /// Whether the component file `kind` has `filter` (lower case) in its shelf or `about` line.
    fn says(self, kind: &str, filter: &str) -> bool {
        let Some(name) = kind.strip_prefix(LIBRARY_MARK) else { return false };
        filter.chars().count() >= TEXT_SEARCH_MIN
            && self.library.iter().any(|o| {
                o.name == name
                    && (o.about.to_lowercase().contains(filter) || o.shelf.to_lowercase().contains(filter))
            })
    }

    /// The tag the search finds `kind` by when its name does not contain the text.
    fn tag(self, kind: &str, filter: &str) -> Option<String> {
        if kind == GAP || matches(kind, filter) {
            return None;
        }
        self.types.tag_found(kind, filter)
    }
}

impl AddDialog {
    pub fn new(screen: Pos2, at: Pos2, family: &str) -> Self {
        Self {
            at,
            screen,
            filter: String::new(),
            focus: true,
            family: family.to_string(),
            title: None,
            in_component: false,
            shelf: ShelfChoice::default(),
            previews: BTreeMap::new(),
            jumped: String::new(),
        }
    }

    /// `library`: the component files on offer, shown in the `library` tab; choosing one gives
    /// `@name`.
    pub fn show(
        &mut self,
        ui: &Ui,
        types: &NodeTypes,
        library: &[Offered],
        auto: &mut automation::Registry,
    ) -> Outcome {
        let groups = groups(types);
        let typed = self.filter.trim().to_lowercase();
        let search = types.search(&typed);
        let filter = search.text.clone();
        let usable = Usable { types, in_component: self.in_component, library, family: search.family };
        // A family word typed (`noise top`) opens its family, once per text, so Tab still
        // moves on from there.
        match search.family {
            Some(f) if self.jumped != typed => {
                self.family = f.to_string();
                self.jumped.clone_from(&typed);
            }
            Some(_) => {}
            None => self.jumped.clear(),
        }
        let mut active = groups.iter().position(|g| g.contains(&self.family)).unwrap_or(0);
        let mut outcome = Outcome::Open;

        // Keys first, so Tab does not move the focus out of the search field.
        let (enter, escape, tab, back) = ui.input_mut(|i| {
            (
                i.key_pressed(Key::Enter),
                i.consume_key(egui::Modifiers::NONE, Key::Escape),
                i.consume_key(egui::Modifiers::NONE, Key::Tab),
                i.consume_key(egui::Modifiers::SHIFT, Key::Tab),
            )
        });
        if (tab || back) && !groups.is_empty() {
            active = next_tab(&groups, active, back, |g| {
                filter.is_empty()
                    || g.iter().any(|f| kinds_of(types, f, library).iter().any(|k| usable.hit(k, &filter)))
            });
            self.focus = true;
            // egui moves the focus on Tab before the key is consumed here; without this the
            // close button gets it, and the next Enter presses it instead of choosing.
            ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        }
        let group: &[String] = groups.get(active).map_or(&[], Vec::as_slice);
        if !group.contains(&self.family) {
            self.family = group.first().cloned().unwrap_or_default();
        }
        if escape {
            outcome = Outcome::Close;
        }

        let on_library = group.iter().any(|f| f == LIBRARY);
        let shelves = shelves(library);
        let shown =
            if on_library { listed(library, &shelves, &self.shelf, usable, &filter) } else { Vec::new() };
        let (cells, sections) = cells(types, group, &shown);
        let first = enter_choice(cells.iter().map(|c| c.kind.as_str()), usable, &filter);
        if enter && let Some(k) = &first {
            outcome = Outcome::Chosen(k.clone());
        }

        let columns = cells.iter().map(|c| c.col + 1).max().unwrap_or(1);
        let width = (columns as f32 * CELL.x).max(MIN_WIDTH);
        let area = egui::Area::new(ui.id().with("add_dialog"))
            .order(egui::Order::Foreground)
            .fixed_pos(self.screen)
            .constrain_to(ui.ctx().content_rect())
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(width);
                    self.search_row(ui, auto, &mut outcome);
                    ui.add_space(2.0);
                    self.tabs(ui, &groups, active, usable, library, &filter, auto);
                    ui.separator();
                    if on_library {
                        self.shelf_rows(ui, &shelves, &filter, shown.len(), width, auto);
                    }
                    headings(ui, group, &sections, width, auto);
                    let grid = Grid {
                        cells: &cells,
                        group,
                        width,
                        filter: &filter,
                        first: first.as_deref(),
                        library,
                        usable,
                    };
                    let hovered = grid.show(ui, auto, &mut outcome);
                    ui.separator();
                    // The summary of the type under the pointer (or of Enter's choice).
                    let about = hovered.or(first.as_deref());
                    if on_library {
                        self.foot(ui, about, library);
                    } else {
                        summary(ui, about, usable, library, &filter, width);
                    }
                });
            });
        let outside = ui.input(|i| i.pointer.any_pressed())
            && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| !area.response.rect.contains(p));
        if outside && matches!(outcome, Outcome::Open) {
            outcome = Outcome::Close;
        }
        outcome
    }

    /// The search field and the close button.
    fn search_row(&mut self, ui: &mut Ui, auto: &mut automation::Registry, outcome: &mut Outcome) {
        ui.horizontal(|ui| {
            let edit =
                ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("search").desired_width(220.0));
            auto.add("add.filter", edit.rect, "search");
            if self.focus {
                edit.request_focus();
                self.focus = false;
            }
            if let Some(title) = &self.title {
                ui.label(egui::RichText::new(title).color(theme::ACCENT));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = icons::small_button(ui, Icon::Remove).on_hover_text("Close (Esc)");
                auto.add("add.close", r.rect, "close");
                // Enter's choice wins over a press of the button in the same frame.
                if r.clicked() && matches!(outcome, Outcome::Open) {
                    *outcome = Outcome::Close;
                }
            });
        });
    }

    /// The rows of shelves of the library tab. They keep their room while searching and on a
    /// shelf without a second level, so the grid stays where it is.
    fn shelf_rows(
        &mut self,
        ui: &mut Ui,
        shelves: &[Shelf],
        filter: &str,
        hits: usize,
        width: f32,
        auto: &mut automation::Registry,
    ) {
        let row = |ui: &mut Ui, draw: &mut dyn FnMut(&mut Ui)| {
            ui.allocate_ui_with_layout(
                vec2(width, SHELF_ROW),
                egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    ui.set_min_height(SHELF_ROW);
                    draw(ui);
                },
            );
        };
        if !filter.is_empty() {
            let text = if hits == 1 {
                "1 match on all shelves".to_string()
            } else {
                format!("{hits} matches on all shelves")
            };
            row(ui, &mut |ui| {
                ui.label(egui::RichText::new(text.as_str()).color(theme::TEXT_WEAK));
            });
            row(ui, &mut |_| {});
            return;
        }
        let Some((active, sub)) = active_shelf(shelves, &self.shelf) else {
            row(ui, &mut |_| {});
            row(ui, &mut |_| {});
            return;
        };
        let active_name = active.name.clone();
        let mut picked = None;
        row(ui, &mut |ui| {
            for s in shelves {
                let id = format!("add.shelf.{}", s.name.replace(' ', "_"));
                if shelf_button(ui, &s.name, s.count, s.name == active_name, false, id, auto).clicked() {
                    picked = Some(s.name.clone());
                }
            }
        });
        let mut picked_sub = None;
        row(ui, &mut |ui| {
            for (name, count) in &active.subs {
                let id = format!("add.shelf.{}.{}", active_name.replace(' ', "_"), name.replace(' ', "_"));
                if shelf_button(ui, name, *count, Some(name.as_str()) == sub, true, id, auto).clicked() {
                    picked_sub = Some(name.clone());
                }
            }
        });
        if let Some(name) = picked_sub {
            self.shelf.sub.insert(active_name.clone(), name);
            self.focus = true;
        }
        self.shelf.top = active_name;
        if let Some(name) = picked {
            self.shelf.top = name;
            self.focus = true;
        }
    }

    /// The foot of the library tab, of one height so the dialog does not jump under the pointer:
    /// the component `about` names, with its picture and what it says.
    fn foot(&mut self, ui: &mut Ui, about: Option<&str>, library: &[Offered]) {
        let entry = about
            .and_then(|k| k.strip_prefix(LIBRARY_MARK))
            .and_then(|name| library.iter().find(|o| o.name == name));
        let layout = egui::Layout::left_to_right(egui::Align::TOP);
        ui.allocate_ui_with_layout(vec2(ui.available_width(), FOOT.y), layout, |ui| {
            ui.set_min_height(FOOT.y);
            let Some(entry) = entry else {
                ui.label(
                    egui::RichText::new("Point at an entry to see what it does.").color(theme::TEXT_WEAK),
                );
                return;
            };
            let picture = self.preview(ui, entry);
            let (room, _) = ui.allocate_exact_size(FOOT, Sense::hover());
            match picture {
                Some(picture) => {
                    let size = picture.size_vec2();
                    let shown =
                        Rect::from_center_size(room.center(), size * (FOOT.x / size.x).min(FOOT.y / size.y));
                    let whole = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                    ui.painter().image(picture.id(), shown, whole, Color32::WHITE);
                }
                None => {
                    ui.painter().rect_stroke(
                        room.shrink(0.5),
                        CornerRadius::ZERO,
                        Stroke::new(1.0, Color32::from_gray(60)),
                        StrokeKind::Inside,
                    );
                    ui.painter().text(
                        room.center(),
                        Align2::CENTER_CENTER,
                        "no picture",
                        FontId::proportional(12.0),
                        Color32::from_gray(95),
                    );
                }
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(egui::RichText::new(&entry.name).color(theme::TEXT).strong());
                let shelf = if entry.shelf.is_empty() { MINE } else { entry.shelf.as_str() };
                ui.label(
                    egui::RichText::new(shelf.replace('/', " / "))
                        .small()
                        .color(theme::family_color(LIBRARY)),
                );
                let about = if entry.about.is_empty() {
                    "This component does not say what it does yet."
                } else {
                    &entry.about
                };
                ui.label(egui::RichText::new(about).color(theme::TEXT_WEAK));
            });
        });
    }

    /// The picture of `entry`, decoded the first time it is shown.
    fn preview(&mut self, ui: &Ui, entry: &Offered) -> Option<egui::TextureHandle> {
        self.previews
            .entry(entry.name.clone())
            .or_insert_with(|| {
                let image = read_preview(entry.preview.as_deref()?)?;
                Some(ui.ctx().load_texture(
                    format!("library {}", entry.name),
                    image,
                    egui::TextureOptions::LINEAR,
                ))
            })
            .clone()
    }

    /// One tab per group, a segment per family coloured like its node headers; a named tab
    /// writes its name across them.
    #[allow(clippy::too_many_arguments)]
    fn tabs(
        &mut self,
        ui: &mut Ui,
        groups: &[Vec<String>],
        active: usize,
        usable: Usable<'_>,
        library: &[Offered],
        filter: &str,
        auto: &mut automation::Registry,
    ) {
        ui.horizontal(|ui| {
            // Wider than a seam, so a tab of several families reads as one.
            ui.spacing_mut().item_spacing.x = 6.0;
            for (g, families) in groups.iter().enumerate() {
                let r = tab(ui, families, g == active, usable, library, filter, auto);
                if r.clicked() {
                    self.family = families.first().cloned().unwrap_or_default();
                    self.focus = true;
                }
            }
        });
    }
}

/// A shelf of the library tab: filled when active on the first level, underlined on the second.
fn shelf_button(
    ui: &mut Ui,
    name: &str,
    count: usize,
    active: bool,
    second: bool,
    id: String,
    auto: &mut automation::Registry,
) -> egui::Response {
    let color = theme::family_color(LIBRARY);
    let text = match (active, second) {
        (true, false) => Color32::from_gray(10),
        (true, true) => Color32::WHITE,
        (false, _) => theme::TEXT,
    };
    let label = ui.painter().layout_no_wrap(name.to_string(), FontId::proportional(13.0), text);
    let number =
        ui.painter().layout_no_wrap(count.to_string(), FontId::proportional(11.0), text.gamma_multiply(0.65));
    let width = label.size().x + number.size().x + 22.0;
    let (rect, r) = ui.allocate_exact_size(vec2(width, SHELF_ROW), Sense::click());
    if second {
        if active {
            let bar = Rect::from_min_max(
                rect.left_bottom() + vec2(3.0, -2.0),
                rect.right_bottom() - vec2(3.0, 0.0),
            );
            ui.painter().rect_filled(bar, CornerRadius::ZERO, color);
        }
    } else {
        let fill = match (active, r.hovered()) {
            (true, _) => color,
            (false, true) => color.gamma_multiply(0.45),
            (false, false) => color.gamma_multiply(0.22),
        };
        ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
    }
    let left = rect.min.x + 8.0;
    let number_at = pos2(left + label.size().x + 5.0, rect.center().y - number.size().y / 2.0);
    ui.painter().galley(pos2(left, rect.center().y - label.size().y / 2.0), label, text);
    ui.painter().galley(number_at, number, text);
    auto.add(id, rect, name);
    r
}

/// One tab of the dialog; `families` its families, `filter` the search text.
fn tab(
    ui: &mut Ui,
    families: &[String],
    active: bool,
    usable: Usable<'_>,
    library: &[Offered],
    filter: &str,
    auto: &mut automation::Registry,
) -> egui::Response {
    let name = tab_name(families);
    let named = families.len() > 1;
    // While searching: which families have a match?
    let mut any_bright = false;
    let segments: Vec<(&String, bool, Option<Arc<egui::Galley>>)> = families
        .iter()
        .map(|f| {
            let kinds = kinds_of(usable.types, f, library);
            let hit = !filter.is_empty() && kinds.iter().any(|k| usable.hit(k, filter));
            let bright = if filter.is_empty() { !kinds.is_empty() } else { hit };
            any_bright |= bright;
            let text = if bright { Color32::WHITE } else { theme::TEXT_WEAK };
            let galley =
                (!named).then(|| ui.painter().layout_no_wrap(f.clone(), FontId::proportional(14.0), text));
            (f, hit, galley)
        })
        .collect();
    // A named tab is bright when any of its families is.
    let title_text = if any_bright { Color32::WHITE } else { theme::TEXT_WEAK };
    let title =
        named.then(|| ui.painter().layout_no_wrap(name.clone(), FontId::proportional(14.0), title_text));
    // A named tab shares its width out evenly among its families.
    let even = title.as_ref().map_or(0.0, |t| (t.size().x + 24.0) / segments.len() as f32);
    let widths: Vec<f32> = segments
        .iter()
        .map(|(_, _, t)| t.as_ref().map_or(even.max(12.0), |t| (t.size().x + 16.0).max(64.0)))
        .collect();
    let (rect, r) = ui.allocate_exact_size(vec2(widths.iter().sum(), 22.0), Sense::click());
    let mut x = rect.min.x;
    for ((f, hit, galley), w) in segments.iter().zip(&widths) {
        let seg = Rect::from_min_size(pos2(x, rect.min.y), vec2(*w, rect.height()));
        x += w;
        let color = theme::family_color(f);
        let fill = match (active, r.hovered()) {
            (true, _) => color,
            (false, true) => color.gamma_multiply(0.55),
            (false, false) => color.gamma_multiply(0.35),
        };
        ui.painter().rect_filled(seg, CornerRadius::ZERO, fill);
        if let Some(galley) = galley {
            let at = seg.center() - galley.size() / 2.0;
            ui.painter().galley(at, galley.clone(), Color32::WHITE);
        }
        if *hit {
            let bar = Rect::from_min_max(
                seg.left_bottom() + vec2(4.0, -3.0),
                seg.right_bottom() + vec2(-4.0, -1.0),
            );
            ui.painter().rect_filled(bar, CornerRadius::ZERO, Color32::WHITE);
        }
    }
    if let Some(title) = title {
        let at = rect.center() - title.size() / 2.0;
        ui.painter().galley(at, title, Color32::WHITE);
    }
    let hit = segments.iter().any(|(_, hit, _)| *hit);
    let label = if hit { format!("{name} (match)") } else { name };
    let first_family = families.first().map_or("", String::as_str);
    auto.add(format!("add.tab.{first_family}"), rect, label);
    r
}

/// A tab of several families or blocks: a heading over each, with a line in its family's colour
/// across its columns. A family's common types are headed by the family in a shared tab, and
/// by the line alone in a tab of their own, whose name says it already.
fn headings(
    ui: &mut Ui,
    group: &[String],
    sections: &[Section],
    width: f32,
    auto: &mut automation::Registry,
) {
    if sections.len() <= 1 {
        return;
    }
    let (row, _) = ui.allocate_exact_size(vec2(width, HEADING), Sense::hover());
    let painter = ui.painter_at(row);
    for Section { family, block, first, last } in sections {
        let left = row.min.x + *first as f32 * CELL.x + 1.0;
        let right = row.min.x + (*last + 1) as f32 * CELL.x - 1.0;
        let (id, heading) = match block {
            Some(b) => (format!("{family}.{}", b.heading().replace(' ', "_")), b.heading()),
            None if group.len() > 1 => (
                family.clone(),
                HEADINGS.iter().find(|(f, _)| f == family).map_or(family.as_str(), |(_, h)| *h),
            ),
            None => (family.clone(), ""),
        };
        let r = painter.text(
            pos2(left + 5.0, row.center().y - 1.0),
            Align2::LEFT_CENTER,
            heading,
            FontId::proportional(13.0),
            theme::TEXT,
        );
        if !heading.is_empty() {
            auto.add(format!("add.heading.{id}"), r, heading);
        }
        painter.line_segment(
            [pos2(left, row.max.y - 1.0), pos2(right, row.max.y - 1.0)],
            Stroke::new(2.0, theme::family_color(family)),
        );
    }
}

/// The grid of the active tab: columns of [`ROWS`], top to bottom.
struct Grid<'a> {
    cells: &'a [Cell],
    group: &'a [String],
    width: f32,
    /// The search text, trimmed and lower case.
    filter: &'a str,
    /// The first match, Enter's choice.
    first: Option<&'a str>,
    library: &'a [Offered],
    usable: Usable<'a>,
}

impl<'a> Grid<'a> {
    /// Draws the grid; a click on a type chooses it. Returns the type under the pointer.
    fn show(&self, ui: &mut Ui, auto: &mut automation::Registry, outcome: &mut Outcome) -> Option<&'a str> {
        let on_library = self.group.iter().any(|f| f == LIBRARY);
        // The library tab keeps its height from shelf to shelf.
        let least = if on_library { ROWS } else { 1 };
        let rows = self.cells.iter().map(|c| c.row + 1).max().unwrap_or(1).max(least);
        let (grid, _) = ui.allocate_exact_size(vec2(self.width, rows as f32 * CELL.y), Sense::hover());
        let painter = ui.painter_at(grid);
        if self.cells.is_empty() {
            let text = if on_library && !self.filter.is_empty() {
                "no component has this name or says this".to_string()
            } else if on_library {
                "no component files yet: \"Save to the library\" in a component's menu adds one".to_string()
            } else {
                format!("no {} node types yet", self.group.join(" or "))
            };
            painter.text(
                grid.left_top() + vec2(6.0, CELL.y / 2.0),
                Align2::LEFT_CENTER,
                text,
                FontId::proportional(14.0),
                theme::TEXT_WEAK,
            );
        }
        let mut hovered = None;
        for Cell { col, row, kind, family } in self.cells {
            let (col, row) = (*col, *row);
            let color = theme::family_color(family);
            let cell = Rect::from_min_size(grid.min + vec2(col as f32 * CELL.x, row as f32 * CELL.y), CELL)
                .shrink2(vec2(1.0, 0.5));
            if kind == GAP {
                let y = cell.center().y;
                painter.line_segment(
                    [pos2(cell.min.x + 6.0, y), pos2(cell.max.x - 6.0, y)],
                    Stroke::new(1.0, theme::TEXT_WEAK),
                );
                continue;
            }
            let allowed = self.usable.allows(kind);
            let sense = if allowed { Sense::click() } else { Sense::hover() };
            let r = ui.interact(cell, ui.id().with(("add", kind.as_str())), sense);
            let label = if allowed { kind.clone() } else { format!("{kind} (only in a component)") };
            auto.add(format!("add.{kind}"), cell, label);
            let hit = self.usable.hit(kind, self.filter);
            let fill = match (hit, r.hovered()) {
                (_, true) if allowed => color.gamma_multiply(1.4),
                (_, true) => color.gamma_multiply(0.3),
                (true, false) => color.gamma_multiply(if row % 2 == 0 { 0.8 } else { 0.65 }),
                (false, false) => color.gamma_multiply(0.2),
            };
            painter.rect_filled(cell, CornerRadius::ZERO, fill);
            if self.first == Some(kind.as_str()) && !self.filter.is_empty() {
                painter.rect_stroke(
                    cell,
                    CornerRadius::ZERO,
                    Stroke::new(1.0, Color32::WHITE),
                    StrokeKind::Inside,
                );
            }
            painter.text(
                cell.left_center() + vec2(6.0, 0.0),
                Align2::LEFT_CENTER,
                short(kind),
                FontId::proportional(14.0),
                if hit { theme::TEXT } else { Color32::from_gray(95) },
            );
            let mut removing = false;
            if let Some(name) = kind.strip_prefix(LIBRARY_MARK)
                && self.library_file(ui, &painter, cell, &r, name, auto)
            {
                *outcome = Outcome::Remove(name.to_string());
                removing = true;
            }
            if r.hovered() {
                hovered = Some(kind.as_str());
            }
            if r.clicked() && allowed && !removing {
                *outcome = Outcome::Chosen(kind.clone());
            }
        }
        hovered
    }

    /// A component file rather than a node type: where it comes from, at the right, and under
    /// the pointer a button that takes it out of the library (there was no way to). True when
    /// that button was clicked.
    fn library_file(
        &self,
        ui: &Ui,
        painter: &egui::Painter,
        cell: Rect,
        r: &egui::Response,
        name: &str,
        auto: &mut automation::Registry,
    ) -> bool {
        let place = self.library.iter().find(|o| o.name == name).map_or("", |o| o.place);
        // penumbra's own components are nearly all there is: naming each would only crowd the
        // names. They stay in its library, so they have no button to take them out.
        if place == "penumbra" {
            return false;
        }
        let place_text = || {
            painter.text(
                cell.right_center() - vec2(6.0, 0.0),
                Align2::RIGHT_CENTER,
                place,
                FontId::proportional(11.0),
                theme::TEXT_WEAK,
            );
        };
        let cross = Rect::from_center_size(
            cell.right_center() - vec2(4.0 + CELL.y / 2.0, 0.0),
            vec2(CELL.y - 4.0, CELL.y - 4.0),
        );
        let x = ui.interact(cross, ui.id().with(("remove", name)), Sense::click());
        auto.add(format!("add.remove.{name}"), cross, name);
        if r.hovered() || x.hovered() {
            if x.hovered() {
                painter.rect_filled(cross, CornerRadius::same(3), theme::NEGATIVE);
            }
            painter.text(cross.center(), Align2::CENTER_CENTER, "×", FontId::proportional(15.0), theme::TEXT);
            let from = if place == "project" { "the project's components" } else { "the library" };
            x.clone().on_hover_text(format!(
                "Removes `{name}` from {from}. The file moves into a `removed` folder beside it"
            ));
        } else {
            place_text();
        }
        x.clicked()
    }
}

/// The summary of the type or component file `about`, in one line, with the tag the search
/// found it by.
fn summary(
    ui: &mut Ui,
    about: Option<&str>,
    usable: Usable<'_>,
    library: &[Offered],
    filter: &str,
    width: f32,
) {
    let text = about
        .and_then(|k| match k.strip_prefix(LIBRARY_MARK) {
            Some(name) => {
                let place = library.iter().find(|o| o.name == name).map_or("", |o| o.place);
                let place = match place {
                    "project" => "the project's components/",
                    "penumbra" => "penumbra's own library",
                    _ => "the library",
                };
                Some(format!("{name}: a component file from {place} (comp.load)"))
            }
            None if !usable.allows(k) => Some(format!(
                "{k}: only inside a component. At the top level there is no component socket for it"
            )),
            None => usable.types.desc(k).map(|d| match usable.tag(k, filter) {
                Some(tag) => format!("{k} (tag `{tag}`): {}", d.summary),
                None => format!("{k}: {}", d.summary),
            }),
        })
        .unwrap_or_default();
    ui.allocate_ui_with_layout(vec2(width, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.add(egui::Label::new(egui::RichText::new(text).color(theme::TEXT_WEAK)).truncate())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_and_the_scan_workflow_follow_the_common_types() {
        let types = penumbra_ops::builtin_types().unwrap();
        let tex = sections_of(&types, "tex", &[]);
        let blocks: Vec<Option<Block>> = tex.iter().map(|(b, _)| *b).collect();
        assert_eq!(blocks, [None, Some(Block::In), Some(Block::ScanAndShapes)]);
        let common = &tex[0].1;
        assert!(common.contains(&"tex.blur".to_string()));
        assert!(!common.contains(&"tex.video_in".to_string()));
        assert_eq!(common[common.len() - 3..], [GAP, "tex.in", "tex.out"], "ports last, after a line");
        assert_eq!(tex[1].1, ["tex.ndi_in", "tex.screen_in", "tex.spout_in", "tex.video_in"]);
        assert_eq!(
            tex[2].1,
            [
                "tex.blend_weight",
                "tex.face_content",
                "tex.scan",
                "tex.scan_pattern",
                "tex.shape_content",
                "tex.shapes"
            ]
        );
        let chan = sections_of(&types, "chan", &[]);
        assert!(chan[1].1.contains(&"chan.osc_in".to_string()));
        let audio = sections_of(&types, "audio", &[]);
        assert_eq!(audio[1].1, ["audio.device_in"], "the device is a live input");
        assert!(audio[0].1.contains(&"audio.oscillator".to_string()));
        let out = sections_of(&types, "out", &[]);
        for sender in ["out.osc", "out.midi", "out.data"] {
            assert!(out[0].1.contains(&sender.to_string()), "{sender} sits with the other outputs");
        }
    }

    #[test]
    fn the_library_is_a_tab_of_its_own() {
        let types = penumbra_ops::builtin_types().unwrap();
        let library = [offered("water", "penumbra", "", ""), offered("Mine", "library", "", "")];
        let tabs = groups(&types);
        assert!(tabs.contains(&vec![LIBRARY.to_string()]));
        assert_eq!(kinds_of(&types, LIBRARY, &library).len(), 2, "the tab's search sees all of them");
        assert_eq!(kinds_of(&types, "comp", &library), ["comp.base"], "comp keeps its own types");
    }

    fn offered(name: &str, place: &'static str, shelf: &str, about: &str) -> Offered {
        Offered { name: name.into(), place, shelf: shelf.into(), about: about.into(), preview: None }
    }

    fn names(list: &[Offered]) -> Vec<&str> {
        list.iter().map(|o| o.name.as_str()).collect()
    }

    /// Shelves about things that are not pictures: nothing in the dialog knows a topic.
    fn stock() -> Vec<Offered> {
        vec![
            offered("cue_stack", "penumbra", "show/cues", "Steps through a list of looks."),
            offered("fade_all", "penumbra", "dmx/safety", "Fades every channel to zero."),
            offered("par_row", "penumbra", "dmx/fixtures", "A row of lamps."),
            offered("loose", "penumbra", "dmx", "Beside the second level."),
            offered("watchdog", "penumbra", "tools", "Warns when the frame rate drops."),
            offered("nameless", "penumbra", "", ""),
            offered("my_cue", "library", "show/cues", ""),
            offered("scratch", "project", "", ""),
        ]
    }

    fn choose(top: &str, sub: Option<&str>) -> ShelfChoice {
        let mut choice = ShelfChoice { top: top.into(), ..Default::default() };
        if let Some(sub) = sub {
            choice.sub.insert(top.into(), sub.into());
        }
        choice
    }

    #[test]
    fn shelves_come_from_the_components() {
        let found = shelves(&stock());
        let order: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(order, ["mine", "dmx", "show", "tools", "other"], "mine first, other last");
        let dmx = &found[1];
        assert_eq!(dmx.count, 3);
        assert_eq!(dmx.subs, [("fixtures".into(), 1), ("safety".into(), 1), ("other".into(), 1)]);
        assert!(found[3].subs.is_empty(), "one level, no second row");
        let mine = &found[0];
        assert_eq!(mine.count, 2);
        assert_eq!(
            mine.subs,
            [("show".to_string(), 1), ("other".to_string(), 1)],
            "by their own first level"
        );
        assert_eq!(levels(" a / b /c"), (Some("a"), Some("b")), "two levels, trimmed");
        assert_eq!(levels("/"), (None, None));
    }

    #[test]
    fn the_tab_shows_one_shelf() {
        let types = penumbra_ops::builtin_types().unwrap();
        let library = stock();
        let found = shelves(&library);
        let usable = Usable { types: &types, in_component: false, library: &library, family: None };
        let on = |c: &ShelfChoice| listed(&library, &found, c, usable, "");
        assert_eq!(names(&on(&choose("dmx", Some("safety")))), ["fade_all"]);
        assert_eq!(names(&on(&choose("dmx", Some("other")))), ["loose"], "beside the second level");
        assert_eq!(names(&on(&choose("dmx", None))), ["par_row"], "the first second level by default");
        assert_eq!(names(&on(&choose("show", Some("cues")))), ["cue_stack", "my_cue"], "the user's too");
        assert_eq!(names(&on(&choose("mine", Some("show")))), ["my_cue"]);
        assert_eq!(names(&on(&choose("mine", Some("other")))), ["scratch"]);
        assert_eq!(names(&on(&choose("gone", None))), ["my_cue"], "a shelf that left: the first");
        assert_eq!(names(&on(&choose("other", None))), ["nameless"]);
    }

    #[test]
    fn the_search_reaches_every_shelf_and_the_about_lines() {
        let types = penumbra_ops::builtin_types().unwrap();
        let library = stock();
        let found = shelves(&library);
        let usable = Usable { types: &types, in_component: false, library: &library, family: None };
        let shelf = choose("tools", None);
        let search = |f: &str| listed(&library, &found, &shelf, usable, f);
        assert_eq!(names(&search("fade")), ["fade_all"], "from another shelf");
        assert_eq!(names(&search("frame rate")), ["watchdog"], "by what it says");
        assert_eq!(names(&search("safety")), ["fade_all"], "by its shelf");
        assert_eq!(names(&search("cue")), ["cue_stack", "my_cue"]);
        assert_eq!(names(&search("zzz")), Vec::<&str>::new());
        assert_eq!(names(&search("ro")), ["par_row"], "two letters look at names only");
        let lamps = search("lamps");
        let kinds: Vec<String> = lamps.iter().map(|o| format!("@{}", o.name)).collect();
        let first = enter_choice(kinds.iter().map(String::as_str), usable, "lamps");
        assert_eq!(first.as_deref(), Some("@par_row"), "Enter takes one found by its line");
    }

    #[test]
    fn a_broken_preview_shows_no_picture() {
        assert!(read_preview(Path::new("no such file.png")).is_none());
        let file = std::env::temp_dir().join(format!("penumbra-preview-{}.png", std::process::id()));
        std::fs::write(&file, b"not a picture").unwrap();
        assert!(read_preview(&file).is_none());
        std::fs::remove_file(&file).unwrap();
    }

    #[test]
    fn the_component_ports_are_offered_only_inside_a_component() {
        let types = penumbra_ops::builtin_types().unwrap();
        let top = Usable { types: &types, in_component: false, library: &[], family: None };
        let inside = Usable { types: &types, in_component: true, library: &[], family: None };
        assert!(!top.allows("tex.in") && !top.allows("chan.out"));
        assert!(top.allows("tex.noise") && top.allows("out.video"));
        assert!(inside.allows("tex.in") && inside.allows("tex.noise"));
        assert!(!top.hit("tex.in", "tex.in"), "the search passes over a port at the top level");
        assert!(inside.hit("tex.in", "tex.in"));
    }

    #[test]
    fn a_tag_finds_a_type_its_name_does_not() {
        let types = penumbra_ops::builtin_types().unwrap();
        let u = Usable { types: &types, in_component: false, library: &[], family: None };
        assert!(u.hit("geo.file", "fbx"), "the extensions geo.file reads are its tags");
        assert_eq!(u.tag("geo.file", "fbx").as_deref(), Some("fbx"));
        assert!(!u.hit("tex.blur", "fbx"));
        assert!(u.hit("tex.slope", "slope") && u.tag("tex.slope", "slope").is_none(), "by its name");
        assert!(u.hit("chan.derivative", "slope"), "and another by a tag, though a name has the text");
        assert!(u.hit("tex.from_chan", "chopto") && u.hit("tex.from_chan", "chop to"));
        assert!(!u.hit("tex.from_chan", "to"), "a tag is found from its start");
        assert!(u.hit("tex.lens_distort", "lens distort"), "spaces count as nothing");
        assert!(u.tag("tex.lens_distort", "lens distort").is_none());
    }

    #[test]
    fn a_touchdesigner_family_word_narrows_the_search_to_its_family() {
        let types = penumbra_ops::builtin_types().unwrap();
        let search = types.search("noise top");
        assert_eq!((search.text.as_str(), search.family), ("noise", Some("tex")));
        assert_eq!(types.search("chop lfo").family, Some("chan"), "at the start as well");
        assert_eq!(types.search("noise").family, None, "a plain search searches every family");
        assert_eq!(types.search("chop to").family, None, "a tag holding the word wins");
        assert_eq!(types.search("top").family, None, "the word alone is a plain search");
        for (text, kind) in
            [("osc out chop", "out.osc"), ("midi out chop", "out.midi"), ("dmx out chop", "out.dmx")]
        {
            assert_eq!(types.search(text).family, None, "{text}: its CHOP is an output here");
            assert!(types.tag_found(kind, text).is_some(), "{text}");
        }
        let u = Usable { types: &types, in_component: false, library: &[], family: search.family };
        assert!(u.hit("tex.noise", &search.text) && !u.hit("chan.noise", &search.text));
        let groups = groups(&types);
        let tex = groups.iter().position(|g| g.iter().any(|f| f == "tex"));
        let kinds = |g: usize| groups[g].iter().flat_map(|f| kinds_of(&types, f, &[])).collect::<Vec<_>>();
        let first = tex.map(|g| enter_choice(kinds(g).iter().map(String::as_str), u, &search.text));
        assert_eq!(first, Some(Some("tex.noise".to_string())));
    }

    #[test]
    fn tab_passes_over_tabs_without_a_match() {
        let tabs = [false, true, false, true];
        let next = |active: usize, back: bool| next_tab(&tabs, active, back, |t| *t);
        assert_eq!(next(1, false), 3);
        assert_eq!(next(3, false), 1, "all the way round");
        assert_eq!(next(1, true), 3, "Shift+Tab goes back");
        assert_eq!(next(0, true), 3);
        assert_eq!(next_tab(&[true, false], 0, false, |t| *t), 0, "the only match stays");
        assert_eq!(next_tab(&[false, false, false], 0, false, |t| *t), 1, "no match: the next one");
        assert_eq!(next_tab(&[false, false, false], 0, true, |t| *t), 2);
        assert_eq!(next_tab::<bool>(&[], 0, false, |t| *t), 0);
    }

    #[test]
    fn enter_takes_the_exact_name_before_one_that_only_contains_it() {
        let types = penumbra_ops::builtin_types().unwrap();
        let u = Usable { types: &types, in_component: false, library: &[], family: None };
        let kinds = ["tex.denoise", "tex.level", "tex.noise", "tex.lens_distort", "geo.file"];
        let choice = |f: &str| enter_choice(kinds.iter().copied(), u, f);
        assert_eq!(choice("noise").as_deref(), Some("tex.noise"));
        assert_eq!(choice("nois").as_deref(), Some("tex.noise"), "a name starting with the text");
        assert_eq!(choice("oise").as_deref(), Some("tex.denoise"), "else the first containing it");
        assert_eq!(choice("lens distort").as_deref(), Some("tex.lens_distort"));
        assert_eq!(choice("fbx").as_deref(), Some("geo.file"), "a tag last");
        assert_eq!(choice("zzz"), None);
    }
}
