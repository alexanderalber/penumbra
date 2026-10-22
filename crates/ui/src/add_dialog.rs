//! The "add node" dialog, after TouchDesigner's OP Create Dialog: a search field on top, a tab
//! per group of node families below it, all types of the active group in columns (each family
//! from a new column in its colour, alphabetical, top to bottom, then the next column), and the
//! summary of the type under the pointer at the bottom. Small families share a tab (nine tabs
//! were too many); the colours tell them apart, and a heading over each family's columns says
//! what it holds, since the colours alone left one guessing which block was which.
//!
//! Typing does not hide types: those whose name does not contain the text are greyed out, so
//! the grid keeps its shape and the eye its places. Families with a match are marked in their
//! tab (bright text, a bar underneath), the others dimmed; the dialog never switches the tab by
//! itself. Enter takes the first match of the active tab, Tab switches to the next tab, Escape or a
//! click outside closes. Opened by double-click on empty canvas or Tab in the editor, and by "Replace
//! node..." in the node menu, where choosing replaces that node.

use std::sync::Arc;

use egui::{
    Align2, Color32, CornerRadius, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};
use penumbra_graph::NodeTypes;

use crate::icons::{self, Icon};
use crate::{automation, theme};

/// Families always offered, by tab and in this order, even before they have node types; others
/// found in the registry follow alphabetically, one tab each.
const GROUPS: [&[&str]; 5] =
    [&["tex"], &["chan"], &["table"], &["geo", "mat", "light", "view"], &["script", "out", "comp"]];
/// Names of the tabs holding several families, by their first family; a tab of one family is named
/// by it.
const NAMES: [(&str, &str); 2] = [("geo", "rendering"), ("script", "system")];
/// The headings over the families of a shared tab; a family missing here is headed by its name.
const HEADINGS: [(&str, &str); 7] = [
    ("geo", "geometry"),
    ("mat", "materials"),
    ("light", "lights"),
    ("view", "cameras"),
    ("script", "scripts"),
    ("out", "outputs"),
    ("comp", "components"),
];
/// Height of the heading row.
const HEADING: f32 = 18.0;
/// Types per column before the next column starts.
const ROWS: usize = 12;
const CELL: egui::Vec2 = vec2(150.0, 22.0);
const MIN_WIDTH: f32 = 4.0 * 150.0;

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

/// The types of a tab, each family from a new column; empty families take no room.
fn cells(types: &NodeTypes, group: &[String], library: &[Offered]) -> Vec<Cell> {
    let mut out = Vec::new();
    let mut col = 0;
    for family in group {
        let kinds = kinds_of(types, family, library);
        let columns = kinds.len().div_ceil(ROWS);
        for (i, kind) in kinds.into_iter().enumerate() {
            out.push(Cell { col: col + i / ROWS, row: i % ROWS, kind, family: family.clone() });
        }
        col += columns;
    }
    out
}

/// The kinds of `family`, sorted by their short name; for `comp`, `base` first, then a gap
/// ([`GAP`]) and the component files on offer as `@name`.
fn kinds_of(types: &NodeTypes, family: &str, library: &[Offered]) -> Vec<String> {
    let mut kinds: Vec<String> = types
        .kinds()
        .filter(|k| k.split_once('.').is_some_and(|(f, _)| f == family))
        // Only loading a project makes a placeholder of a node that did not load.
        .filter(|k| *k != penumbra_graph::broken::BROKEN_KIND)
        .map(str::to_string)
        .collect();
    kinds.sort_by_key(|k| (*k != penumbra_ops::comp::COMP_KIND, short(k).to_lowercase()));
    if family == "comp" && !library.is_empty() {
        kinds.push(GAP.to_string());
        kinds.extend(library.iter().map(|o| format!("{LIBRARY_MARK}{}", o.name)));
    }
    kinds
}

/// A component file on offer and where it lies: `project` (the project's `components/`) or
/// `library` (the user's library).
#[derive(Clone, Debug, PartialEq)]
pub struct Offered {
    pub name: String,
    pub place: &'static str,
}

/// The empty cell between the `comp` tab's built-in types and the component files: a line, not
/// a choice. Once the family has more built-in types than `base`, or penumbra ships
/// components of its own, the tab gets a toggle between standard and user components instead.
const GAP: &str = "";

