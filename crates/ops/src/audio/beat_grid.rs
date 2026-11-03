//! `chan.beat_grid`: the beats and bars of the sound file an `audio.file` plays, read at its
//! playhead. The beats come from the Beat This! network ([`penumbra_audio::beats`]), found once
//! per file in the background and kept in a cache on disk.
//!
//! **Wiring:** the node's `audio` input takes an `audio.file`'s `out`, the same wire a
//! `chan.beat_analysis` takes. The node does not listen to the samples: it asks [`SoundFiles`]
//! which file the node at the other end plays and where its playhead stands.
//!
//! **Analyses** belong to [`BeatFiles`], one per sound file, shared by every node and command
//! that wants it. Each runs on a thread of its own and hands on the beats that are final so far
//! while it runs ([`beats::pick_final`]), so the grid can be read long before a two-hour mix is
//! done. An analysis stops when the last node or command that wanted it lets go, when it is
//! cancelled, and when the app closes. A finished one stays in memory for the next node that
//! wants the same file, until the file changes on disk.
//!
//! **Folding:** passages the network counted at twice or half the tempo of the minute around
//! them come back to that tempo before anything reads the grid ([`beats::fold`]).
//!
//! **Bars** are voted over 64 beats from the network's downbeat logits ([`beats::bars`]), so
//! they hold on DJ mixes, where the downbeats the network marks one by one do not.
//!
//! **Envelopes:** `level`, `low`, `mid`, `high`, `onset` and `kick` are what a
//! `chan.beat_analysis` with its default settings gives for the same sound, measured over the
//! whole file once beside the beats ([`beats::analyze_envelopes`]) and read with the analysis'
//! own lag taken out: the reading at the playhead is what the live node reports some 20 to 30
//! ms later. A moment not analysed yet reads 0.
//!
//! **Sections:** once the grid and the envelopes are complete, the sections of the file are
//! found from both ([`Sections::of`], about a tenth of a second for a two-hour mix), on the
//! thread that finished last. The section channels say where the playhead stands among them;
//! they read 0, and `sections_ready` 0, before. A looped file looks them up by the time within
//! its pass, so its end is a boundary like any other: the countdowns reach 0 and `build` 1
//! there, and the first section begins again.
//!
//! **Music bus:** with the sections come curves of the whole file, pure functions of the time
//! in it ([`Bus`]): the beats travelled at the grid's smoothed and clamped tempo, how a grid beat
//! compares to a travelled one, how far the bars drift from the beats travelled, energies
//! normalised over the whole file, each section's energy, drop and character (role, change,
//! bass, onsets per beat, share of beats with a kick). They read 0 before. A
//! `travel_floor` or `travel_cap` other than the defaults computes the travel again on the
//! cook thread, once per change.
//!
//! **Time:** the grid is known ahead, so `offset` moves the lookup itself: a tick comes exactly
//! `offset` early, with no prediction. A looped file's grid runs from its last beat into the
//! first beat of the next pass ([`BeatGrid::at_looped`]). Per cook the node costs one binary
//! search and a few hops of envelopes.
//!
//! **Overview:** the `overview` output is the whole file as a picture ([`super::overview`]):
//! waveform, bars, sections and playhead, the same the viewer shows. It is drawn only while
//! something reads it.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Instant, SystemTime};

use penumbra_audio::beats::{
    self, BeatError, BeatGrid, BeatModel, Beats, Bus, Envelopes, Partial, Sections, TRAVEL_CAP, TRAVEL_FLOOR,
    Travel,
};
use penumbra_core::{Channel, Chans, Error, Name, NodeId, ParamSpec, Table, lock};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDef, NodeDesc,
    NodeTypes, Operator, Waker,
};

use super::file::SoundFiles;
use super::overview::{self, ColumnCache, Overview};
use crate::meters::{Palette, Rect};
use crate::tex::meters_picture::{self, ShapePicture};

/// Kind of the grid node.
pub const BEAT_GRID: &str = "chan.beat_grid";
/// The index of the `grid` output.
const GRID: usize = 1;
/// The index of the `overview` output.
const OVERVIEW: usize = 2;
/// The index of the `sections` output.
const SECTIONS: usize = 3;
/// The params' section of the overview.
const OVERVIEW_GROUP: &str = "overview";
/// A lookup that moved further than this since the last cook is a jump, not a beat passing.
const JUMP_SECONDS: f64 = 0.5;
/// Finished analyses kept for nodes to come.
const KEEP_DONE: usize = 32;
/// Progress steps that wake the nodes.
const WAKE_STEP: f64 = 0.01;
/// Cores the analysis leaves free for sound, pictures and the rest of the app.
const SPARE_CORES: usize = 4;

/// The threads every analysis runs its network on: fewer than the cores and at the lowest
/// priority, so a long analysis never starves the sound output. `None` when the pool cannot be
/// built; the analysis then runs on the calling thread alone.
fn pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        rayon::ThreadPoolBuilder::new()
            .num_threads(cores.saturating_sub(SPARE_CORES).max(1))
            .thread_name(|i| format!("beats-{i}"))
            .start_handler(|_| {
                penumbra_system::process::lower_thread_priority();
            })
            .build()
            .map_err(|e| log::warn!("beat analysis threads: {e}"))
            .ok()
    })
    .as_ref()
}

