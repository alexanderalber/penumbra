//! Built-in node types: `chan`, `audio` and `table` nodes on the CPU, `tex` nodes on the GPU, the projector
//! DMX and movie outputs and the Python script node.

pub mod audio;
pub mod chan;
pub mod comp;
pub mod data_io;
pub mod dmx;
pub mod freeview;
pub mod geo;
pub mod geo_text;
pub mod http;
pub mod keyboard;
pub mod light;
pub mod mapping;
pub mod mat;
pub mod meters;
pub mod midi;
pub mod mouse;
pub mod note;
pub mod osc;
pub mod out;
pub mod palette;
pub mod readback;
pub mod res;
pub mod rigid;
pub mod script;
pub mod seek;
pub mod sequence;
pub mod shapes;
mod show;
pub mod spacemouse;
pub mod table;
pub mod tex;
pub mod trace;
pub mod view;
pub mod vol;

use std::any::Any;

use penumbra_core::Error;
use penumbra_gpu::Gpu;
use penumbra_graph::{CookContext, CookError, Cooked, NodeTypes, Operator};

/// Registers every built-in node type.
pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    // What a node of a project file that does not load becomes.
    types.register(penumbra_graph::broken::node_type())?;
    audio::register(types)?;
    chan::register(types)?;
    comp::register(types)?;
    data_io::register(types)?;
    dmx::register(types)?;
    geo::register(types)?;
    geo_text::register(types)?;
    http::register(types)?;
    keyboard::register(types)?;
    light::register(types)?;
    mat::register(types)?;
    midi::register(types)?;
    mouse::register(types)?;
    note::register(types)?;
    osc::register(types)?;
    rigid::register(types)?;
    out::register(types)?;
    script::register(types)?;
    sequence::register(types)?;
    spacemouse::register(types)?;
    table::register(types)?;
    tex::register(types)?;
    view::register(types)?;
    vol::register(types)?;
    for (old, new) in RENAMED_KINDS {
        types.renamed_kind(old, new)?;
    }
    for (kind, words) in TAGS {
        types.tag(kind, words)?;
    }
    Ok(())
}