/// Marks a component from the library among the kinds of the `comp` tab.
pub const LIBRARY_MARK: char = '@';

fn short(kind: &str) -> &str {
    if let Some(name) = kind.strip_prefix(LIBRARY_MARK) {
        return name;
    }
    kind.split_once('.').map_or(kind, |(_, s)| s)
}

fn matches(kind: &str, filter: &str) -> bool {
    kind != GAP && (filter.is_empty() || kind.to_lowercase().contains(filter))
}

impl AddDialog {
    pub fn new(screen: Pos2, at: Pos2, family: &str) -> Self {
        Self { at, screen, filter: String::new(), focus: true, family: family.to_string(), title: None }
    }

    /// `library`: the component files on offer, shown in the `comp` tab; choosing one gives
    /// `@name`.
    pub fn show(
        &mut self,
        ui: &Ui,
        types: &NodeTypes,
        library: &[Offered],
        auto: &mut automation::Registry,
    ) -> Outcome {
        let groups = groups(types);
        let mut active = groups.iter().position(|g| g.contains(&self.family)).unwrap_or(0);
        let filter = self.filter.trim().to_lowercase();
        let mut outcome = Outcome::Open;

        // Keys first, so Tab does not move the focus out of the search field.
        let (enter, escape, tab) = ui.input_mut(|i| {
            (
                i.key_pressed(Key::Enter),
                i.consume_key(egui::Modifiers::NONE, Key::Escape),
                i.consume_key(egui::Modifiers::NONE, Key::Tab),
            )
        });
        if tab && !groups.is_empty() {
            active = (active + 1) % groups.len();
            self.focus = true;
        }
        let group: &[String] = groups.get(active).map_or(&[], Vec::as_slice);
        if !group.contains(&self.family) {
            self.family = group.first().cloned().unwrap_or_default();
        }
        if escape {
            outcome = Outcome::Close;
        }

        let cells = cells(types, group, library);
        let first = cells.iter().map(|c| &c.kind).find(|k| matches(k, &filter)).cloned();
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
                    self.tabs(ui, &groups, active, types, library, &filter, auto);
                    ui.separator();
                    headings(ui, group, &cells, width, auto);
                    let grid = Grid {
                        cells: &cells,
                        group,
                        width,
                        filter: &filter,
                        first: first.as_deref(),
                        library,
                    };
                    let hovered = grid.show(ui, auto, &mut outcome);
                    ui.separator();
                    // The summary of the type under the pointer (or of Enter's choice).
                    let about = hovered.or(first.as_deref());
                    summary(ui, about, types, library, width);
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
                if r.clicked() {
                    *outcome = Outcome::Close;
                }
            });
        });
    }

    /// One tab per group, a segment per family coloured like its node headers; a named tab
    /// writes its name across them.
    #[allow(clippy::too_many_arguments)]
    fn tabs(
        &mut self,
        ui: &mut Ui,
        groups: &[Vec<String>],
        active: usize,
        types: &NodeTypes,
        library: &[Offered],
        filter: &str,
        auto: &mut automation::Registry,
    ) {
        ui.horizontal(|ui| {
            // Wider than a seam, so a tab of several families reads as one.
            ui.spacing_mut().item_spacing.x = 6.0;
            for (g, families) in groups.iter().enumerate() {
                let r = tab(ui, families, g == active, types, library, filter, auto);
                if r.clicked() {
                    self.family = families.first().cloned().unwrap_or_default();
                    self.focus = true;
                }
            }
        });
    }
}