/// The channels `chan.beat_grid` outputs, in order. The first eight and `level` to `kick` mean
/// what they mean on `chan.beat_analysis`, so a binding keeps working when one node replaces
/// the other. New channels go at the end.
pub const CHANNELS: [&str; 51] = [
    "beat",
    "tick",
    "bar",
    "bar_beat",
    "bpm",
    "beats_per_bar",
    "locked",
    "coasting",
    "downbeat",
    "beat_index",
    "bar_index",
    "ready",
    "progress",
    "analyzing",
    "level",
    "low",
    "mid",
    "high",
    "onset",
    "kick",
    "section_index",
    "section_label",
    "section_letter",
    "track_index",
    "phrase_bar",
    "phrase",
    "bars_to_boundary",
    "seconds_to_boundary",
    "build",
    "section_seed",
    "transition",
    "transition_from",
    "transition_to",
    "sections_ready",
    "seconds",
    "travelled",
    "travel_bpm",
    "beat_length",
    "drift",
    "energy",
    "energy_low",
    "energy_mid",
    "energy_high",
    "section_energy",
    "drop",
    "section_role",
    "section_change",
    "section_bass",
    "section_density",
    "section_kick",
    "track_phase",
];
/// Where the envelopes begin in [`CHANNELS`].
const LEVEL: usize = 14;
/// Where the sections begin in [`CHANNELS`].
const SECTION: usize = 20;
/// The section channels, `section_index` to `sections_ready`.
const SECTION_CHANNELS: usize = 14;
/// Where `seconds` stands in [`CHANNELS`].
const SECONDS: usize = SECTION + SECTION_CHANNELS;
/// Where the music bus begins in [`CHANNELS`]: `travelled` to `section_kick`.
const BUS: usize = SECONDS + 1;
/// The bus channels.
const BUS_CHANNELS: usize = 15;
/// Where `track_phase` stands in [`CHANNELS`].
const TRACK_PHASE: usize = BUS + BUS_CHANNELS;
/// The highest tempo `travel_floor` and `travel_cap` take, beats per minute.
const MAX_TRAVEL_BPM: f64 = 1000.0;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let [width, height, keep_size] = meters_picture::size_params(OVERVIEW_GROUP, "overview");
    types.register(FnNodeType::new(
        NodeDesc::new(
            BEAT_GRID,
            "The beats and bars of the sound file an audio.file plays, found ahead of time, as numbers to animate with",
        )
        .param(
            ParamSpec::float("offset", 0.0)
                .clamp(-5000.0, 5000.0)
                .range(-200.0, 200.0)
                .doc("Milliseconds to move beat and bar ahead by, to make up for the projector's delay. Ticks come exactly this much early. Negative values move them later"),
        )
        .param(
            ParamSpec::float("travel_floor", TRAVEL_FLOOR)
                .clamp(0.0, MAX_TRAVEL_BPM)
                .range(0.0, 300.0)
                .doc("Slowest tempo travelled moves by, in beats per minute. A slower grid, or a long gap between beats, travels at this tempo. 0 sets no floor"),
        )
        .param(
            ParamSpec::float("travel_cap", TRAVEL_CAP)
                .clamp(0.0, MAX_TRAVEL_BPM)
                .range(0.0, 300.0)
                .doc("Fastest tempo travelled moves by, in beats per minute. A faster grid travels at this tempo. 0 sets no cap. A travel_floor above the cap wins"),
        )
        .param(
            ParamSpec::float("span", overview::DEFAULT_SPAN)
                .clamp(overview::MIN_SPAN, overview::MAX_SPAN)
                .range(10.0, 600.0)
                .curve(penumbra_core::Curve::Log)
                .unit(penumbra_core::Unit::Seconds)
                .group(OVERVIEW_GROUP)
                .doc("Seconds the lower lane of the overview shows around the playhead, in the viewer and in the overview output"),
        )
        .param(width)
        .param(height)
        .param(keep_size)
        .input("audio", DataKind::Audio)
        .describe_input("audio", "The out of an audio.file, wired straight from it")
        .output("out", DataKind::Chan)
        .output("grid", DataKind::Table)
        .lazy_output("grid")
        .output("overview", DataKind::Tex)
        .lazy_output("overview")
        .output("sections", DataKind::Table)
        .lazy_output("sections")
        .describe_output(
            "out",
            "Channels to bind: beat and bar rise from 0 to 1, tick and downbeat are 1 in the frame a beat or bar begins, bar_beat counts the beats in the bar from 0, \
             beat_index and bar_index count from the first beat and the first downbeat; also bpm, beats_per_bar, locked, coasting. \
             level, low, mid, high, onset and kick are what chan.beat_analysis gives for this sound, without its lag of 20 to 30 ms. \
             ready is 1 where the grid is final, progress goes from 0 to 1 while the file is analysed, analyzing is 1 meanwhile. \
             The section channels say where the playhead stands among the file's tracks and sections. section_index and track_index count from 0. \
             section_label numbers the kind of sound: the same number anywhere in the file where it sounds alike. \
             section_letter numbers the repeats within a track (0 is A). phrase_bar counts the bars of an 8-bar phrase from 0, phrase rises from 0 to 1 over it. \
             bars_to_boundary and seconds_to_boundary count down to the next section, build rises from 0 to 1 over the last 8 bars before it. \
             section_seed is a number fixed per section_label, for random looks that come back with the music. \
             transition rises from 0 to 1 through the blend from one section into the next, transition_from and transition_to are their labels. \
             sections_ready is 1 once the sections are found, after the whole file is analysed. Until then the section channels read 0. \
             seconds is the time into the file the channels are read at: the playhead plus offset, within the pass for a looped file, never below 0. \
             Bind a chan.sequence's clock to it, and its steps follow the music through seeks and loops. \
             travelled counts the beats travelled since the file's start. It rises at travel_bpm: the grid's tempo, kept between travel_floor and travel_cap and smoothed over 1.5 s. \
             A world moved by travelled moves with the music, steadily through breaks and tempo changes. travelled starts at 0 again in each pass of a looped file. \
             beat_length is how many travelled beats one grid beat takes: 1 where the tempo is steady. \
             drift is the bars the grid counts (bar_index plus bar) minus travelled divided by the bar length most bars of the file have. It changes slowly. \
             An event lands on a downbeat where travelled divided by that bar length, plus drift, is a whole number. \
             energy, energy_low, energy_mid and energy_high are level, bass, mid and treble from 0 (the quietest 5 % of the file) to 1 (the loudest 5 %), smoothed (0.05 s up, 0.3 s down). \
             Unlike level, they compare across the whole file. section_energy is the mean energy of the section under the playhead. \
             drop is 1 where a section begins with at least 0.2 more section_energy than the section before, and falls to 37 % in 0.5 s. \
             section_change says how strongly the music changes where the section under the playhead begins, from 0 to 1: scale a change of the look by it. \
             Track starts read near 1, drops and breakdowns about 0.5 to 1, small changes within a track mostly below 0.3. \
             section_role is what the section does in its track: 0 intro, 1 break, 2 build, 3 drop, 4 outro, 5 main. \
             section_bass is the section's mean energy_low, section_density its onsets per beat, section_kick the share of its beats with a kick (0 to 1). \
             These read 0 until the sections are found.              track_phase rises from 0 to 1 through the track under the playhead, from its first section's start to its last section's end",
        )
        .describe_output(
            "grid",
            "One row per beat found so far, under a header row: time (seconds into the file), beat, bar, beat_in_bar, downbeat (1 or 0)",
        )
        .describe_output(
            "overview",
            "The whole file as a picture, as the viewer shows it: the level as a waveform coloured by bass (red), mid (green) and treble (blue), bars, sections and the playhead. \
             The strip on top shows the sections, coloured by section_label, with their letter in the lower lane. \
             The lower lane shows span seconds around the playhead. Grey: levels not measured yet. Veiled: grid not final yet. Drawn only while something reads this output",
        )
        .describe_output(
            "sections",
            "One row per section, under a header row, once the whole file is analysed: start and end (seconds into the file), bars, \
             label, letter (0 is A) and track as the channels count them, strength (0 to 1: how sharply the sound changes at the start), \
             zone_start and zone_end (seconds: the blend from the section before; equal at a cut), energy (the section's section_energy, 0 to 1), \
             bass, density and kick (as section_bass, section_density and section_kick), jump (the section's energy minus the one before's), \
             role (intro, break, build, drop, outro or main) and change (as section_change)",
        ),
        || Box::new(GridNode::default()) as Box<dyn Operator>,
    )
    .warm_up(crate::on_gpu(meters_picture::warm_up)))
}