/// Words the add dialog's search also finds a kind by, from their start. Mostly the
/// TouchDesigner names of nodes that do the same under another name here, so someone who knows
/// TouchDesigner finds their way. A name that already contains the TouchDesigner name needs no
/// tag: the search ignores spaces, underscores and dots, so `lens distort` finds
/// `tex.lens_distort`. The file name extensions a node reads are tags of their own and need no
/// row.
pub const TAGS: &[(&str, &[&str])] = &[
    ("tex.blend_weight", &["edge blend", "soft edge", "projection mapping"]),
    ("tex.channel_mix", &["reorder"]),
    (
        "tex.composite",
        &["over", "under", "add", "multiply", "screen", "subtract", "difference", "inside", "outside"],
    ),
    ("tex.dilate", &["grow", "expand", "thicken", "maximum", "morphology"]),
    ("tex.erode", &["shrink", "thin", "minimum", "morphology"]),
    ("tex.face_content", &["projection mapping"]),
    ("tex.feedback", &["substeps", "iterate", "passes", "repeat"]),
    ("tex.fluid", &["simulation", "physics", "smoke", "ink"]),
    ("tex.from_chan", &["chop to"]),
    ("tex.hsv", &["hsv adjust"]),
    ("tex.image_file", &["movie file in"]),
    ("tex.level", &["math"]),
    (
        "tex.mapper",
        &[
            "kantan mapper",
            "stoner",
            "corner pin",
            "keystone",
            "mesh warp",
            "edge blend",
            "soft edge",
            "projection mapping",
            "mapping",
        ],
    ),
    ("tex.primitive", &["rectangle", "circle"]),
    ("tex.render", &["render select"]),
    ("tex.render_volume", &["simulation", "raymarch"]),
    ("tex.scan", &["structured light", "gray code", "calibration", "projection mapping", "projector camera"]),
    ("tex.scan_pattern", &["structured light", "gray code", "test pattern", "projection mapping"]),
    (
        "tex.scatter",
        &[
            "particles gpu",
            "point sprite",
            "splat",
            "deposit",
            "accumulate",
            "instancing additive",
            "histogram",
        ],
    ),
    ("tex.screen_in", &["screen grab", "capture", "desktop", "window capture"]),
    ("tex.shader", &["glsl"]),
    ("tex.shape_content", &["projection mapping"]),
    ("tex.shapes", &["projection mapping", "mask"]),
    ("tex.spout_in", &["syphon spout in"]),
    ("tex.video_file", &["movie file in"]),
    ("tex.video_in", &["video device in", "webcam"]),
    ("chan.audio_pitch", &["audio analysis", "tuner", "fundamental", "f0", "melody", "note detection"]),
    ("chan.beat_analysis", &["audio analysis"]),
    ("chan.clock", &["timer"]),
    ("chan.data_in", &["serial", "udp in", "tcp", "arduino", "com port", "socket", "json", "uart"]),
    ("chan.derivative", &["slope"]),
    ("chan.dropdown", &["drop down menu", "menu", "combo box", "radio buttons", "preset"]),
    ("chan.envelope", &["trigger", "adsr"]),
    ("chan.filter", &["lag"]),
    ("chan.from_table", &["dat to", "lookup", "select dat"]),
    ("chan.feedback", &["delay", "previous frame"]),
    ("chan.from_tex", &["top to chop", "analyze", "pixel", "texel", "sample", "readback"]),
    ("chan.http_in", &["web server", "websocket"]),
    ("chan.integrator", &["speed"]),
    ("chan.math", &["function", "limit", "quantize", "round", "ceil", "floor"]),
    ("chan.spacemouse_in", &["joystick"]),
    ("audio.file", &["audio file in"]),
    ("table.file", &["file in"]),
    ("table.from_chan", &["chop to dat", "chop to", "chan to table", "channels to table"]),
    ("geo.assign_material", &["material"]),
    ("geo.file", &["file in"]),
    ("geo.instance", &["instancing", "copy"]),
    ("geo.primitive", &["box", "sphere", "grid", "rectangle"]),
    ("geo.text", &["text 3d", "3d text", "lettering", "extrude"]),
    ("geo.tex_instance", &["instancing"]),
    (
        "geo.uv_project",
        &["texture sop", "texture coordinates", "uv map", "unwrap", "planar", "cylindrical", "triplanar"],
    ),
    ("light.ambient", &["ambient light"]),
    ("light.environment", &["environment light"]),
    ("light.projector", &["projection mapping", "projector light"]),
    ("physics.bodies", &["actor", "simulation", "collision shape", "physics material"]),
    (
        "physics.rigid_bodies",
        &["bullet solver", "multi body", "multibody", "simulation", "collision", "rapier", "dynamics"],
    ),
    ("vol.emitter", &["nvidia flow emitter", "simulation"]),
    ("vol.obstacle", &["simulation", "collision"]),
    ("vol.smoke", &["nvidia flow", "simulation", "physics", "fire", "fluid 3d"]),
    ("comp.base", &["container", "custom parameters", "customize component"]),
    ("out.audio", &["audio device out"]),
    ("out.audio_file", &["audio file out"]),
    ("out.data", &["data out", "serial", "udp out", "tcp", "arduino", "com port", "socket", "json", "uart"]),
    ("out.dmx", &["dmx out chop"]),
    ("out.midi", &["midi out chop"]),
    ("out.ndi", &["ndi out"]),
    ("out.osc", &["osc out chop"]),
    ("out.projector", &["window", "projection mapping", "display"]),
    ("out.spout", &["syphon spout out"]),
    ("out.table", &["file out"]),
    ("out.video", &["movie file out"]),
    ("script.python", &["execute", "chop execute", "dat execute"]),
    ("ui.note", &["comment", "annotate", "sticky note"]),
];

/// Kinds that were renamed, old name first. A project, component or copied node that still
/// names the old kind loads as the new one, and the next save writes the new name. A row stays
/// as long as files with the old name may be around; a further rename of a kind updates its
/// rows here.
pub const RENAMED_KINDS: &[(&str, &str)] = &[
    // Named for what it does, so the name reads in the list of kinds.
    ("tex.raymarch", "tex.render_volume"),
    // The analysis is named for what it is there for: the beat.
    ("chan.audio_analysis", "chan.beat_analysis"),
    // Compound names got an underscore.
    ("chan.audioanalysis", "chan.beat_analysis"),
    ("chan.audioin", "audio.device_in"),
    ("chan.fromtex", "chan.from_tex"),
    ("chan.oscin", "chan.osc_in"),
    ("chan.oscout", "out.osc"),
    ("geo.texinstance", "geo.tex_instance"),
    ("tex.blobtrack", "tex.blob_track"),
    ("tex.channelmix", "tex.channel_mix"),
    ("tex.chromakey", "tex.chroma_key"),
    ("tex.fromchan", "tex.from_chan"),
    ("tex.lensdistort", "tex.lens_distort"),
    ("tex.opticalflow", "tex.optical_flow"),
    ("tex.timemachine", "tex.time_machine"),
    ("tex.videoin", "tex.video_in"),
    // Names that say what the node does.
    ("geo.material", "geo.assign_material"),
    ("chan.speed", "chan.integrator"),
    // Sending is an output, as for DMX.
    ("chan.osc_out", "out.osc"),
    ("chan.midi_out", "out.midi"),
    ("chan.data_out", "out.data"),
    // Sound got a family of its own.
    ("chan.audio_in", "audio.device_in"),
    // Files, set apart from the live sources.
    ("tex.image", "tex.image_file"),
    ("tex.video", "tex.video_file"),
];