/// One tab of the dialog; `families` its families, `filter` the search text.
fn tab(
    ui: &mut Ui,
    families: &[String],
    active: bool,
    types: &NodeTypes,
    library: &[Offered],
    filter: &str,
    auto: &mut automation::Registry,
) -> egui::Response {
    let name = tab_name(families);
    let named = families.len() > 1;
    // While searching: which families have a match?
    let segments: Vec<(&String, bool, Option<Arc<egui::Galley>>)> = families
        .iter()
        .map(|f| {
            let kinds = kinds_of(types, f, library);
            let hit = !filter.is_empty() && kinds.iter().any(|k| matches(k, filter));
            let bright = if filter.is_empty() { !kinds.is_empty() } else { hit };
            let text = if bright { Color32::WHITE } else { theme::TEXT_WEAK };
            let galley =
                (!named).then(|| ui.painter().layout_no_wrap(f.clone(), FontId::proportional(14.0), text));
            (f, hit, galley)
        })
        .collect();
    let title =
        named.then(|| ui.painter().layout_no_wrap(name.clone(), FontId::proportional(14.0), Color32::WHITE));
    // A named tab shares its width out evenly among its families.
    let even = title.as_ref().map_or(0.0, |t| (t.size().x + 24.0) / segments.len() as f32);
    let widths: Vec<f32> = segments
        .iter()
        .map(|(_, _, t)| t.as_ref().map_or(even.max(28.0), |t| (t.size().x + 16.0).max(64.0)))
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

/// A tab of several families: a heading over each, with a line in its colour across its
/// columns.
fn headings(ui: &mut Ui, group: &[String], cells: &[Cell], width: f32, auto: &mut automation::Registry) {
    if group.len() <= 1 {
        return;
    }
    let (row, _) = ui.allocate_exact_size(vec2(width, HEADING), Sense::hover());
    let painter = ui.painter_at(row);
    for family in group {
        let columns: Vec<usize> = cells.iter().filter(|c| &c.family == family).map(|c| c.col).collect();
        let (Some(first), Some(last)) = (columns.iter().min(), columns.iter().max()) else {
            continue;
        };
        let left = row.min.x + *first as f32 * CELL.x + 1.0;
        let right = row.min.x + (*last + 1) as f32 * CELL.x - 1.0;
        let heading = HEADINGS.iter().find(|(f, _)| f == family).map_or(family.as_str(), |(_, h)| *h);
        let r = painter.text(
            pos2(left + 5.0, row.center().y - 1.0),
            Align2::LEFT_CENTER,
            heading,
            FontId::proportional(13.0),
            theme::TEXT,
        );
        auto.add(format!("add.heading.{family}"), r, heading);
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
}

impl<'a> Grid<'a> {
    /// Draws the grid; a click on a type chooses it. Returns the type under the pointer.
    fn show(&self, ui: &mut Ui, auto: &mut automation::Registry, outcome: &mut Outcome) -> Option<&'a str> {
        let rows = self.cells.iter().map(|c| c.row + 1).max().unwrap_or(1);
        let (grid, _) = ui.allocate_exact_size(vec2(self.width, rows as f32 * CELL.y), Sense::hover());
        let painter = ui.painter_at(grid);
        if self.cells.is_empty() {
            painter.text(
                grid.left_top() + vec2(6.0, CELL.y / 2.0),
                Align2::LEFT_CENTER,
                format!("no {} node types yet", self.group.join(" or ")),
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
            let r = ui.interact(cell, ui.id().with(("add", kind.as_str())), Sense::click());
            auto.add(format!("add.{kind}"), cell, kind.as_str());
            let hit = matches(kind, self.filter);
            let fill = match (hit, r.hovered()) {
                (_, true) => color.gamma_multiply(1.4),
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
            if r.clicked() && !removing {
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
            painter.text(
                cell.right_center() - vec2(6.0, 0.0),
                Align2::RIGHT_CENTER,
                place,
                FontId::proportional(11.0),
                theme::TEXT_WEAK,
            );
        }
        x.clicked()
    }
}

/// The summary of the type or component file `about`, in one line.
fn summary(ui: &mut Ui, about: Option<&str>, types: &NodeTypes, library: &[Offered], width: f32) {
    let text = about
        .and_then(|k| match k.strip_prefix(LIBRARY_MARK) {
            Some(name) => {
                let place = library.iter().find(|o| o.name == name).map_or("", |o| o.place);
                let place = if place == "project" { "the project's components/" } else { "the library" };
                Some(format!("{name}: a component file from {place} (comp.load)"))
            }
            None => types.desc(k).map(|d| format!("{k}: {}", d.summary)),
        })
        .unwrap_or_default();
    ui.allocate_ui_with_layout(vec2(width, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.add(egui::Label::new(egui::RichText::new(text).color(theme::TEXT_WEAK)).truncate())
    });
}