/// A line on how the analysis of a file stands, for the overview: `None` once both analyses
/// ended well.
pub fn status_line(s: &State) -> Option<String> {
    let complete = s.grid.as_ref().is_some_and(|g| g.complete());
    if s.running {
        Some(format!("finding beats {:.0} %", s.progress * 100.0))
    } else if let Some(e) = &s.error {
        Some(format!("no beats: {e}"))
    } else if !complete {
        Some("beat analysis cancelled".to_string())
    } else if s.envelopes_running {
        Some(format!("measuring levels {:.0} %", s.envelope_progress * 100.0))
    } else if s.sections_running {
        Some("finding sections".to_string())
    } else if let Some(e) = &s.envelope_error {
        Some(format!("no levels: {e}"))
    } else {
        s.sections_error.as_ref().map(|e| format!("no sections: {e}"))
    }
}

/// The overview of a file as [`overview::shapes`] wants it, from its analysis and its player.
pub fn overview_of<'a>(
    grid: Option<&'a Grid>,
    envelopes: Option<&'a Envelopes>,
    duration: f64,
    playhead: Option<f64>,
    span: f64,
    sections: Option<&'a Structure>,
    status: Option<&'a str>,
) -> Overview<'a> {
    Overview {
        duration,
        playhead,
        beats: grid.map_or(&[], |g| g.grid.beats()),
        bars: grid.map_or(&[], |g| g.bars.as_slice()),
        final_until: grid.map(|g| g.final_until),
        envelopes,
        sections: sections.map_or(&[], |s| s.strip.as_slice()),
        span,
        status,
    }
}

/// The beats of a file as far as they are known.
#[derive(Debug)]
pub struct Grid {
    pub beats: Beats,
    pub grid: BeatGrid,
    /// The grid's bar starts, seconds into the file: voted from the network's downbeat
    /// logits, so they need not be `beats.downbeats`.
    pub bars: Vec<f64>,
    /// Seconds into the file before which the grid is final; infinite once the analysis ended,
    /// minus infinity before two bars are known.
    pub final_until: f64,
}

impl Grid {
    pub fn new(beats: Beats, final_until: f64) -> Self {
        let grid = BeatGrid::from_beats(&beats);
        let bars = grid.downbeats();
        Grid { beats, grid, bars, final_until }
    }

    /// The analysis ended and this is all of it.
    pub fn complete(&self) -> bool {
        self.final_until == f64::INFINITY
    }

    /// The grid as a table: a header row, then one row per beat.
    pub fn table(&self) -> Result<Table, Error> {
        let mut rows = Vec::with_capacity(self.grid.beats().len() + 1);
        rows.push(["time", "beat", "bar", "beat_in_bar", "downbeat"].map(str::to_string).to_vec());
        for t in self.grid.beats() {
            let Some(p) = self.grid.at(*t) else { continue };
            rows.push(vec![
                format!("{t:.3}"),
                p.beat.to_string(),
                p.bar.to_string(),
                p.beat_in_bar.to_string(),
                if p.beat_in_bar == 0 { "1" } else { "0" }.to_string(),
            ]);
        }
        Table::new(rows)
    }
}

/// The sections of a file, found once its grid and envelopes are complete.
#[derive(Debug)]
pub struct Structure {
    pub sections: Sections,
    /// The sections as the overview's strip draws them: coloured by label, with their letter.
    pub strip: Vec<overview::Section>,
}

impl Structure {
    pub fn new(sections: Sections) -> Self {
        let strip = sections
            .list()
            .iter()
            .map(|s| overview::Section {
                start: s.start,
                end: s.end,
                label: s.label.to_string(),
                text: letter(s.letter),
                track_start: s.track_start,
                zone: s.zone,
            })
            .collect();
        Structure { sections, strip }
    }

    /// The sections as a table: a header row, then one row per section. `energy` to `change` come
    /// from `bus`: 0 without it, and an empty `role`.
    pub fn table(&self, bus: Option<&Bus>) -> Result<Table, Error> {
        let mut rows = Vec::with_capacity(self.sections.len() + 1);
        rows.push(
            [
                "start",
                "end",
                "bars",
                "label",
                "letter",
                "track",
                "strength",
                "zone_start",
                "zone_end",
                "energy",
                "bass",
                "density",
                "kick",
                "jump",
                "role",
                "change",
            ]
            .map(str::to_string)
            .to_vec(),
        );
        let energies = bus.map_or(&[][..], |b| b.sections());
        let num = |x: Option<f64>| format!("{:.3}", x.unwrap_or(0.0));
        for (i, s) in self.sections.list().iter().enumerate() {
            let e = energies.get(i);
            rows.push(vec![
                format!("{:.3}", s.start),
                format!("{:.3}", s.end),
                s.bars.to_string(),
                s.label.to_string(),
                s.letter.to_string(),
                s.track.to_string(),
                format!("{:.3}", s.strength),
                format!("{:.3}", s.zone.0),
                format!("{:.3}", s.zone.1),
                num(e.map(|e| e.energy)),
                num(e.map(|e| e.bass)),
                num(e.map(|e| e.density)),
                num(e.map(|e| e.kick)),
                num(e.map(|e| e.jump)),
                e.map_or("", |e| e.role.name()).to_string(),
                num(e.map(|e| e.change)),
            ]);
        }
        Table::new(rows)
    }
}

/// A section's letter within its track as text: A to Z, then the number.
pub fn letter(n: usize) -> String {
    match u8::try_from(n).ok().filter(|n| *n < 26) {
        Some(n) => char::from(b'A' + n).to_string(),
        None => n.to_string(),
    }
}