/// The error of a node whose input has nothing (yet): still pending while one is wired,
/// otherwise a hint to wire one.
pub(crate) fn waiting(cx: &CookContext<'_>) -> CookError {
    waiting_for(cx, "connect an input")
}

/// [`waiting`] with `hint` for the unwired case.
pub(crate) fn waiting_for(cx: &CookContext<'_>, hint: &'static str) -> CookError {
    if cx.def().inputs.iter().any(Option::is_some) { CookError::Pending } else { CookError::failed(hint) }
}

/// The GPU, for nodes that need one; an error until it arrives.
pub(crate) fn gpu<'a>(cx: &'a CookContext<'_>) -> Result<&'a Gpu, CookError> {
    cx.resource::<Gpu>().ok_or_else(|| CookError::failed("no GPU yet"))
}

/// A warm-up hook ([`penumbra_graph::WarmUp`]) that runs `f` with the GPU.
pub(crate) fn on_gpu(
    f: impl Fn(&Gpu) -> Result<(), Error> + Send + Sync + 'static,
) -> impl Fn(&dyn Any) -> Result<(), Error> + Send + Sync + 'static {
    move |resource| resource.downcast_ref::<Gpu>().map_or(Ok(()), &f)
}

/// Compiles the shaders and pipelines of every node type in `types` ([`penumbra_graph::NodeType::warm_up`]), so
/// the first node of each kind does not stall the cook thread (a pipeline takes up to about a
/// hundred milliseconds on some backends). Call it on a background thread when the GPU appears.
pub fn warm_up(types: &NodeTypes, gpu: &Gpu) {
    for kind in types.kinds() {
        if let Some(t) = types.get(kind)
            && let Err(e) = t.warm_up(gpu)
        {
            log::error!("the shaders of {kind} do not compile: {e}");
        }
    }
}

/// Passes its input on as it is, without a copy: the nulls of every family but `tex`
/// (`chan.null`, `audio.null`, `table.null`, `geo.null`, `mat.null`, `light.null`, `view.null`, `vol.null`,
/// `physics.null`).
pub(crate) struct Null;

impl Operator for Null {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        match cx.input(0) {
            Some(data) => Ok(Cooked::Outputs(vec![data.clone()])),
            None => Err(waiting(cx)),
        }
    }
}

/// All built-in node types.
pub fn builtin_types() -> Result<NodeTypes, Error> {
    let mut types = NodeTypes::new();
    register(&mut types)?;
    Ok(types)
}

#[cfg(test)]
mod port_docs {
    /// Wiring the wrong one of several outputs is easy (the editor names them, but `tex.render`'s
    /// `data` looks like a picture of its own), so each says what it holds.
    #[test]
    fn every_output_of_a_node_with_several_is_described() {
        let types = super::builtin_types().unwrap();
        for kind in types.kinds() {
            let desc = types.desc(kind).unwrap();
            if desc.outputs.len() > 1 {
                for o in &desc.outputs {
                    assert!(!o.doc.is_empty(), "{kind} output `{}` has no description", o.name);
                }
            }
        }
    }

    /// The param panel's help overlay and the reference show these texts; a param nobody
    /// explained leaves its user guessing. A text of three words or fewer ("Wave form", "In
    /// tiles") says too little to count, and each value of a choice says what it does.
    #[test]
    fn every_param_is_described() {
        let types = super::builtin_types().unwrap();
        let mut missing = Vec::new();
        let mut total = 0;
        for kind in types.kinds() {
            for p in &types.desc(kind).unwrap().params {
                total += 1;
                if p.full_doc().split_whitespace().count() <= 3 {
                    missing.push(format!("{kind}.{} ({:?})", p.name, p.full_doc()));
                }
                if let penumbra_core::ParamKind::Choice(values) = &p.kind {
                    let bare: Vec<&str> =
                        values.iter().filter(|v| p.doc_of_choice(v).is_none()).map(String::as_str).collect();
                    if !bare.is_empty() {
                        missing.push(format!("{kind}.{}: no text for {}", p.name, bare.join(", ")));
                    }
                }
            }
        }
        assert!(
            missing.is_empty(),
            "{} problems in the descriptions of {total} params (four words or more, every choice):
{}",
            missing.len(),
            missing.join(
                "
"
            )
        );
    }
}