/// The sections of a complete grid with complete envelopes.
pub(crate) fn find_sections(grid: &Grid, envelopes: &Envelopes) -> Result<Structure, String> {
    catch_unwind(AssertUnwindSafe(|| Sections::of(&grid.grid, envelopes)))
        .unwrap_or_else(|_| Err("finding the sections failed".into()))
        .map(Structure::new)
}

/// The music bus of a complete grid with complete envelopes, and of the sections when found,
/// at the default travel clamps.
pub(crate) fn find_bus(grid: &Grid, envelopes: &Envelopes, sections: Option<&Structure>) -> Option<Bus> {
    let sections = sections.map(|s| &s.sections);
    catch_unwind(AssertUnwindSafe(|| Bus::new(&grid.grid, envelopes, sections, TRAVEL_FLOOR, TRAVEL_CAP)))
        .map_err(|_| log::warn!("finding the music bus failed"))
        .ok()
}

/// How one file's analysis stands.
#[derive(Clone, Debug, Default)]
pub struct State {
    /// The analysis is running.
    pub running: bool,
    /// 0 to 1.
    pub progress: f64,
    /// The beats known so far; complete once the analysis ended.
    pub grid: Option<Arc<Grid>>,
    pub error: Option<String>,
    pub cancelled: bool,
    /// Seconds it ran (so far).
    pub seconds: f64,
    /// The envelopes known so far; complete once their analysis ended.
    pub envelopes: Option<Arc<Envelopes>>,
    /// The envelopes' analysis is running.
    pub envelopes_running: bool,
    /// 0 to 1.
    pub envelope_progress: f64,
    pub envelope_error: Option<String>,
    /// The sections, once grid and envelopes are complete and the sections are found.
    pub sections: Option<Arc<Structure>>,
    /// The sections are being found.
    pub sections_running: bool,
    pub sections_error: Option<String>,
    /// The music bus, found with the sections (also when they fail).
    pub bus: Option<Arc<Bus>>,
}

impl State {
    /// Either analysis is running, or the sections are being found.
    pub fn busy(&self) -> bool {
        self.running || self.envelopes_running || self.sections_running
    }

    /// The grid and the envelopes to find the sections from, when both are complete and the
    /// sections are neither found nor being found; claims the finding then.
    pub(crate) fn claim_sections(&mut self) -> Option<(Arc<Grid>, Arc<Envelopes>)> {
        if self.busy() || self.sections.is_some() || self.sections_error.is_some() {
            return None;
        }
        let grid = self.grid.clone().filter(|g| g.complete())?;
        let envelopes = self.envelopes.clone().filter(|e| e.complete())?;
        self.sections_running = true;
        Some((grid, envelopes))
    }

    /// The share of both analyses done: the lesser of the two, leaving out one that failed.
    pub fn progress_all(&self) -> f64 {
        if self.envelope_error.is_some() { self.progress } else { self.progress.min(self.envelope_progress) }
    }
}

/// What the thread and the handles share.
struct Shared {
    state: Mutex<State>,
    wakers: Mutex<BTreeMap<NodeId, Waker>>,
    started: Instant,
}

impl Shared {
    fn update(&self, f: impl FnOnce(&mut State)) {
        let mut s = lock(&self.state);
        f(&mut s);
        s.seconds = self.started.elapsed().as_secs_f64();
    }

    fn wake(&self) {
        for w in lock(&self.wakers).values() {
            w.wake();
        }
    }

    /// Applies the end of an analysis with `f` and wakes the nodes. When that was the last of
    /// the two to end, finds the sections on this thread.
    fn finish(&self, cancel: &AtomicBool, f: impl FnOnce(&mut State)) {
        let mut claimed = None;
        self.update(|s| {
            f(s);
            if !cancel.load(Ordering::Relaxed) {
                claimed = s.claim_sections();
            }
        });
        self.wake();
        let Some((grid, envelopes)) = claimed else { return };
        let found = find_sections(&grid, &envelopes);
        let bus = find_bus(&grid, &envelopes, found.as_ref().ok()).map(Arc::new);
        self.update(|s| {
            s.sections_running = false;
            s.bus = bus;
            match found {
                Ok(x) => s.sections = Some(Arc::new(x)),
                Err(e) => s.sections_error = Some(e),
            }
        });
        self.wake();
    }
}

/// One sound file's analysis, shared by the nodes and commands that want it. Dropping the last
/// handle stops it.
pub struct Analysis {
    file: PathBuf,
    /// The file's size and modification time when the analysis began.
    stamp: Option<(u64, SystemTime)>,
    cancel: Arc<AtomicBool>,
    shared: Arc<Shared>,
    /// A newer analysis of the file took this one's place.
    replaced: AtomicBool,
}

impl Analysis {
    pub fn file(&self) -> &Path {
        &self.file
    }

    pub fn state(&self) -> State {
        lock(&self.shared.state).clone()
    }

    /// The grid so far, the progress of both analyses, whether either runs, and the error once
    /// the beats ended in one.
    fn grid(&self) -> (Option<Arc<Grid>>, f64, bool, Option<String>) {
        let s = lock(&self.shared.state);
        (s.grid.clone(), s.progress_all(), s.busy(), s.error.clone().filter(|_| !s.running))
    }

    /// The envelopes so far.
    fn envelopes(&self) -> Option<Arc<Envelopes>> {
        lock(&self.shared.state).envelopes.clone()
    }

    /// The sections, once found.
    fn sections(&self) -> Option<Arc<Structure>> {
        lock(&self.shared.state).sections.clone()
    }

    /// The music bus, once found.
    fn bus(&self) -> Option<Arc<Bus>> {
        lock(&self.shared.state).bus.clone()
    }

    /// Stops it before its next chunk.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Whether a newer analysis of the same file took this one's place.
    pub fn replaced(&self) -> bool {
        self.replaced.load(Ordering::Relaxed)
    }

    /// Recooks `node` whenever the analysis moves on.
    fn wake_on_change(&self, node: NodeId, waker: Waker) {
        lock(&self.shared.wakers).insert(node, waker);
    }

    fn job_status(&self) -> JobStatus {
        let s = lock(&self.shared.state);
        if s.busy() {
            // `as`: a fraction for the progress bar.
            JobStatus::Running { progress: Some(s.progress_all() as f32) }
        } else if let Some(e) = &s.error {
            JobStatus::Failed { error: e.clone() }
        } else if s.grid.is_some() {
            JobStatus::Done
        } else {
            JobStatus::Idle
        }
    }

    /// Both analyses ended.
    fn ended(&self) -> bool {
        !lock(&self.shared.state).busy()
    }
}

impl Drop for Analysis {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn stamp(file: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(file).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// The beat analyses of sound files, one per file, for the `chan.beat_grid` nodes and the
/// `beats.*` commands. Provided by the engine as a cooker resource; cheap to clone.
#[derive(Clone)]
pub struct BeatFiles(Arc<Inner>);

struct Inner {
    /// Where results are kept on disk; `None` keeps none.
    cache: Option<PathBuf>,
    /// Every analysis some handle still holds.
    files: Mutex<BTreeMap<PathBuf, Weak<Analysis>>>,
    /// Analyses held without a node: by commands until they end, and the last finished ones.
    held: Mutex<Vec<Arc<Analysis>>>,
    /// The network, loaded while an analysis needs it.
    model: Mutex<Weak<BeatModel>>,
    stopped: AtomicBool,
}

impl BeatFiles {
    /// Results are kept in `cache` (a directory, made when needed).
    pub fn new(cache: Option<PathBuf>) -> Self {
        BeatFiles(Arc::new(Inner {
            cache,
            files: Mutex::new(BTreeMap::new()),
            held: Mutex::new(Vec::new()),
            model: Mutex::new(Weak::new()),
            stopped: AtomicBool::new(false),
        }))
    }

    /// The analysis of `file`: the one running or finished, else a new one. A finished or
    /// failed one is begun anew when the file changed on disk since.
    pub fn want(&self, file: &Path) -> Arc<Analysis> {
        let mut files = lock(&self.0.files);
        if let Some(a) = files.get(file).and_then(Weak::upgrade)
            && !(a.ended() && a.stamp != stamp(file))
        {
            return a;
        }
        self.begin(&mut files, file)
    }

    /// Retires the analysis of `file` (after a cancel or a failure): it stops, and its nodes
    /// and the next [`BeatFiles::want`] begin a new one.
    pub fn restart(&self, file: &Path) {
        let old = lock(&self.0.files).remove(file).and_then(|w| w.upgrade());
        if let Some(old) = old {
            old.replaced.store(true, Ordering::Relaxed);
            old.cancel();
            lock(&self.0.held).retain(|h| !Arc::ptr_eq(h, &old));
            old.shared.wake();
        }
    }

    /// [`BeatFiles::want`], and kept going without a node until it ends.
    pub fn hold(&self, file: &Path) -> Arc<Analysis> {
        let a = self.want(file);
        let mut held = lock(&self.0.held);
        if !held.iter().any(|h| Arc::ptr_eq(h, &a)) {
            held.push(a.clone());
        }
        a
    }

    /// The analysis of `file`, if some node or command holds it.
    pub fn get(&self, file: &Path) -> Option<Arc<Analysis>> {
        lock(&self.0.files).get(file).and_then(Weak::upgrade)
    }

    /// Every analysis held, by file.
    pub fn list(&self) -> Vec<(PathBuf, State)> {
        lock(&self.0.files).iter().filter_map(|(f, w)| w.upgrade().map(|a| (f.clone(), a.state()))).collect()
    }

    /// Stops `file`'s analysis. Its nodes keep the beats found so far and do not begin again
    /// until [`BeatFiles::restart`]. Whether one was running.
    pub fn cancel(&self, file: &Path) -> bool {
        let Some(a) = self.get(file) else { return false };
        let running = !a.ended();
        a.cancel();
        lock(&self.0.held).retain(|h| !Arc::ptr_eq(h, &a) || h.ended());
        running
    }

    /// Stops every analysis; none begins after this. For the app's end.
    pub fn shutdown(&self) {
        self.0.stopped.store(true, Ordering::Relaxed);
        for w in lock(&self.0.files).values() {
            if let Some(a) = w.upgrade() {
                a.cancel();
            }
        }
        lock(&self.0.held).clear();
    }

    fn begin(&self, files: &mut BTreeMap<PathBuf, Weak<Analysis>>, file: &Path) -> Arc<Analysis> {
        if let Some(old) = files.get(file).and_then(Weak::upgrade) {
            // The file changed on disk since it ended.
            old.replaced.store(true, Ordering::Relaxed);
            lock(&self.0.held).retain(|h| !Arc::ptr_eq(h, &old));
        }
        files.retain(|_, w| w.strong_count() > 0);
        let shared = Arc::new(Shared {
            state: Mutex::new(State { running: true, envelopes_running: true, ..State::default() }),
            wakers: Mutex::new(BTreeMap::new()),
            started: Instant::now(),
        });
        let a = Arc::new(Analysis {
            file: file.to_path_buf(),
            stamp: stamp(file),
            cancel: Arc::new(AtomicBool::new(false)),
            shared: shared.clone(),
            replaced: AtomicBool::new(false),
        });
        files.insert(file.to_path_buf(), Arc::downgrade(&a));
        if self.0.stopped.load(Ordering::Relaxed) {
            shared.update(|s| {
                s.running = false;
                s.envelopes_running = false;
                s.cancelled = true;
            });
            return a;
        }
        self.begin_envelopes(file, &a);
        let (inner, path, cancel) = (self.0.clone(), file.to_path_buf(), a.cancel.clone());
        let spawned = std::thread::Builder::new().name("beats".into()).spawn(move || {
            penumbra_system::process::lower_thread_priority();
            let work = || run(&inner, &path, &cancel, &shared);
            let result = catch_unwind(AssertUnwindSafe(|| match pool() {
                Some(p) => p.install(work),
                None => work(),
            }))
            .unwrap_or_else(|_| Err(BeatError::Network("the beat analysis failed".into())));
            shared.finish(&cancel, |s| {
                s.running = false;
                match result {
                    Ok(b) => {
                        s.grid = Some(Arc::new(Grid::new(b, f64::INFINITY)));
                        s.progress = 1.0;
                    }
                    Err(BeatError::Cancelled) => s.cancelled = true,
                    Err(e) => s.error = Some(e.to_string()),
                }
            });
            inner.keep(&path, &shared);
        });
        if let Err(e) = spawned {
            a.shared.update(|s| {
                s.running = false;
                s.error = Some(format!("cannot start the beat analysis: {e}"));
            });
        }
        a
    }

    /// The envelopes' analysis of `a`'s file, on a thread of its own at the lowest priority:
    /// one core, beside the network's pool.
    fn begin_envelopes(&self, file: &Path, a: &Analysis) {
        let (inner, path, cancel, shared) =
            (self.0.clone(), file.to_path_buf(), a.cancel.clone(), a.shared.clone());
        let spawned = std::thread::Builder::new().name("envelopes".into()).spawn(move || {
            penumbra_system::process::lower_thread_priority();
            let mut woke = 0.0;
            let mut progress = |f: f64| {
                shared.update(|s| s.envelope_progress = f);
                if f - woke >= WAKE_STEP {
                    woke = f;
                    shared.wake();
                }
            };
            let mut partial = |e: &Envelopes| {
                let e = Arc::new(e.clone());
                shared.update(|s| s.envelopes = Some(e));
                shared.wake();
            };
            let result = catch_unwind(AssertUnwindSafe(|| {
                beats::analyze_envelopes(
                    &path,
                    inner.cache.as_deref(),
                    Some(&cancel),
                    &mut progress,
                    &mut partial,
                )
            }))
            .unwrap_or_else(|_| Err(BeatError::Input("the envelope analysis failed".into())));
            shared.finish(&cancel, |s| {
                s.envelopes_running = false;
                match result {
                    Ok(e) => {
                        s.envelopes = Some(Arc::new(e));
                        s.envelope_progress = 1.0;
                    }
                    Err(BeatError::Cancelled) => {}
                    Err(e) => s.envelope_error = Some(e.to_string()),
                }
            });
            inner.keep(&path, &shared);
        });
        if let Err(e) = spawned {
            a.shared.update(|s| {
                s.envelopes_running = false;
                s.envelope_error = Some(format!("cannot start the envelope analysis: {e}"));
            });
        }
    }
}

impl Inner {
    /// The network, loaded once for the analyses running at the same time.
    fn model(&self) -> Result<Arc<BeatModel>, BeatError> {
        let mut slot = lock(&self.model);
        if let Some(m) = slot.upgrade() {
            return Ok(m);
        }
        let m = Arc::new(BeatModel::shipped()?);
        *slot = Arc::downgrade(&m);
        Ok(m)
    }

    /// A finished analysis stays for the next node; a command's hold ends with it.
    fn keep(&self, file: &Path, shared: &Arc<Shared>) {
        let Some(a) = lock(&self.files).get(file).and_then(Weak::upgrade) else { return };
        if !Arc::ptr_eq(&a.shared, shared) {
            return;
        }
        let mut held = lock(&self.held);
        held.retain(|h| !Arc::ptr_eq(h, &a));
        if lock(&shared.state).grid.as_ref().is_some_and(|g| g.complete()) {
            held.push(a);
            let done = held.iter().filter(|h| h.ended()).count();
            if done > KEEP_DONE
                && let Some(i) = held.iter().position(|h| h.ended())
            {
                held.remove(i);
            }
        }
    }
}

/// The analysis itself, on its thread.
fn run(inner: &Inner, file: &Path, cancel: &AtomicBool, shared: &Shared) -> Result<Beats, BeatError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(BeatError::Cancelled);
    }
    let model = inner.model()?;
    let mut woke = 0.0;
    let mut progress = |p: &beats::Progress| {
        let f = p.fraction();
        shared.update(|s| s.progress = f);
        if f - woke >= WAKE_STEP {
            woke = f;
            shared.wake();
        }
    };
    let mut partial = |p: &Partial| {
        let grid = Arc::new(Grid::new(p.beats.clone(), p.final_until.unwrap_or(f64::NEG_INFINITY)));
        shared.update(|s| s.grid = Some(grid));
        shared.wake();
    };
    beats::analyze_file_with(file, &model, inner.cache.as_deref(), Some(cancel), &mut progress, &mut partial)
}

#[derive(Default)]
struct GridNode {
    analysis: Option<Arc<Analysis>>,
    watch: Option<FileWatch>,
    /// The pass through a looped file, beat and bar at the last lookup and its time, for the
    /// ticks.
    last: Option<(i64, i64, i64, f64)>,
    /// The time the envelopes were last read at, for the hops since.
    last_read: Option<f64>,
    names: Vec<Name>,
    /// The `grid` output and the grid it was made from.
    table: Option<(Arc<Grid>, Data)>,
    /// The `sections` output and the sections it was made from.
    section_table: Option<(Arc<Structure>, Data)>,
    /// The `overview` output's drawing state, made when it is first read.
    picture: Option<Box<OverviewPicture>>,
    /// The travel for clamps other than the bus's own, and the bus it was made for.
    travel: Option<(Arc<Bus>, Arc<Travel>)>,
}

/// What drawing the overview keeps between cooks.
#[derive(Default)]
struct OverviewPicture {
    picture: ShapePicture,
    columns: ColumnCache,
}

impl GridNode {
    /// The analysis of `file`, anew when the file is another, changed on disk or was analysed
    /// again.
    fn follow(&mut self, cx: &CookContext<'_>, file: &Path) -> Result<Arc<Analysis>, CookError> {
        let changed = self.watch.as_ref().is_some_and(FileWatch::take_changed);
        if let Some(a) = &self.analysis
            && a.file() == file
            && !changed
            && !a.replaced()
        {
            return Ok(a.clone());
        }
        let files = cx
            .resource::<BeatFiles>()
            .ok_or_else(|| CookError::failed("beat analysis is not available in this process"))?;
        if self.watch.as_ref().is_none_or(|w| w.path() != file) {
            self.watch = Some(cx.watch_file(file));
        }
        let a = files.want(file);
        a.wake_on_change(cx.node(), cx.waker());
        self.analysis = Some(a.clone());
        self.last = None;
        self.last_read = None;
        self.table = None;
        self.section_table = None;
        self.travel = None;
        Ok(a)
    }

    /// The `overview` output: drawn while something reads it, else [`Data::skipped`]. A
    /// failure to draw is a warning, so the channels keep flowing.
    fn overview(&mut self, cx: &CookContext<'_>, o: &Overview<'_>) -> Data {
        let p = self.picture.get_or_insert_with(Box::default);
        let drawn = (|| {
            p.picture.take_glyphs(cx)?;
            let (w, h) = (pixels(cx, "width")?, pixels(cx, "height")?);
            let k = meters_picture::layout_scale(w, h);
            let room = Rect::from_min_size([0.0, 0.0], [w as f32 / k, h as f32 / k]);
            let (_, shapes) =
                overview::shapes(room, o, k, &mut p.columns, &Palette::STANDARD, p.picture.measure());
            p.picture.render(cx, w, h, k, shapes, "chan.beat_grid overview")
        })();
        match drawn {
            Ok(tex) => Data::new(DataKind::Tex, tex),
            Err(e) => {
                let why = match e {
                    CookError::Failed(m) => m,
                    CookError::Pending => "not ready".to_string(),
                };
                cx.warn(format!("the overview output is not drawn: {why}"));
                Data::skipped(DataKind::Tex)
            }
        }
    }

    fn table(&mut self, cx: &CookContext<'_>, grid: Option<&Arc<Grid>>) -> Data {
        if !cx.output_wanted(GRID) {
            self.table = None;
            return Data::skipped(DataKind::Table);
        }
        let Some(grid) = grid else {
            return Data::new(DataKind::Table, Table::default());
        };
        if let Some((g, d)) = &self.table
            && Arc::ptr_eq(g, grid)
        {
            return d.clone();
        }
        let data = match grid.table() {
            Ok(t) => Data::new(DataKind::Table, t),
            Err(e) => {
                cx.warn(format!("the grid output is empty: {e}"));
                Data::new(DataKind::Table, Table::default())
            }
        };
        self.table = Some((grid.clone(), data.clone()));
        data
    }

    /// The travel with `floor` and `cap`: `None` for the bus's own clamps, else one made once per
    /// change of the clamps.
    fn travel(&mut self, bus: &Arc<Bus>, grid: Option<&Grid>, floor: f64, cap: f64) -> Option<Arc<Travel>> {
        if bus.travel().clamps() == (floor, cap) {
            self.travel = None;
            return None;
        }
        if let Some((b, t)) = &self.travel
            && Arc::ptr_eq(b, bus)
            && t.clamps() == (floor, cap)
        {
            return Some(t.clone());
        }
        let t = Arc::new(bus.travel_with(&grid?.grid, floor, cap));
        self.travel = Some((bus.clone(), t.clone()));
        Some(t)
    }

    fn section_table(
        &mut self,
        cx: &CookContext<'_>,
        sections: Option<&Arc<Structure>>,
        bus: Option<&Bus>,
    ) -> Data {
        if !cx.output_wanted(SECTIONS) {
            self.section_table = None;
            return Data::skipped(DataKind::Table);
        }
        let Some(sections) = sections else {
            return Data::new(DataKind::Table, Table::default());
        };
        if let Some((s, d)) = &self.section_table
            && Arc::ptr_eq(s, sections)
        {
            return d.clone();
        }
        let data = match sections.table(bus) {
            Ok(t) => Data::new(DataKind::Table, t),
            Err(e) => {
                cx.warn(format!("the sections output is empty: {e}"));
                Data::new(DataKind::Table, Table::default())
            }
        };
        self.section_table = Some((sections.clone(), data.clone()));
        data
    }
}

/// A tempo param in beats per minute: within 0 to [`MAX_TRAVEL_BPM`], 0 for no number.
fn bpm_param(cx: &CookContext<'_>, name: &str) -> Result<f64, CookError> {
    let v = cx.float(name)?;
    Ok(if v.is_finite() { v.clamp(0.0, MAX_TRAVEL_BPM) } else { 0.0 })
}

/// The bus channels, `travelled` to `section_kick`, at `t` seconds into the file (into the pass,
/// for a looped file), with the grid's position there; all 0 before the bus is found. `drift`
/// and `beat_length` need the position and read 0 without it.
pub(crate) fn bus_values(
    bus: Option<&Bus>,
    travel: Option<&Travel>,
    pos: Option<&beats::Position>,
    t: f64,
) -> [f64; BUS_CHANNELS] {
    let Some(bus) = bus else { return [0.0; BUS_CHANNELS] };
    let r = bus.at_with(t, travel.unwrap_or(bus.travel()));
    let (beat_length, drift) = match pos {
        Some(p) => {
            let length = if p.bpm > 0.0 && p.bpm.is_finite() { r.travel_bpm / p.bpm } else { 0.0 };
            // `as`: counts far below 2^53.
            let bars = p.bar as f64 + p.bar_phase;
            (length, bars - r.travelled / bus.bar_beats().max(1) as f64)
        }
        None => (0.0, 0.0),
    };
    let [energy, low, mid, high] = r.energy;
    let s = r.section;
    // `as`: a role's number, 0 to 5.
    let role = s.map_or(0.0, |s| s.role.index() as f64);
    let v = [
        r.travelled,
        r.travel_bpm,
        beat_length,
        drift,
        energy,
        low,
        mid,
        high,
        r.section_energy,
        r.drop,
        role,
        s.map_or(0.0, |s| s.change),
        s.map_or(0.0, |s| s.bass),
        s.map_or(0.0, |s| s.density),
        s.map_or(0.0, |s| s.kick),
    ];
    v.map(|x| if x.is_finite() { x } else { 0.0 })
}

/// The section channels, `section_index` to `sections_ready`, at `t` seconds into the file (into
/// the pass, for a looped file); all 0 before the sections are found.
pub(crate) fn section_values(sections: Option<&Structure>, t: f64) -> [f64; SECTION_CHANNELS] {
    let mut v = [0.0; SECTION_CHANNELS];
    let Some(s) = sections else { return v };
    if let Some(p) = s.sections.at(t) {
        // `as`: counts far below 2^53.
        let n = |x: usize| x as f64;
        v = [
            n(p.section_index),
            n(p.section_label),
            n(p.section_letter),
            n(p.track_index),
            n(p.phrase_bar),
            p.phrase_phase,
            p.bars_to_boundary,
            p.seconds_to_boundary,
            p.build,
            f64::from(p.section_seed),
            p.transition,
            n(p.transition_from),
            n(p.transition_to),
            1.0,
        ];
    }
    if let Some(ready) = v.last_mut() {
        *ready = 1.0;
    }
    v
}

impl Operator for GridNode {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let Some(port) = cx.def().inputs.first().copied().flatten() else {
            self.analysis = None;
            return Err(CookError::failed("wire the out of an audio.file into audio"));
        };
        let sounds = cx
            .resource::<SoundFiles>()
            .ok_or_else(|| CookError::failed("sound files are not available here"))?;
        let Some(status) = sounds.get(port.node) else {
            return Err(if cx.input(0).is_none() {
                CookError::Pending
            } else {
                CookError::failed(
                    "audio must come straight from an audio.file: its file and playhead give the grid",
                )
            });
        };
        if status.file.is_empty() {
            self.analysis = None;
            return Err(CookError::failed("the audio.file has no file"));
        }
        let analysis = self.follow(cx, Path::new(&status.file))?;
        let (grid, progress, running, error) = analysis.grid();
        // Without beats the node still says where the playhead is (`seconds`) and how loud the
        // file is, so a show whose clock follows the music keeps following it.
        if let Some(e) = error {
            cx.warn(format!("no beats: {e}"));
        } else if !running && grid.as_ref().is_none_or(|g| !g.complete()) {
            cx.warn("the beat analysis was cancelled: analyze starts it again");
        }

        let offset = cx.float("offset")?;
        let offset = if offset.is_finite() { offset.clamp(-5000.0, 5000.0) / 1000.0 } else { 0.0 };
        // A looped file's grid runs on from its last beat into the first beat of the next pass,
        // once all of it is known; `t` counts from the start of the first pass then.
        let looped = status.looping && status.duration > 0.0 && grid.as_ref().is_some_and(|g| g.complete());
        let (t, lookup) = if looped {
            let t = status.position + offset;
            (t, grid.as_ref().and_then(|g| g.grid.at_looped(t, status.duration)))
        } else {
            let t = status.in_file + offset;
            (t, grid.as_ref().and_then(|g| g.grid.at(t)).map(|p| (p, 0)))
        };
        let pos = lookup.map(|(p, _)| p);
        let ready = pos.is_some() && grid.as_ref().is_some_and(|g| t < g.final_until);
        let (mut tick, mut downbeat) = (false, false);
        if let (Some((p, pass)), Some((last_pass, beat, bar, before))) = (lookup, self.last) {
            let forward = t > before && t - before <= JUMP_SECONDS;
            tick = forward && (pass, p.beat) > (last_pass, beat);
            downbeat = forward && (pass, p.bar) > (last_pass, bar);
        }
        self.last = lookup.map(|(p, pass)| (pass, p.beat, p.bar, t));

        // The envelopes run on through a loop whether or not the grid is complete: a moment
        // not analysed yet reads 0.
        let env_loop = status.looping && status.duration > 0.0;
        let te = if env_loop { status.position + offset } else { status.in_file + offset };
        let since = self.last_read.filter(|before| te > *before && te - before <= JUMP_SECONDS);
        let reading = analysis
            .envelopes()
            .map(|e| e.read(since, te, env_loop.then_some(status.duration)))
            .unwrap_or_default();
        self.last_read = Some(te);

        let flag = |b: bool| if b { 1.0 } else { 0.0 };
        let mut values = [0.0; CHANNELS.len()];
        let grid_values = match pos {
            Some(p) => [
                p.beat_phase,
                flag(tick),
                p.bar_phase,
                p.beat_in_bar as f64,
                p.bpm,
                p.beats_in_bar as f64,
                flag(ready && p.inside),
                flag(!(ready && p.inside)),
                flag(downbeat),
                p.beat as f64,
                p.bar as f64,
                flag(ready),
                progress,
                flag(running),
            ],
            None => {
                let mut v = [0.0; LEVEL];
                if let Some(x) = v.get_mut(12) {
                    *x = progress;
                }
                if let Some(x) = v.get_mut(13) {
                    *x = flag(running);
                }
                v
            }
        };
        for (v, g) in values.iter_mut().zip(grid_values) {
            *v = g;
        }
        let envelopes =
            [reading.level, reading.low, reading.mid, reading.high, flag(reading.onset), flag(reading.kick)];
        for (v, e) in values.iter_mut().skip(LEVEL).zip(envelopes) {
            *v = e;
        }
        // The sections by the time within the pass: a looped file's end is a boundary.
        let structure = analysis.sections();
        let ts = if env_loop { te.rem_euclid(status.duration) } else { te };
        for (v, x) in values.iter_mut().skip(SECTION).zip(section_values(structure.as_deref(), ts)) {
            *v = x;
        }
        // The time the section channels are read at, for a chan.sequence's `clock`: never below
        // 0, so a negative offset at the file's start holds the show at its start.
        if let Some(v) = values.get_mut(SECONDS) {
            *v = if ts.is_finite() { ts.max(0.0) } else { 0.0 };
        }
        // The bus by the time within the pass, as the sections.
        let bus = analysis.bus();
        let (floor, cap) = (bpm_param(cx, "travel_floor")?, bpm_param(cx, "travel_cap")?);
        let travel = bus.as_ref().and_then(|b| self.travel(b, grid.as_deref(), floor, cap));
        let bus_v = bus_values(bus.as_deref(), travel.as_deref(), pos.as_ref(), ts);
        for (v, x) in values.iter_mut().skip(BUS).zip(bus_v) {
            *v = x;
        }
        if let Some(v) = values.get_mut(TRACK_PHASE) {
            let p = structure.as_deref().and_then(|s| s.sections.at(ts)).map_or(0.0, |p| p.track_phase);
            *v = if p.is_finite() { p } else { 0.0 };
        }
        if self.names.len() != CHANNELS.len() {
            self.names = CHANNELS.iter().filter_map(|n| Name::new(n).ok()).collect();
        }
        let channels = self
            .names
            .iter()
            .zip(values)
            .map(|(name, v)| Channel { name: name.clone(), samples: vec![v] })
            .collect();
        let out = Data::new(DataKind::Chan, Chans::new(cx.rate().as_f64(), channels)?);
        let table = self.table(cx, grid.as_ref());
        let section_table = self.section_table(cx, structure.as_ref(), bus.as_deref());
        let picture = if cx.output_wanted(OVERVIEW) {
            let line = status_line(&analysis.state());
            let envelopes = analysis.envelopes();
            let span = cx.float("span")?;
            let o = overview_of(
                grid.as_deref(),
                envelopes.as_deref(),
                status.duration,
                Some(status.in_file),
                span,
                structure.as_deref(),
                line.as_deref(),
            );
            self.overview(cx, &o)
        } else {
            // Nobody reads it: the textures and the glyph atlas go too.
            self.picture = None;
            Data::skipped(DataKind::Tex)
        };
        Ok(Cooked::Outputs(vec![out, table, picture, section_table]))
    }

    fn job_status(&self) -> Option<JobStatus> {
        self.analysis.as_ref().map(|a| a.job_status())
    }

    fn gpu_bytes(&self) -> u64 {
        self.picture.as_ref().map_or(0, |p| p.picture.gpu_bytes())
    }

    fn sets_size(&self, _def: &NodeDef, _desc: &NodeDesc) -> bool {
        true
    }
}

/// A size param in pixels.
fn pixels(cx: &CookContext<'_>, name: &str) -> Result<u32, CookError> {
    u32::try_from(cx.int(name)?).map_err(|_| CookError::failed(format!("`{name}` must be positive")))
}
