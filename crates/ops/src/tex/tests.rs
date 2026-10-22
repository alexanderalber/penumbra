//! `tex` nodes on the software adapter (WARP on Windows): golden images, determinism, hot
//! reload, the registry-wide time check.
//!
//! Golden images live in `crates/ops/tests/golden/`. `cargo xtask bless` rewrites them
//! (`PENUMBRA_BLESS=1`); a mismatch writes the actual picture to `target/golden-out/` and
//! fails. Tolerance 2 LSB on 8-bit sRGB.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use penumbra_core::{LatePolicy, NodeId, NodePath, ParamPath, Rate, ShowClock, Value};
use penumbra_gpu::{AdapterChoice, Gpu, Pixels, Tex};
use penumbra_graph::{Cooker, Density, JobPool, Network, NodeTypes, Op, Port};

use crate::builtin_types;

const W: i64 = 96;
const H: i64 = 64;

fn gpu() -> Option<Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| match Gpu::headless(AdapterChoice::Software) {
        Ok(g) => Some(g),
        Err(e) if cfg!(windows) => panic!("WARP must exist on Windows: {e}"),
        Err(e) => {
            eprintln!("skipping GPU tests: {e}");
            None
        }
    })
    .clone()
}

/// Every node type's warm-up hook succeeds: the shaders and pipelines it compiles ahead of the
/// first cook are valid.
#[test]
fn every_node_type_warms_up() {
    let Some(gpu) = gpu() else { return };
    let types = builtin_types().unwrap();
    for kind in types.kinds() {
        let t = types.get(kind).unwrap();
        t.warm_up(&gpu).unwrap_or_else(|e| panic!("{kind}: {e}"));
    }
}

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Copies `crates/ops/tests/fixtures/<name>` into `dir`.
fn copy_fixture(name: &str, dir: &Path) {
    std::fs::copy(manifest().join("tests/fixtures").join(name), dir.join(name)).unwrap();
}

/// A scratch directory per test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("penumbra-tex-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn create(path: &str, kind: &str, params: &[(&str, Value)]) -> Op {
    Op::Create {
        path: NodePath::parse(path).unwrap(),
        kind: kind.into(),
        params: params.iter().map(|(k, v)| (k.parse().unwrap(), v.clone())).collect(),
    }
}

fn connect(from: &str, to: &str, input: u32) -> Op {
    Op::Connect { from: from.parse().unwrap(), output: 0, to: to.parse().unwrap(), input }
}

fn set(param: &str, value: Value) -> Op {
    Op::Set { param: ParamPath::parse(param).unwrap(), value }
}

fn size() -> [(&'static str, Value); 2] {
    [("width", Value::Int(W)), ("height", Value::Int(H))]
}

struct Rig {
    types: Arc<NodeTypes>,
    net: Arc<Network>,
    cooker: Cooker,
    clock: ShowClock,
    gpu: Gpu,
    /// Audio is fed here (key `test`), not read from a device.
    audio: penumbra_audio::Hub,
    /// Camera frames are fed here (key `test`), not read from a camera.
    camera: penumbra_camera::Hub,
    /// The recordings of `out.video` nodes.
    record: penumbra_record::Hub,
}

/// A camera frame of one colour, 4:2:0, limited range.
fn camera_frame(y: u8, cb: u8, cr: u8) -> penumbra_camera::CameraFrame {
    penumbra_camera::CameraFrame {
        width: 8,
        height: 4,
        format: penumbra_camera::PixelFormat::Nv12,
        y: vec![y; 32],
        cb: vec![cb; 8],
        cr: vec![cr; 8],
        shift: (1, 1),
        matrix: penumbra_camera::Matrix::Bt709,
        full_range: false,
        sequence: 0,
        arrived: Instant::now(),
        delay_ms: None,
    }
}

impl Rig {
    fn new(gpu: Gpu, dir: Option<&Path>, ops: &[Op]) -> Self {
        let types = Arc::new(builtin_types().unwrap());
        let net = Arc::new(Network::new().apply(&types, ops).unwrap().network);
        let mut cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(2).unwrap()));
        cooker.provide(gpu.clone());
        let hub = penumbra_osc::Hub::new();
        hub.set_wanted(&crate::osc::wants(&net, &types));
        cooker.provide(hub);
        let audio = penumbra_audio::Hub::new();
        audio.feed("test", 48_000, 2, &[0.0; 2]).unwrap();
        cooker.provide(audio.clone());
        let camera = penumbra_camera::Hub::new();
        camera.feed(&penumbra_camera::Key::new("test", ""), camera_frame(126, 128, 128));
        cooker.provide(camera.clone());
        cooker.provide(penumbra_dmx::Hub::new());
        cooker.provide(penumbra_dmx::Inputs::new());
        let record = penumbra_record::Hub::new();
        cooker.provide(record.clone());
        cooker.set_base_dir(dir.map(Path::to_path_buf));
        Self {
            types,
            net,
            cooker,
            clock: ShowClock::new(Rate::FPS_60, LatePolicy::Realtime),
            gpu,
            audio,
            camera,
            record,
        }
    }

    fn id(&self, path: &str) -> NodeId {
        self.net.resolve(&path.parse().unwrap()).unwrap()
    }

    fn edit(&mut self, ops: &[Op]) {
        self.net = Arc::new(self.net.apply(&self.types, ops).unwrap().network);
    }

    fn cook(&mut self, path: &str) {
        let id = self.id(path);
        self.cooker.cook(&self.net, self.clock.now(), &[id], &[]);
    }

    /// Cooks until `path` has a result whose version differs from `after` (asynchronous nodes
    /// take a few passes).
    fn cook_until_new(&mut self, path: &str, after: u64) -> Tex {
        let id = self.id(path);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            self.cooker.cook(&self.net, self.clock.now(), &[id], &[]);
            let s = self.cooker.status(id).unwrap();
            if s.version != after
                && let Some(t) = self.tex(path)
            {
                return t;
            }
            assert!(Instant::now() < deadline, "{path} produced nothing: {s:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn tex(&self, path: &str) -> Option<Tex> {
        self.cooker.output(Port { node: self.id(path), output: 0 })?.get::<Tex>().cloned()
    }

    fn pixels(&mut self, path: &str) -> Pixels {
        let tex = self.cook_until_new(path, 0);
        self.gpu.read(&tex).unwrap()
    }

    fn version(&self, path: &str) -> u64 {
        self.cooker.status(self.id(path)).unwrap().version
    }
}

/// Compares with (or, when blessing, writes) `tests/golden/<name>.png`.
fn golden(name: &str, px: &Pixels) {
    let file = manifest().join("tests/golden").join(format!("{name}.png"));
    if std::env::var_os("PENUMBRA_BLESS").is_some() {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, px.to_png().unwrap()).unwrap();
        return;
    }
    let bytes =
        std::fs::read(&file).unwrap_or_else(|e| panic!("{}: {e}; run `cargo xtask bless`", file.display()));
    let want = photocraft_codecs::decode(&bytes).unwrap();
    let got = px.to_srgb8();
    assert_eq!(want.dimensions(), (px.width, px.height), "{name}: size");
    let want = want.to_rgba8();
    let diffs: Vec<u8> = want.iter().zip(&got).map(|(a, b)| a.abs_diff(*b)).collect();
    let worst = diffs.iter().copied().max().unwrap_or(0);
    if worst > 2 {
        let out = manifest().join("../../target/golden-out");
        std::fs::create_dir_all(&out).unwrap();
        let actual = out.join(format!("{name}.png"));
        std::fs::write(&actual, px.to_png().unwrap()).unwrap();
        let bad = diffs.iter().filter(|d| **d > 2).count();
        panic!("{name}: {bad} samples off by up to {worst}; actual picture in {}", actual.display());
    }
}

/// A 16-bit PNG: red ramp left to right, green ramp top to bottom, alpha 1 except a
/// transparent right quarter.
fn write_png16(path: &Path) {
    let (w, h) = (48u32, 32u32);
    let mut samples = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let a = if x >= w * 3 / 4 { 0 } else { 65535 };
            samples.extend([(x * 65535 / (w - 1)) as u16, (y * 65535 / (h - 1)) as u16, 20000, a]);
        }
    }
    let img =
        photocraft_codecs::Image::from_u16(w, h, photocraft_codecs::ChannelLayout::Rgba, &samples).unwrap();
    let png = photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap();
    std::fs::write(path, png).unwrap();
}

const SHADER: &str = "\
// @param tint color = [1, 0.5, 0.25, 1] -- multiplied in
// @param rings float = 6 1..20
// @density 0.5
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let src = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
    let d = length(v.uv - 0.5);
    let ring = 0.5 + 0.5 * cos(d * params.rings * 6.2831853 - pn_time.seconds * 3.0);
    return vec4f(src.rgb * params.tint.rgb * ring, src.a);
}
";

#[test]
fn golden_images() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("golden");
    write_png16(&dir.join("ramp.png"));
    copy_fixture("bars.mp4", &dir);
    // One frame of the bars as HAP (ffmpeg 8): Hap Alpha with alpha rising left to right, and Hap Q.
    copy_fixture("bars_hap_alpha.mov", &dir);
    copy_fixture("bars_hap_q.mov", &dir);
    std::fs::write(dir.join("rings.wgsl"), SHADER).unwrap();
    let ops = vec![
        create(
            "/constant",
            "tex.constant",
            &[size()[0].clone(), size()[1].clone(), ("color", Value::Vec4([1.0, 0.5, 0.25, 0.5]))],
        ),
        create("/noise", "tex.noise", &size()),
        create(
            "/noise_value",
            "tex.noise",
            &[
                size()[0].clone(),
                size()[1].clone(),
                ("basis", "value".into()),
                ("color", Value::Bool(true)),
                ("seed", Value::Int(3)),
                ("octaves", Value::Int(4)),
                ("frequency", Value::Float(8.0)),
            ],
        ),
        create(
            "/transform",
            "tex.transform",
            &[
                ("rotate", Value::Float(30.0)),
                ("scale", Value::Vec2([0.5, 0.5])),
                ("translate", Value::Vec2([0.1, 0.0])),
                ("extend", "mirror".into()),
            ],
        ),
        create(
            "/transform_zero",
            "tex.transform",
            &[("rotate", Value::Float(-20.0)), ("scale", Value::Vec2([0.7, 0.7])), ("extend", "zero".into())],
        ),
        create(
            "/level",
            "tex.level",
            &[
                ("black", Value::Float(0.2)),
                ("white", Value::Float(0.8)),
                ("gamma", Value::Float(2.0)),
                ("invert", Value::Bool(true)),
            ],
        ),
        create(
            "/quantize",
            "tex.quantize",
            &[("levels", Value::Int(3)), ("block", Value::Int(4)), ("dither", "ordered".into())],
        ),
        create("/over", "tex.composite", &[]),
        create("/swapped", "tex.composite", &[("op", "subtract".into()), ("swap", Value::Bool(true))]),
        create("/image", "tex.image", &[("file", "ramp.png".into())]),
        create("/image_moved", "tex.transform", &[("rotate", Value::Float(90.0)), ("extend", "zero".into())]),
        create("/shader", "tex.shader", &[("file", "rings.wgsl".into()), ("rings", Value::Float(4.0))]),
        create("/video", "tex.video", &[("file", "bars.mp4".into()), ("play", Value::Bool(false))]),
        create(
            "/hap_alpha",
            "tex.video",
            &[("file", "bars_hap_alpha.mov".into()), ("play", Value::Bool(false))],
        ),
        create("/hap_q", "tex.video", &[("file", "bars_hap_q.mov".into()), ("play", Value::Bool(false))]),
        create(
            "/text",
            "tex.text",
            &[
                size()[0].clone(),
                size()[1].clone(),
                (
                    "text",
                    "Hi
lumen"
                        .into(),
                ),
                ("size", Value::Float(24.0)),
                ("color", Value::Vec4([1.0, 0.8, 0.2, 1.0])),
                ("align", "left".into()),
                ("position", Value::Vec2([0.1, 0.5])),
            ],
        ),
        // The filters after the M2 polish, on the colour noise or the ramp image.
        create("/flip", "tex.flip", &[("flip_x", Value::Bool(true)), ("flop", Value::Bool(true))]),
        create(
            "/channel_mix",
            "tex.channel_mix",
            &[("red", Value::Vec4([0.0, 0.0, 1.0, 0.0])), ("blue", Value::Vec4([0.5, 0.5, 0.0, 0.0]))],
        ),
        create(
            "/chroma_key",
            "tex.chroma_key",
            &[("key", Value::Vec4([0.2, 0.8, 0.2, 1.0])), ("tolerance", Value::Float(0.12))],
        ),
        create("/cross", "tex.cross", &[("cross", Value::Float(0.3))]),
        create("/displace", "tex.displace", &[("weight", Value::Vec2([0.2, 0.2]))]),
        create("/edge", "tex.edge", &[]),
        create(
            "/fit",
            "tex.fit",
            &[size()[0].clone(), size()[1].clone(), ("justify", Value::Vec2([0.0, 0.5]))],
        ),
        create("/lens_distort", "tex.lens_distort", &[("k1", Value::Float(0.3))]),
        create("/limit", "tex.limit", &[("max", Value::Float(0.5)), ("mode", "mirror".into())]),
        create("/lookup", "tex.lookup", &[]),
        create("/matte", "tex.matte", &[]),
        create("/monochrome", "tex.monochrome", &[]),
        create("/remap", "tex.remap", &[("extend", "mirror".into())]),
        create("/resolution", "tex.resolution", &[("multiply", Value::Float(0.25))]),
        create("/slope", "tex.slope", &[]),
        create("/threshold", "tex.threshold", &[("soft", Value::Float(0.1))]),
        create("/tile", "tex.tile", &[("repeat", Value::Vec2([3.0, 2.0])), ("mirror", Value::Bool(true))]),
        create("/blur", "tex.blur", &[("size", Value::Float(6.0))]),
        create("/contour", "tex.contour", &[("interval", Value::Float(0.1))]),
        create(
            "/contour_smooth",
            "tex.contour",
            &[("interval", Value::Float(0.1)), ("smooth", Value::Float(4.0)), ("mode", "over".into())],
        ),
        create("/hsv", "tex.hsv", &[("hue", Value::Float(120.0)), ("saturation", Value::Float(0.5))]),
        create("/cut", "tex.threshold", &[("output", "alpha".into())]),
        create("/outside", "tex.composite", &[("op", "outside".into())]),
        create("/atop", "tex.composite", &[("op", "atop".into())]),
        create("/overlay", "tex.composite", &[("op", "overlay".into())]),
        create(
            "/ramp",
            "tex.ramp",
            &[
                size()[0].clone(),
                size()[1].clone(),
                (
                    "keys",
                    "0 1 0.5 0 1
0.6 0.1 0.2 1 0.5
0.6 0 0 0 0"
                        .into(),
                ),
                ("shape", "radial".into()),
            ],
        ),
        connect("/image", "/flip", 0),
        connect("/noise_value", "/channel_mix", 0),
        connect("/noise_value", "/chroma_key", 0),
        connect("/noise", "/cross", 0),
        connect("/noise_value", "/cross", 1),
        connect("/image", "/displace", 0),
        connect("/noise_value", "/displace", 1),
        connect("/noise_value", "/edge", 0),
        connect("/image", "/fit", 0),
        connect("/image", "/lens_distort", 0),
        connect("/noise_value", "/limit", 0),
        connect("/noise", "/lookup", 0),
        connect("/image", "/lookup", 1),
        connect("/image", "/matte", 0),
        connect("/constant", "/matte", 1),
        connect("/threshold", "/matte", 2),
        connect("/noise_value", "/monochrome", 0),
        connect("/image", "/remap", 0),
        connect("/noise_value", "/remap", 1),
        connect("/noise_value", "/resolution", 0),
        connect("/noise", "/slope", 0),
        connect("/noise", "/threshold", 0),
        connect("/image", "/tile", 0),
        connect("/noise_value", "/blur", 0),
        connect("/noise", "/contour", 0),
        connect("/noise", "/contour_smooth", 0),
        connect("/noise_value", "/hsv", 0),
        connect("/noise", "/cut", 0),
        connect("/cut", "/outside", 0),
        connect("/image", "/outside", 1),
        connect("/cut", "/atop", 0),
        connect("/image", "/atop", 1),
        connect("/noise_value", "/overlay", 0),
        connect("/image", "/overlay", 1),
        connect("/noise_value", "/transform", 0),
        connect("/image", "/transform_zero", 0),
        connect("/noise", "/level", 0),
        connect("/noise_value", "/quantize", 0),
        connect("/noise", "/over", 0),
        connect("/constant", "/over", 1),
        connect("/noise_value", "/swapped", 0),
        connect("/noise", "/swapped", 1),
        connect("/image", "/image_moved", 0),
        connect("/noise_value", "/shader", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    rig.clock.advance(60);
    let names = [
        "constant",
        "noise",
        "noise_value",
        "transform",
        "transform_zero",
        "level",
        "quantize",
        "over",
        "swapped",
        "image",
        "image_moved",
        "shader",
        "text",
        "video",
        "hap_alpha",
        "hap_q",
        "flip",
        "channel_mix",
        "chroma_key",
        "cross",
        "displace",
        "edge",
        "fit",
        "lens_distort",
        "limit",
        "lookup",
        "matte",
        "monochrome",
        "remap",
        "resolution",
        "slope",
        "threshold",
        "tile",
        "blur",
        "contour",
        "contour_smooth",
        "hsv",
        "outside",
        "atop",
        "overlay",
        "ramp",
    ];
    for name in names {
        let px = rig.pixels(&format!("/{name}"));
        golden(name, &px);
    }
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn every_builtin_cooks_without_warnings() {
    // A registry-wide check: a node reading the clock must declare it (here: a node whose instance
    // is time-dependent must say so). Every node type is created and cooked with its inputs fed, at
    // a moving time.
    let Some(gpu) = gpu() else { return };
    let dir = scratch("every");
    write_png16(&dir.join("ramp.png"));
    copy_fixture("bars.mp4", &dir);
    std::fs::write(
        dir.join("rows.csv"),
        "a,b
1,2
",
    )
    .unwrap();
    std::fs::write(dir.join("rings.wgsl"), SHADER).unwrap();
    let types = builtin_types().unwrap();
    let mut ops = vec![
        create("/feed", "tex.constant", &size()),
        create("/chan_feed", "chan.constant", &[]),
        create("/table_feed", "table.text", &[]),
        create("/geo_feed", "geo.primitive", &[]),
        create("/mat_feed", "mat.constant", &[]),
        create("/light_feed", "light.ambient", &[]),
        create("/view_feed", "view.camera", &[]),
        create("/audio_feed", "chan.audio_in", &[("device", "test".into())]),
    ];
    let cube = penumbra_mesh::prim::cuboid([1.0; 3]).unwrap();
    std::fs::write(dir.join("box.obj"), penumbra_mesh::write::to_obj(&cube, None)).unwrap();
    std::fs::write(dir.join("poster.psd"), layers::psd()).unwrap();
    // The placeholder of a node that did not load fails its cook on purpose.
    let kinds: Vec<String> =
        types.kinds().filter(|k| *k != penumbra_graph::broken::BROKEN_KIND).map(str::to_string).collect();
    let passes_on = |kind: &str| {
        matches!(
            kind,
            "chan.null" | "chan.select" | "chan.rename" | "table.null" | "chan.osc_out" | "out.dmx"
        )
    };
    // out.dmx sends here rather than to an Open DMX USB that may be plugged in.
    let dmx_rx = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let dmx_host = dmx_rx.local_addr().unwrap().to_string();
    // chan.dmx_in listens on this machine only, on a port that was free, not on Art-Net's.
    let dmx_in_port = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    // tex.spout_in receives what out.spout sends (it cooks first, `out` sorts before `tex`).
    let spout = format!("penumbra test registry {}", std::process::id());
    for (i, kind) in kinds.iter().enumerate() {
        let path = format!("/n{i}");
        let mut params: Vec<(&str, Value)> = Vec::new();
        let desc = types.desc(kind).unwrap();
        if desc.param_spec("width").is_some() {
            params.extend(size());
        }
        match kind.as_str() {
            "tex.image" => params.push(("file", "ramp.png".into())),
            "tex.shader" => params.push(("file", "rings.wgsl".into())),
            "tex.video" => params.push(("file", "bars.mp4".into())),
            "tex.noise" => params.push(("speed", Value::Float(0.5))),
            "tex.select" => params.push(("node", "/feed".into())),
            "out.projector" => params.push(("display", "test".into())),
            "table.file" => params.push(("file", "rows.csv".into())),
            "geo.file" => params.push(("file", "box.obj".into())),
            "tex.layer" => params.push(("file", "poster.psd".into())),
            // Any free port, so the test never collides with something listening on 8000.
            "chan.osc_in" => params.push(("port", Value::Int(0))),
            "light.environment" => params.push(("size", Value::Int(64))),
            "chan.audio_in" => params.push(("device", "test".into())),
            "out.dmx" => params.extend([("protocol", "art_net".into()), ("host", dmx_host.as_str().into())]),
            "chan.dmx_in" => params.extend([
                ("interface", "127.0.0.1".into()),
                ("port", Value::Int(i64::from(dmx_in_port))),
                ("count", Value::Int(4)),
            ]),
            "tex.video_in" => params.push(("device", "test".into())),
            "out.spout" => params.push(("name", spout.as_str().into())),
            "tex.spout_in" => params.push(("sender", spout.as_str().into())),
            _ => {}
        }
        ops.push(create(&path, kind, &params));
        for (input, spec) in desc.inputs.iter().enumerate() {
            let feed = match spec.kind {
                penumbra_graph::DataKind::Tex => Some("/feed"),
                penumbra_graph::DataKind::Geo => Some("/geo_feed"),
                penumbra_graph::DataKind::Mat => Some("/mat_feed"),
                penumbra_graph::DataKind::Light => Some("/light_feed"),
                penumbra_graph::DataKind::View => Some("/view_feed"),
                // Component ports and nulls pass on what they get and complain about nothing wired.
                penumbra_graph::DataKind::Chan if kind == "chan.audio_analysis" => Some("/audio_feed"),
                penumbra_graph::DataKind::Chan
                    if desc.port.is_some() || passes_on(kind) || kind == "tex.from_chan" =>
                {
                    Some("/chan_feed")
                }
                penumbra_graph::DataKind::Table
                    if desc.port.is_some() || passes_on(kind) || kind == "geo.instance" =>
                {
                    Some("/table_feed")
                }
                _ => None,
            };
            if let Some(feed) = feed {
                ops.push(connect(feed, &path, input as u32));
            }
        }
    }
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    for (i, kind) in kinds.iter().enumerate() {
        let path = format!("/n{i}");
        if kind == "script.python" && penumbra_script::host().install().is_err() {
            // Needs a Python; without one its error is the expected outcome.
            continue;
        }
        // On the network, which makes Windows ask about the firewall for every new test binary:
        // `ndi_round_trip` (ignored) covers both.
        if kind == "tex.ndi_in" || kind == "out.ndi" {
            continue;
        }
        rig.clock.advance(1);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            rig.clock.advance(1);
            // A frame of audio for the audio nodes.
            rig.audio.feed("test", 48_000, 2, &[0.1; 1600]).unwrap();
            // As the engine does every frame: a readback's waker (tex.blob_track's) only fires
            // when the device is polled.
            rig.gpu.poll();
            // A frame of Art-Net for chan.dmx_in, which warns until something arrived.
            let dmx_in = kind == "chan.dmx_in";
            if dmx_in {
                let packet = penumbra_dmx::artnet::dmx_packet(1, 0, &[255; 4]);
                dmx_rx.send_to(&packet, ("127.0.0.1", dmx_in_port)).unwrap();
            }
            rig.cook(&path);
            let s = rig.cooker.status(rig.id(&path)).unwrap();
            if s.has_output && !(dmx_in && s.warning.is_some()) {
                // One more pass at a new time, where an undeclared time read would show.
                rig.clock.advance(1);
                rig.cook(&path);
                let s = rig.cooker.status(rig.id(&path)).unwrap();
                assert_eq!((s.error, s.warning), (None, None), "{kind}");
                break;
            }
            assert!(s.error.is_none(), "{kind}: {s:?}");
            assert!(Instant::now() < deadline, "{kind} produced nothing");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn noise_is_deterministic_and_only_animated_when_moving() {
    let Some(gpu) = gpu() else { return };
    let ops = [create("/n", "tex.noise", &size())];
    let a = Rig::new(gpu.clone(), None, &ops).pixels("/n");
    let b = Rig::new(gpu.clone(), None, &ops).pixels("/n");
    assert_eq!(a, b, "the same noise twice must be bit-identical");

    let mut rig = Rig::new(gpu, None, &ops);
    rig.cook("/n");
    let v = rig.version("/n");
    rig.clock.advance(1);
    rig.cook("/n");
    assert_eq!(rig.version("/n"), v, "a still noise does not recook when time moves");
    assert!(!rig.cooker.status(rig.id("/n")).unwrap().time_dependent);
    rig.edit(&[set("/n.speed", Value::Float(1.0))]);
    rig.cook("/n");
    let v = rig.version("/n");
    rig.clock.advance(1);
    rig.cook("/n");
    assert_ne!(rig.version("/n"), v, "a moving noise recooks with time");
}

#[test]
fn shader_hot_reload_keeps_the_last_good_version() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("reload");
    let file = dir.join("s.wgsl");
    let red = "@fragment fn main(v: PnVertex) -> @location(0) vec4f { return vec4f(1.0, 0.0, 0.0, 1.0); }";
    std::fs::write(&file, red).unwrap();
    let ops = [create(
        "/s",
        "tex.shader",
        &[
            ("file", "s.wgsl".into()),
            ("resolution", "custom".into()),
            ("width", Value::Int(8)),
            ("height", Value::Int(8)),
        ],
    )];
    let mut rig = Rig::new(gpu.clone(), Some(&dir), &ops);
    let px = rig.pixels("/s");
    assert_eq!(px.get(3, 3), Some([1.0, 0.0, 0.0, 1.0]));
    let id = rig.id("/s");
    assert!(!rig.cooker.status(id).unwrap().time_dependent);

    // A broken version: the node warns with the compiler's message and keeps working.
    std::fs::write(
        &file,
        "// @param gain float = 2\n@fragment fn main(v: PnVertex) -> @location(0) vec4f { return oops; }",
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        rig.cook("/s");
        let s = rig.cooker.status(id).unwrap();
        if let Some(w) = s.warning {
            assert!(w.contains("s.wgsl:2") && w.contains("last version that compiled"), "{w}");
            assert!(s.error.is_none());
            break;
        }
        assert!(Instant::now() < deadline, "the broken shader was not noticed");
        std::thread::sleep(Duration::from_millis(20));
    }

    // A param named like one of the node's own is refused, not shadowed by the node's value.
    std::fs::write(
        &file,
        "// @param width float = 2\n@fragment fn main(v: PnVertex) -> @location(0) vec4f { return vec4f(params.width); }",
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        rig.cook("/s");
        let s = rig.cooker.status(id).unwrap();
        if s.warning.as_deref().is_some_and(|w| w.contains("`width` is a param of the node itself")) {
            break;
        }
        assert!(Instant::now() < deadline, "the clashing param was not noticed: {s:?}");
        std::thread::sleep(Duration::from_millis(20));
    }

    // Fixed, with a declared param and the clock: picked up, params reported, time-dependent.
    let v = rig.version("/s");
    std::fs::write(
        &file,
        "// @param gain float = 0.5 0..1\n@fragment fn main(v: PnVertex) -> @location(0) vec4f { return vec4f(0.0, params.gain + pn_time.seconds * 0.0, 0.0, 1.0); }",
    )
    .unwrap();
    let tex = rig.cook_until_new_after_reload("/s", v);
    assert_eq!(rig.gpu.read(&tex).unwrap().get(0, 0), Some([0.0, 0.5, 0.0, 1.0]));
    let s = rig.cooker.status(id).unwrap();
    assert_eq!(s.warning, None);
    assert!(s.time_dependent);
    assert_eq!(s.params.unwrap()[0].name, "gain");

    // Values are checked against the header: unknown names and wrong types warn.
    rig.edit(&[set("/s.gain", Value::Float(0.25)), set("/s.bogus", Value::Int(1))]);
    rig.cook("/s");
    let s = rig.cooker.status(id).unwrap();
    assert_eq!(s.warning.as_deref(), Some("the shader declares no param `bogus`"));
    let px = rig.gpu.read(&rig.tex("/s").unwrap()).unwrap();
    assert_eq!(px.get(0, 0), Some([0.0, 0.25, 0.0, 1.0]));
    std::fs::remove_dir_all(&dir).unwrap();
}

impl Rig {
    /// Like [`Rig::cook_until_new`], but keeps the clock moving so a reload (which arrives
    /// through the watcher and a job) gets its passes.
    fn cook_until_new_after_reload(&mut self, path: &str, after: u64) -> Tex {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            self.cook(path);
            let s = self.cooker.status(self.id(path)).unwrap();
            if s.version != after && s.warning.is_none() && s.params.is_some() {
                return self.tex(path).unwrap();
            }
            assert!(Instant::now() < deadline, "{path} did not reload: {s:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
fn image_reloads_and_reports_bad_files() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("image");
    write_png16(&dir.join("a.png"));
    let mut rig = Rig::new(gpu, Some(&dir), &[create("/i", "tex.image", &[("file", "a.png".into())])]);
    assert_eq!(rig.pixels("/i").width, 48);
    let id = rig.id("/i");
    std::fs::write(dir.join("a.png"), b"not a png any more").unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        rig.cook("/i");
        let s = rig.cooker.status(id).unwrap();
        if s.error.is_some() {
            // The last good picture stays.
            assert!(s.has_output);
            break;
        }
        assert!(Instant::now() < deadline, "the broken file was not noticed");
        std::thread::sleep(Duration::from_millis(20));
    }
    rig.edit(&[set("/i.file", "missing.png".into())]);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        rig.cook("/i");
        let e = rig.cooker.status(id).unwrap().error.unwrap_or_default();
        if e.contains("missing.png") {
            break;
        }
        assert!(Instant::now() < deadline, "missing file not reported: {e}");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn without_a_gpu_tex_nodes_fail_cleanly() {
    let types = Arc::new(builtin_types().unwrap());
    let net = Arc::new(Network::new().apply(&types, &[create("/c", "tex.constant", &[])]).unwrap().network);
    let mut cooker = Cooker::new(types, Arc::new(JobPool::new(1).unwrap()));
    let id = net.resolve(&"/c".parse().unwrap()).unwrap();
    cooker.cook(&net, ShowClock::new(Rate::FPS_60, LatePolicy::Realtime).now(), &[id], &[]);
    assert!(cooker.status(id).unwrap().error.unwrap().contains("no GPU"));
}

#[test]
fn files_load_once_the_gpu_arrives() {
    // A file node cooked before the GPU is there (the editor asks for thumbnails at once) must
    // still load once it is.
    let Some(gpu) = gpu() else { return };
    let dir = scratch("late_gpu");
    write_png16(&dir.join("ramp.png"));
    let cube = penumbra_mesh::prim::cuboid([1.0; 3]).unwrap();
    std::fs::write(dir.join("box.obj"), penumbra_mesh::write::to_obj(&cube, None)).unwrap();
    let types = Arc::new(builtin_types().unwrap());
    let ops = [
        create("/img", "tex.image", &[("file", "ramp.png".into())]),
        create("/obj", "geo.file", &[("file", "box.obj".into())]),
    ];
    let net = Arc::new(Network::new().apply(&types, &ops).unwrap().network);
    let mut cooker = Cooker::new(types, Arc::new(JobPool::new(2).unwrap()));
    cooker.set_base_dir(Some(dir.clone()));
    let ids: Vec<NodeId> =
        ["/img", "/obj"].iter().map(|p| net.resolve(&p.parse().unwrap()).unwrap()).collect();
    let mut clock = ShowClock::new(Rate::FPS_60, LatePolicy::Realtime);
    cooker.cook(&net, clock.now(), &ids, &[]);
    for id in &ids {
        assert!(cooker.status(*id).unwrap().error.unwrap().contains("no GPU"));
    }
    cooker.provide(gpu);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        clock.advance(1);
        cooker.cook(&net, clock.now(), &ids, &[]);
        if ids.iter().all(|id| cooker.status(*id).unwrap().has_output) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{:?}",
            ids.iter().map(|id| cooker.status(*id)).collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn transform_density_follows_scale() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        create("/c", "tex.constant", &size()),
        create("/t", "tex.transform", &[("scale", Value::Vec2([2.0, 1.0]))]),
        connect("/c", "/t", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    rig.cook("/t");
    // Scaled up by 2 across: the input needs twice the pixels across.
    let d = rig.cooker.density(rig.id("/t"), 0).unwrap().demand([100.0, 50.0]).unwrap();
    assert!((d[0] - 200.0).abs() < 1e-9 && (d[1] - 50.0).abs() < 1e-9, "{d:?}");
    assert_eq!(rig.cooker.density(rig.id("/c"), 0), Some(Density::SAME));
    assert!(rig.cooker.sets_size(rig.id("/c")));
    assert!(!rig.cooker.sets_size(rig.id("/t")));
}

#[test]
fn text_follows_a_new_size_before_its_raster_is_done() {
    let Some(gpu) = gpu() else { return };
    let ops = [create(
        "/t",
        "tex.text",
        &[size()[0].clone(), size()[1].clone(), ("text", "ii".into()), ("size", Value::Float(16.0))],
    )];
    let mut rig = Rig::new(gpu, None, &ops);
    // Columns with ink, from the left edge of the first to the right edge of the last.
    let ink = |px: &Pixels| {
        let cols: Vec<u32> = (0..px.width)
            .filter(|x| (0..px.height).any(|y| px.data[(y * px.width + x) as usize][3] > 0.5))
            .collect();
        cols.last().unwrap() - cols.first().unwrap() + 1
    };
    let small = ink(&rig.pixels("/t"));
    rig.edit(&[set("/t.size", Value::Float(32.0))]);
    // The cook that starts the new raster draws the old one twice as large.
    rig.cook("/t");
    let tex = rig.tex("/t").unwrap();
    let scaled = ink(&rig.gpu.read(&tex).unwrap());
    assert!((f64::from(scaled) / f64::from(small) - 2.0).abs() < 0.3, "{small} -> {scaled}");
}

#[test]
fn a_camera_frame_becomes_a_texture_and_every_new_one_a_new_result() {
    let Some(gpu) = gpu() else { return };
    let mut rig = Rig::new(gpu, None, &[create("/cam", "tex.video_in", &[("device", "test".into())])]);
    let key = penumbra_camera::Key::new("test", "");
    rig.camera.feed(&key, camera_frame(235, 128, 128));
    let px = rig.pixels("/cam");
    assert_eq!((px.width, px.height), (8, 4));
    let white = px.get(3, 2).unwrap();
    assert!(white.iter().all(|v| (v - 1.0).abs() < 0.01), "{white:?}");
    // Nothing new: no new result.
    let v = rig.version("/cam");
    rig.cook("/cam");
    assert_eq!(rig.version("/cam"), v);
    // Black in limited range, then the same frame read as full range is a dark grey.
    rig.camera.feed(&key, camera_frame(16, 128, 128));
    let tex = rig.cook_until_new("/cam", v);
    assert!(rig.gpu.read(&tex).unwrap().get(0, 0).unwrap()[0].abs() < 0.01);
    // A repeat of the picture (a capture card doubling a 30p source) is no new result either.
    let v = rig.version("/cam");
    rig.camera.feed(&key, camera_frame(16, 128, 128));
    for _ in 0..5 {
        rig.cook("/cam");
    }
    assert_eq!(rig.version("/cam"), v);
    rig.edit(&[set("/cam.range", "full".into())]);
    let tex = rig.cook_until_new("/cam", v);
    let grey = rig.gpu.read(&tex).unwrap().get(0, 0).unwrap()[0];
    assert!(grey > 0.003 && grey < 0.01, "{grey}");
}

/// Red of the top left pixel of a node's output, after cooking it.
fn red(rig: &mut Rig, path: &str) -> f32 {
    rig.cook(path);
    let tex = rig.tex(path).unwrap();
    rig.gpu.read(&tex).unwrap().get(0, 0).unwrap()[0]
}

fn constant(path: &str, r: f64) -> Op {
    create(
        path,
        "tex.constant",
        &[("width", Value::Int(8)), ("height", Value::Int(4)), ("color", Value::Vec4([r, 0.0, 0.0, 1.0]))],
    )
}

#[test]
fn feedback_loops_through_its_target() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        constant("/c", 0.1),
        create("/fb", "tex.feedback", &[]),
        create("/acc", "tex.composite", &[("op", "add".into())]),
        connect("/c", "/fb", 0),
        connect("/acc", "/fb", 1),
        connect("/fb", "/acc", 0),
        connect("/c", "/acc", 1),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let mut seen = Vec::new();
    for _ in 0..4 {
        rig.clock.advance(1);
        seen.push(red(&mut rig, "/acc"));
    }
    let want = [0.2, 0.3, 0.4, 0.5];
    for (got, want) in seen.iter().zip(want) {
        assert!((got - want).abs() < 0.01, "{seen:?}");
    }
    // Reset passes the input again; a frame that does not move time does not loop.
    rig.edit(&[set("/fb.reset", Value::Bool(true))]);
    rig.clock.advance(1);
    assert!((red(&mut rig, "/acc") - 0.2).abs() < 0.01);
    let still = red(&mut rig, "/acc");
    assert!((still - 0.2).abs() < 0.01, "{still}");
    // A loop through a normal input is still refused.
    let back = connect("/acc", "/c2", 0);
    let e = rig.net.apply(&rig.types, &[create("/c2", "tex.level", &[]), connect("/c2", "/fb", 0), back]);
    assert!(e.unwrap_err().0.contains("loop"));
}

#[test]
fn feedback_keeps_a_restored_frame_until_the_loop_moves_on() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        constant("/c", 0.1),
        create("/fb", "tex.feedback", &[]),
        create("/acc", "tex.composite", &[("op", "add".into())]),
        connect("/c", "/fb", 0),
        connect("/acc", "/fb", 1),
        connect("/fb", "/acc", 0),
        connect("/c", "/acc", 1),
    ];
    let mut rig = Rig::new(gpu.clone(), None, &ops);
    for _ in 0..4 {
        rig.clock.advance(1);
        red(&mut rig, "/acc");
    }
    let saved = red(&mut rig, "/fb");
    assert!(saved > 0.35, "{saved}");
    let kept = rig.cooker.kept();
    let [(_, penumbra_graph::Kept::State(items))] = kept.as_slice() else { panic!("{kept:?}") };

    // A restart in which only the feedback cooks for a while (its loop's end is not looked at):
    // it keeps passing the saved frame instead of falling back to `initial`.
    let mut again = Rig::new(gpu, None, &ops);
    again.cooker.cook(&again.net, again.clock.now(), &[], &[]);
    again.cooker.restore(again.id("/fb"), penumbra_graph::Kept::State(items.clone())).unwrap();
    for _ in 0..3 {
        again.clock.advance(1);
        assert!((red(&mut again, "/fb") - saved).abs() < 0.01);
    }
    // Once the end of the loop has made something from it, the loop carries on from there.
    let next = red(&mut again, "/acc");
    assert!((next - (saved + 0.1)).abs() < 0.01, "{next} after {saved}");
    again.clock.advance(1);
    assert!((red(&mut again, "/acc") - (saved + 0.2)).abs() < 0.01);
    // A restore into a running loop shows the saved frame once the clock moves, then goes on.
    again.cooker.restore(again.id("/fb"), penumbra_graph::Kept::State(items.clone())).unwrap();
    again.clock.advance(1);
    assert!((red(&mut again, "/acc") - (saved + 0.1)).abs() < 0.01);
    again.clock.advance(1);
    assert!((red(&mut again, "/acc") - (saved + 0.2)).abs() < 0.01);
}

#[test]
fn color_and_luminosity_trade_hue_and_brightness() {
    let Some(gpu) = gpu() else { return };
    let rgba = |path: &str, c: [f64; 4]| {
        create(
            path,
            "tex.constant",
            &[("width", Value::Int(8)), ("height", Value::Int(4)), ("color", Value::Vec4(c))],
        )
    };
    let ops = [
        rgba("/grey", [0.4, 0.4, 0.4, 1.0]),
        rgba("/orange", [0.8, 0.4, 0.1, 1.0]),
        create("/color", "tex.composite", &[("op", "color".into())]),
        connect("/grey", "/color", 0),
        connect("/orange", "/color", 1),
        create("/luminosity", "tex.composite", &[("op", "luminosity".into())]),
        connect("/grey", "/luminosity", 0),
        connect("/orange", "/luminosity", 1),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let lum = |c: [f32; 4]| 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
    let mut pixel = |path: &str| {
        rig.cook(path);
        let tex = rig.tex(path).unwrap();
        rig.gpu.read(&tex).unwrap().get(0, 0).unwrap()
    };
    let c = pixel("/color");
    assert!((lum(c) - 0.4).abs() < 0.01, "the grey's brightness: {c:?}");
    assert!(c[0] > c[1] && c[1] > c[2], "the orange's hue: {c:?}");
    assert!(((c[0] - c[2]) - 0.7).abs() < 0.01, "and its saturation: {c:?}");
    let l = pixel("/luminosity");
    let orange = 0.3 * 0.8 + 0.59 * 0.4 + 0.11 * 0.1;
    assert!(l.iter().take(3).all(|v| (v - orange).abs() < 0.01), "grey at the orange's brightness: {l:?}");
}

#[test]
fn composite_lays_further_layers_on_in_turn() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        constant("/a", 0.1),
        constant("/b", 0.2),
        constant("/c", 0.3),
        constant("/d", 0.4),
        create("/sum", "tex.composite", &[("op", "add".into())]),
        connect("/a", "/sum", 0),
        connect("/b", "/sum", 1),
        connect("/c", "/sum", 2),
        connect("/d", "/sum", 3),
        create("/gap", "tex.composite", &[("op", "add".into())]),
        connect("/a", "/gap", 0),
        connect("/b", "/gap", 1),
        // layer3 left out.
        connect("/d", "/gap", 3),
        create("/over", "tex.composite", &[]),
        connect("/a", "/over", 0),
        connect("/c", "/over", 2),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    assert!((red(&mut rig, "/sum") - 1.0).abs() < 0.01, "0.1 + 0.2 + 0.3 + 0.4");
    assert!((red(&mut rig, "/gap") - 0.7).abs() < 0.01, "0.1 + 0.2 + 0.4");
    // Without layer2, layer3 goes onto layer1; opaque, it covers it.
    assert!((red(&mut rig, "/over") - 0.3).abs() < 0.01);
}

#[test]
fn contour_draws_a_line_per_interval_and_none_around_clipped_flats() {
    let Some(gpu) = gpu() else { return };
    let ramp = |path: &str, keys: &str| {
        create(
            path,
            "tex.ramp",
            &[("width", Value::Int(200)), ("height", Value::Int(4)), ("keys", keys.into())],
        )
    };
    let ops = [
        ramp(
            "/rise",
            "0 0 0 0
1 1 1 1",
        ),
        // Flat at 0 on the left half, then rising: a valley floor clipped at 0.
        ramp(
            "/floor",
            "0 0 0 0
0.5 0 0 0
1 1 1 1",
        ),
        create("/lines", "tex.contour", &[("interval", Value::Float(0.1))]),
        create("/clipped", "tex.contour", &[("interval", Value::Float(0.1))]),
        connect("/rise", "/lines", 0),
        connect("/floor", "/clipped", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let row = |rig: &mut Rig, path: &str| {
        let px = rig.pixels(path);
        (0..200).map(|x| px.get(x, 2).unwrap()[0]).collect::<Vec<f32>>()
    };
    // Lines are runs of lit pixels; on the rise one per level 0.1 to 0.9, not at 0 and 1.
    let runs = |v: &[f32]| {
        let mut starts = Vec::new();
        for x in 0..v.len() {
            if v[x] > 0.5 && (x == 0 || v[x - 1] <= 0.5) {
                starts.push(x);
            }
        }
        starts
    };
    let lines = row(&mut rig, "/lines");
    let starts = runs(&lines);
    assert_eq!(starts.len(), 9, "{starts:?}");
    for (i, x) in starts.iter().enumerate() {
        let want = 20.0 * (i + 1) as f32;
        assert!((*x as f32 - want).abs() <= 2.0, "line {i} at {x}, not near {want}");
    }
    assert!(lines[10] < 0.01 && lines[30] < 0.01, "between the lines");
    // The floor: nothing on the flat half nor at its border, the levels of the rise as before.
    let clipped = row(&mut rig, "/clipped");
    assert!(clipped[..100].iter().all(|v| *v < 0.01), "{:?}", &clipped[..110]);
    assert_eq!(runs(&clipped).len(), 9, "{:?}", runs(&clipped));
}

/// A grey rise from 0 at the left to 1 at the right (linear) plus fine noise of +-0.03: the
/// value crosses every level back and forth a few times.
fn write_noisy_rise(path: &Path, w: u32, h: u32) {
    let encode = |v: f32| if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    let mut samples = Vec::new();
    let mut seed = 7u32;
    for _ in 0..h {
        for x in 0..w {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = ((seed >> 8) as f32 / 16_777_216.0 - 0.5) * 0.06;
            let v = encode((x as f32 / (w - 1) as f32 + noise).clamp(0.0, 1.0));
            let g = (v * 65535.0).round() as u16;
            samples.extend([g, g, g, 65535]);
        }
    }
    let img =
        photocraft_codecs::Image::from_u16(w, h, photocraft_codecs::ChannelLayout::Rgba, &samples).unwrap();
    let png = photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap();
    std::fs::write(path, png).unwrap();
}

/// Starts of runs of lit pixels (red above 0.5) in row `y`.
fn lit_runs(px: &Pixels, y: u32) -> Vec<u32> {
    let lit = |x: u32| px.get(x, y).is_some_and(|p| p[0] > 0.5);
    (0..px.width).filter(|&x| lit(x) && (x == 0 || !lit(x - 1))).collect()
}

#[test]
fn contour_smooth_calms_noise_and_ignores_transparent_parts() {
    let Some(gpu) = gpu() else { return };
    let (w, h) = (200, 24);
    let dir = scratch("contour_smooth");
    write_noisy_rise(&dir.join("noisy.png"), w as u32, h as u32);
    let ops = [
        create("/noisy", "tex.image", &[("file", "noisy.png".into())]),
        create("/raw", "tex.contour", &[("interval", Value::Float(0.1))]),
        create("/calm", "tex.contour", &[("interval", Value::Float(0.1)), ("smooth", Value::Float(8.0))]),
        connect("/noisy", "/raw", 0),
        connect("/noisy", "/calm", 0),
        // Opaque up to 0.55 of the width, transparent after: the blur must not pull the values
        // near that edge towards the zeros of the transparent part.
        create(
            "/cut",
            "tex.ramp",
            &[
                ("width", Value::Int(w)),
                ("height", Value::Int(h)),
                (
                    "keys",
                    "0 0 0 0 1
0.55 0.55 0.55 0.55 1
0.551 0.55 0.55 0.55 0
1 1 1 1 0"
                        .into(),
                ),
            ],
        ),
        create(
            "/cut_lines",
            "tex.contour",
            &[("interval", Value::Float(0.1)), ("smooth", Value::Float(8.0)), ("mode", "alpha".into())],
        ),
        connect("/cut", "/cut_lines", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let raw = rig.pixels("/raw");
    let calm = rig.pixels("/calm");
    let mut raw_runs = 0;
    for y in [6, 12, 18] {
        raw_runs += lit_runs(&raw, y).len();
        let starts = lit_runs(&calm, y);
        assert_eq!(starts.len(), 9, "row {y}: {starts:?}");
        for (i, x) in starts.iter().enumerate() {
            let want = 20.0 * (i + 1) as f32;
            assert!((*x as f32 - want).abs() <= 4.0, "row {y}: line {i} at {x}, not near {want}");
        }
    }
    assert!(raw_runs > 27, "the noise breaks the unsmoothed lines: {raw_runs} runs in three rows");
    let cut = rig.pixels("/cut_lines");
    let alpha = |x: u32| cut.get(x, 12).map_or(0.0, |p| p[3]);
    let lit: Vec<u32> =
        (0..w as u32).filter(|&x| alpha(x) > 0.5 && (x == 0 || alpha(x - 1) <= 0.5)).collect();
    assert_eq!(lit.len(), 5, "levels 0.1 to 0.5, none along the transparent edge: {lit:?}");
    assert!((104..200).all(|x| alpha(x) < 0.1), "{:?}", (104..120).map(alpha).collect::<Vec<_>>());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn contour_lines_are_as_wide_on_a_diagonal_as_across() {
    let Some(gpu) = gpu() else { return };
    // One level crossed by a horizontal ramp and by a radial one: the lit area per unit of line
    // length must match whatever the direction.
    let ops = [
        create(
            "/across",
            "tex.ramp",
            &[
                ("width", Value::Int(128)),
                ("height", Value::Int(128)),
                (
                    "keys",
                    "0 0 0 0
1 1 1 1"
                        .into(),
                ),
            ],
        ),
        create(
            "/round",
            "tex.ramp",
            &[
                ("width", Value::Int(128)),
                ("height", Value::Int(128)),
                (
                    "keys",
                    "0 0 0 0
1 1 1 1"
                        .into(),
                ),
                ("shape", "radial".into()),
            ],
        ),
        create("/a", "tex.contour", &[("interval", Value::Float(0.5)), ("thickness", Value::Float(3.0))]),
        create("/r", "tex.contour", &[("interval", Value::Float(0.5)), ("thickness", Value::Float(3.0))]),
        connect("/across", "/a", 0),
        connect("/round", "/r", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let sum = |px: &Pixels| px.data.iter().map(|p| f64::from(p[0])).sum::<f64>();
    let across = sum(&rig.pixels("/a")) / 128.0;
    assert!((across - 3.0).abs() < 0.3, "a line 3 px wide: {across}");
    // The radial ramp's level 0.5 is a circle; its length from the lit area at the right width.
    let round = rig.pixels("/r");
    let lit = sum(&round);
    let rows: Vec<u32> = (0..128).filter(|&y| (0..128).any(|x| round.get(x, y).unwrap()[0] > 0.5)).collect();
    let diameter = f64::from(rows.len() as u32);
    let width = lit / (std::f64::consts::PI * diameter);
    assert!((width - 3.0).abs() < 0.45, "the circle's line is {width} px wide (diameter {diameter})");
}

/// A gentle grey rise from 0 at the left to 0.25 at the right, stored in 8-bit steps (four
/// pixels per step) after dither noise of +-half a step: plateaus whose pixels all share a value,
/// with ragged borders between them, as in a measured height map.
fn write_stepped_rise(path: &Path, w: u32, h: u32) {
    let encode = |v: f32| if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    let mut samples = Vec::new();
    let mut seed = 11u32;
    for _ in 0..h {
        for x in 0..w {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = ((seed >> 8) as f32 / 16_777_216.0 - 0.5) / 255.0;
            let v = (0.25 * x as f32 / w as f32 + noise).clamp(0.0, 1.0);
            let g = (encode((v * 255.0).round() / 255.0) * 65535.0).round() as u16;
            samples.extend([g, g, g, 65535]);
        }
    }
    let img =
        photocraft_codecs::Image::from_u16(w, h, photocraft_codecs::ChannelLayout::Rgba, &samples).unwrap();
    let png = photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap();
    std::fs::write(path, png).unwrap();
}

#[test]
fn thick_contour_lines_hold_together_on_a_stepped_map() {
    let Some(gpu) = gpu() else { return };
    let (w, h) = (256, 32);
    let dir = scratch("contour_stepped");
    write_stepped_rise(&dir.join("stepped.png"), w, h);
    let ops = [
        create("/stepped", "tex.image", &[("file", "stepped.png".into())]),
        create(
            "/thick",
            "tex.contour",
            &[("interval", Value::Float(0.05)), ("thickness", Value::Float(4.0))],
        ),
        connect("/stepped", "/thick", 0),
        // Opaque up to 0.55 of the width, transparent after: the fit must not see the zeros
        // there and draw a line along the edge.
        create(
            "/cut",
            "tex.ramp",
            &[
                ("width", Value::Int(200)),
                ("height", Value::Int(8)),
                (
                    "keys",
                    "0 0 0 0 1
0.55 0.55 0.55 0.55 1
0.551 0.55 0.55 0.55 0
1 1 1 1 0"
                        .into(),
                ),
            ],
        ),
        create(
            "/cut_lines",
            "tex.contour",
            &[("interval", Value::Float(0.1)), ("thickness", Value::Float(4.0)), ("mode", "alpha".into())],
        ),
        connect("/cut", "/cut_lines", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let px = rig.pixels("/thick");
    let lit = |x: u32, y: u32| px.get(x, y).is_some_and(|p| p[0] > 0.5);
    // Levels 0.05 to 0.2 lie at x = 51, 102, 154, 205; every row crosses each of them in one
    // unbroken run about four pixels wide.
    for y in 0..h {
        for level in 1..=4u32 {
            let at = (f64::from(level) * 0.05 / 0.25 * f64::from(w)).round() as u32;
            let window = at - 12..at + 12;
            let xs: Vec<u32> = window.clone().filter(|&x| lit(x, y)).collect();
            let row: String = window.map(|x| if lit(x, y) { '#' } else { '.' }).collect();
            let (Some(first), Some(last)) = (xs.first(), xs.last()) else {
                panic!("row {y}: no line at level {level}: {row}");
            };
            assert_eq!((last - first + 1) as usize, xs.len(), "row {y}, level {level}: a hole: {row}");
            assert!((2..=7).contains(&xs.len()), "row {y}, level {level}: {} px wide: {row}", xs.len());
        }
    }
    let cut = rig.pixels("/cut_lines");
    let alpha = |x: u32| cut.get(x, 4).map_or(0.0, |p| p[3]);
    let lit: Vec<u32> = (0..200).filter(|&x| alpha(x) > 0.5 && (x == 0 || alpha(x - 1) <= 0.5)).collect();
    assert_eq!(lit.len(), 5, "levels 0.1 to 0.5, none along the transparent edge: {lit:?}");
    assert!((104..200).all(|x| alpha(x) < 0.1), "{:?}", (100..120).map(alpha).collect::<Vec<_>>());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn select_follows_the_named_node_and_switch_picks_or_blends() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        constant("/a", 0.2),
        constant("/b", 0.6),
        create("/sel", "tex.select", &[("node", "/a".into())]),
        create("/sw", "tex.switch", &[("index", Value::Float(1.0))]),
        connect("/a", "/sw", 0),
        connect("/b", "/sw", 3),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    assert!((red(&mut rig, "/sel") - 0.2).abs() < 0.01);
    rig.edit(&[set("/a.color", Value::Vec4([0.4, 0.0, 0.0, 1.0]))]);
    assert!((red(&mut rig, "/sel") - 0.4).abs() < 0.01, "select recooks when its node changes");
    // Index 1 is the second connected input, whatever its slot.
    assert!((red(&mut rig, "/sw") - 0.6).abs() < 0.01);
    rig.edit(&[set("/sw.index", Value::Float(0.25)), set("/sw.blend", Value::Bool(true))]);
    assert!((red(&mut rig, "/sw") - 0.45).abs() < 0.01, "a quarter of the way from 0.4 to 0.6");
    rig.edit(&[set("/sw.index", Value::Float(7.0))]);
    assert!((red(&mut rig, "/sw") - 0.6).abs() < 0.01, "past the last is the last");
}

#[test]
fn a_switch_that_skips_cooks_only_what_it_shows() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        constant("/a", 0.2),
        constant("/b", 0.6),
        constant("/c", 0.8),
        create("/sw", "tex.switch", &[("index", Value::Float(1.0)), ("inactive", "skip".into())]),
        connect("/a", "/sw", 0),
        connect("/b", "/sw", 3),
        connect("/c", "/sw", 5),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    // Index 1 is the second wired input, whatever its slot, though the first never cooked.
    assert!((red(&mut rig, "/sw") - 0.6).abs() < 0.01);
    let cooked = |rig: &Rig, p: &str| rig.cooker.outputs(rig.id(p)).is_some();
    assert!(!cooked(&rig, "/a") && !cooked(&rig, "/c"));
    rig.edit(&[set("/sw.index", Value::Float(1.5)), set("/sw.blend", Value::Bool(true))]);
    assert!((red(&mut rig, "/sw") - 0.7).abs() < 0.01, "half way from 0.6 to 0.8");
    assert!(!cooked(&rig, "/a"));
    rig.edit(&[set("/sw.index", Value::Float(-3.0))]);
    assert!((red(&mut rig, "/sw") - 0.2).abs() < 0.01, "before the first is the first");
    // An unwired switch waits, as it does when cooking everything.
    rig.edit(&[create("/empty", "tex.switch", &[("inactive", "skip".into())])]);
    rig.cook("/empty");
    assert!(rig.tex("/empty").is_none());
}

#[test]
fn cache_outputs_frames_back() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        constant("/c", 0.0),
        create("/cache", "tex.cache", &[("length", Value::Int(3)), ("index", Value::Int(2))]),
        connect("/c", "/cache", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    for i in 1..=4 {
        rig.edit(&[set("/c.color", Value::Vec4([f64::from(i) * 0.1, 0.0, 0.0, 1.0]))]);
        rig.clock.advance(1);
        rig.cook("/cache");
    }
    // Kept: 0.4, 0.3, 0.2 (newest first); two back is 0.2.
    assert!((red(&mut rig, "/cache") - 0.2).abs() < 0.01);
    rig.edit(&[set("/cache.index", Value::Int(0)), set("/cache.record", Value::Bool(false))]);
    assert!((red(&mut rig, "/cache") - 0.4).abs() < 0.01);
    rig.edit(&[set("/c.color", Value::Vec4([0.9, 0.0, 0.0, 1.0]))]);
    rig.clock.advance(1);
    assert!((red(&mut rig, "/cache") - 0.4).abs() < 0.01, "not recording holds what is kept");

    // Held frames are state; a second cooker (a restart) takes them back.
    let kept = rig.cooker.kept();
    let [(id, penumbra_graph::Kept::State(items))] = kept.as_slice() else { panic!("{kept:?}") };
    assert_eq!((*id, items.len()), (rig.id("/cache"), 3));
    let mut again = Rig::new(rig.gpu.clone(), None, &ops);
    again.edit(&[set("/cache.index", Value::Int(1)), set("/cache.record", Value::Bool(false))]);
    again.cooker.cook(&again.net, again.clock.now(), &[], &[]);
    again.cooker.restore(again.id("/cache"), penumbra_graph::Kept::State(items.clone())).unwrap();
    assert!((red(&mut again, "/cache") - 0.3).abs() < 0.01);
    // A recording cache keeps nothing: it refills by itself.
    rig.edit(&[set("/cache.record", Value::Bool(true))]);
    rig.clock.advance(1);
    rig.cook("/cache");
    assert!(rig.cooker.kept().is_empty());
}

#[test]
fn time_machine_looks_back_as_far_as_the_map_says() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        constant("/c", 0.0),
        create(
            "/white",
            "tex.constant",
            &[("width", Value::Int(8)), ("height", Value::Int(4)), ("color", Value::Vec4([1.0; 4]))],
        ),
        create("/tm", "tex.time_machine", &[("length", Value::Int(2))]),
        connect("/c", "/tm", 0),
        connect("/white", "/tm", 1),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    for i in 1..=4 {
        rig.edit(&[set("/c.color", Value::Vec4([f64::from(i) * 0.1, 0.0, 0.0, 1.0]))]);
        rig.clock.advance(1);
        rig.cook("/tm");
    }
    // Kept: 0.4, 0.3, 0.2 (newest first); white is two back.
    assert!((red(&mut rig, "/tm") - 0.2).abs() < 0.01);
    rig.clock.advance(1);
    assert!((red(&mut rig, "/tm") - 0.2).abs() < 0.01, "the same input frame is not taken twice");
    // Without a map, the offset delays everything; not recording holds what is kept.
    rig.edit(&[
        Op::Disconnect { to: "/tm".parse().unwrap(), input: 1 },
        set("/tm.offset", Value::Float(1.0)),
        set("/tm.record", Value::Bool(false)),
        set("/c.color", Value::Vec4([0.9, 0.0, 0.0, 1.0])),
    ]);
    assert!((red(&mut rig, "/tm") - 0.3).abs() < 0.01);
    // Past the budget fewer frames are kept, with a warning.
    rig.edit(&[set("/tm.length", Value::Int(1_000_000)), set("/tm.record", Value::Bool(true))]);
    rig.cook("/tm");
    let s = rig.cooker.status(rig.id("/tm")).unwrap();
    let most = penumbra_gpu::Stack::capacity(&rig.gpu, 8, 4, penumbra_gpu::stack::Precision::Half).unwrap();
    let keeping = format!("keeping {most} ");
    assert!(
        s.warning.as_deref().is_some_and(|w| w.contains(&keeping) && w.contains("one stack holds")),
        "{s:?}"
    );
    assert!((red(&mut rig, "/tm") - 0.9).abs() < 0.01, "a new stack starts from the current frame");
}

#[test]
fn time_machine_out_of_memory_keeps_fewer_frames_and_does_not_retry_every_cook() {
    // A device of its own: the stack limit applies to every stack of a device.
    let Ok(gpu) = Gpu::headless(AdapterChoice::Software) else { return };
    let ops = [
        create(
            "/c",
            "tex.constant",
            &[("width", Value::Int(256)), ("height", Value::Int(256)), ("color", Value::Vec4([0.5; 4]))],
        ),
        create("/tm", "tex.time_machine", &[("length", Value::Int(100_000)), ("budget", Value::Float(1.0))]),
        connect("/c", "/tm", 0),
    ];
    // A 256 x 256 frame at 16 bits is 0.5 MiB: 256 to a chunk. Room for two chunks.
    let per = penumbra_gpu::Stack::bytes_for(256, 256, 1, penumbra_gpu::stack::Precision::Half);
    gpu.set_stack_limit(Some(per * 256 * 2));
    let mut rig = Rig::new(gpu.clone(), None, &ops);
    let before = gpu.stack_attempts();
    for i in 0..5 {
        rig.clock.advance(1);
        rig.edit(&[set("/c.color", Value::Vec4([f64::from(i) * 0.1, 0.0, 0.0, 1.0]))]);
        rig.cook("/tm");
    }
    let s = rig.cooker.status(rig.id("/tm")).unwrap();
    assert_eq!(s.error, None);
    let w = s.warning.unwrap_or_default();
    assert!(w.contains("keeping 512 ") && w.contains("refused more memory"), "{w}");
    // Two created, the third refused, once; not again on the next four cooks.
    assert_eq!(gpu.stack_attempts() - before, 3);
    assert!((red(&mut rig, "/tm") - 0.4).abs() < 0.01, "the shorter stack works");

    // Not even one chunk: an error, again without a retry on every cook.
    gpu.set_stack_limit(Some(per));
    rig.edit(&[set("/tm.budget", Value::Float(2.0))]);
    let before = gpu.stack_attempts();
    for i in 0..5 {
        rig.clock.advance(1);
        rig.edit(&[set("/c.color", Value::Vec4([f64::from(i) * 0.1, 0.0, 0.0, 1.0]))]);
        rig.cook("/tm");
    }
    let s = rig.cooker.status(rig.id("/tm")).unwrap();
    assert!(s.error.as_deref().is_some_and(|e| e.contains("tried again when")), "{s:?}");
    assert_eq!(gpu.stack_attempts() - before, 1);
    // A param changing is the retry.
    gpu.set_stack_limit(None);
    rig.edit(&[set("/tm.length", Value::Int(10))]);
    rig.cook("/tm");
    let s = rig.cooker.status(rig.id("/tm")).unwrap();
    assert_eq!((s.error, s.warning), (None, None));
    assert_eq!(gpu.errors().count, 0);
}

#[test]
fn resizing_nodes_set_their_resolution() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        constant("/c", 0.5),
        create("/half", "tex.resolution", &[("multiply", Value::Float(0.5))]),
        create(
            "/custom",
            "tex.resolution",
            &[("mode", "custom".into()), ("width", Value::Int(5)), ("height", Value::Int(3))],
        ),
        create("/fit", "tex.fit", &[("width", Value::Int(16)), ("height", Value::Int(16))]),
        create("/flop", "tex.flip", &[("flop", Value::Bool(true))]),
        connect("/c", "/half", 0),
        connect("/c", "/custom", 0),
        connect("/c", "/fit", 0),
        connect("/c", "/flop", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let size = |rig: &mut Rig, p: &str| rig.cook_until_new(p, 0).size();
    assert_eq!(size(&mut rig, "/half"), (4, 2));
    assert_eq!(size(&mut rig, "/custom"), (5, 3));
    assert_eq!(size(&mut rig, "/fit"), (16, 16));
    assert_eq!(size(&mut rig, "/flop"), (4, 8));
    // Fit keeps the aspect: an 8 x 4 image in a 16 x 16 frame is 16 x 8, centred.
    let px = rig.gpu.read(&rig.tex("/fit").unwrap()).unwrap();
    assert_eq!(px.get(8, 0).unwrap()[3], 0.0, "transparent above");
    assert!((px.get(8, 8).unwrap()[0] - 0.5).abs() < 0.01, "the image in the middle");
    // Half the input's size: the input needs twice what the output needs. A custom size and a
    // fit set their own size; the fit's input spans half the frame's height.
    assert_eq!(rig.cooker.density(rig.id("/half"), 0), Some(Density::needs([2.0; 2])));
    assert_eq!(rig.cooker.density(rig.id("/custom"), 0), Some(Density::SAME));
    assert!(!rig.cooker.sets_size(rig.id("/half")));
    assert!(rig.cooker.sets_size(rig.id("/custom")));
    assert!(rig.cooker.sets_size(rig.id("/fit")));
    let fit = rig.cooker.density(rig.id("/fit"), 0).unwrap().demand([16.0, 16.0]).unwrap();
    assert!((fit[0] - 16.0).abs() < 1e-9 && (fit[1] - 8.0).abs() < 1e-9, "{fit:?}");
    let flop = rig.cooker.density(rig.id("/flop"), 0).unwrap().demand([4.0, 8.0]).unwrap();
    assert_eq!(flop, [8.0, 4.0]);
}

#[test]
fn mappers_on_one_surface_blend_with_each_other() {
    use crate::tex::mapper::Mappers;
    let Some(gpu) = gpu() else { return };
    let small = || [("width", Value::Int(120)), ("height", Value::Int(8))];
    let white = [("color", Value::Vec4([1.0; 4])), ("width", Value::Int(64)), ("height", Value::Int(16))];
    let mapper = |x: f64| {
        let mut p = small().to_vec();
        p.push(("region", Value::Vec4([x, 0.0, 0.6, 1.0])));
        p.push(("fit", Value::Text("stretch".into())));
        p
    };
    let mut rig = Rig::new(
        gpu,
        None,
        &[
            create("/c", "tex.constant", &white),
            create("/d", "tex.constant", &white),
            create("/a", "tex.mapper", &mapper(0.0)),
            create("/b", "tex.mapper", &mapper(0.4)),
            create("/x", "tex.mapper", &mapper(0.4)),
            connect("/c", "/a", 0),
            connect("/c", "/b", 0),
            connect("/d", "/x", 0),
        ],
    );
    let mappers = Mappers::new();
    rig.cooker.provide(mappers.clone());
    let ids = [rig.id("/a"), rig.id("/b"), rig.id("/x")];
    // The first pass publishes the regions and wakes the others; the second blends.
    for _ in 0..3 {
        rig.cooker.cook(&rig.net, rig.clock.now(), &ids, &[]);
        rig.clock.advance(1);
    }
    let read = |rig: &Rig, path: &str| rig.gpu.read(&rig.tex(path).unwrap()).unwrap();
    let (pa, pb, px) = (read(&rig, "/a"), read(&rig, "/b"), read(&rig, "/x"));
    // Surface 0.5 is pixel 99.5 of /a and 19.5 of /b: half each; /x is on another surface.
    let (va, vb) = (pa.get(99, 4).unwrap()[0], pb.get(20, 4).unwrap()[0]);
    assert!((va + vb - 1.0).abs() < 0.05 && (va - 0.5).abs() < 0.1, "{va} {vb}");
    assert!((px.get(20, 4).unwrap()[0] - 1.0).abs() < 1e-3);
    assert!(mappers.mapping(ids[0]).is_some());

    // /b stops blending: /a takes the overlap fully again.
    rig.edit(&[set("/b.blend", Value::Bool(false))]);
    for _ in 0..3 {
        rig.cooker.cook(&rig.net, rig.clock.now(), &ids, &[]);
        rig.clock.advance(1);
    }
    assert!((read(&rig, "/a").get(99, 4).unwrap()[0] - 1.0).abs() < 1e-3);

    // A broken warp text is an error on the node.
    rig.edit(&[set("/a.warp", Value::Text("0,0 1".into()))]);
    rig.cooker.cook(&rig.net, rig.clock.now(), &ids, &[]);
    assert!(rig.cooker.status(ids[0]).unwrap().error.unwrap().contains("warp"));

    // Deleting /a wakes /b, which no longer has it as a peer.
    rig.edit(&[Op::Delete { path: "/a".parse().unwrap() }]);
    mappers.prune(&rig.net);
    assert!(mappers.mapping(ids[0]).is_none());
}

#[test]
fn shapes_give_masks_per_tag_a_distance_field_outlines_and_a_table() {
    use penumbra_core::Table;
    let Some(gpu) = gpu() else { return };
    // 96 x 64: the window is pixels 24..48 x 16..32, three copies 24 px apart (one row).
    let text = "window rect 0.25,0.25 0.25,0.25 repeat 3x1 0.25,0\ndoor ellipse 0,0.5 0.25,0.5\n";
    let mut rig = Rig::new(
        gpu,
        None,
        &[
            create(
                "/s",
                "tex.shapes",
                &[
                    ("width", Value::Int(W)),
                    ("height", Value::Int(H)),
                    ("shapes", Value::Text(text.into())),
                    ("tag", Value::Text("window".into())),
                ],
            ),
            create(
                "/doors",
                "tex.shapes",
                &[size()[0].clone(), size()[1].clone(), ("tag", Value::Text("door".into()))],
            ),
            Op::Connect { from: "/s".parse().unwrap(), output: 3, to: "/doors".parse().unwrap(), input: 0 },
        ],
    );
    let mask = rig.pixels("/s");
    assert_eq!(mask.get(30, 20).unwrap(), [1.0; 4], "inside the first window");
    assert_eq!(mask.get(80, 20).unwrap(), [1.0; 4], "inside the third copy");
    assert_eq!(mask.get(12, 48).unwrap(), [0.0; 4], "the door is not a window");
    let out = |rig: &Rig, path: &str, o: u32| {
        rig.cooker.output(Port { node: rig.id(path), output: o }).cloned().unwrap()
    };
    let sdf = rig.gpu.read(out(&rig, "/s", 1).get::<Tex>().unwrap()).unwrap();
    // Pixel 10's centre is 13.5 px left of the first window: 13.5 / 96 surface widths, growing leftwards.
    let v = sdf.get(10, 20).unwrap();
    assert!((v[0] - 13.5 / 96.0).abs() < 1e-3 && (v[1] + 1.0).abs() < 1e-3, "{v:?}");
    let outline = rig.gpu.read(out(&rig, "/s", 2).get::<Tex>().unwrap()).unwrap();
    assert!(outline.get(24, 20).unwrap()[3] > 0.9, "the window's left edge");
    assert!(outline.get(12, 32).unwrap()[3] > 0.9, "the door's top, though not in the mask");
    let table = out(&rig, "/s", 3);
    let table = table.get::<Table>().unwrap();
    assert_eq!(table.num_rows(), 5);
    assert_eq!(table.cell(3, 3), "0.75");

    // The second node draws the doors of the first one's table.
    rig.cook("/doors");
    let doors = rig.gpu.read(&rig.tex("/doors").unwrap()).unwrap();
    assert_eq!(doors.get(12, 48).unwrap(), [1.0; 4]);
    assert_eq!(doors.get(30, 20).unwrap(), [0.0; 4]);

    // A broken line is an error naming it; the last picture stays.
    rig.edit(&[set("/s.shapes", Value::Text("window rect 0,0".into()))]);
    rig.cook("/s");
    let error = rig.cooker.status(rig.id("/s")).unwrap().error.unwrap();
    assert!(error.contains("line 1"), "{error}");
    assert!(rig.tex("/s").is_some());
}

#[test]
fn shape_content_fills_and_chases_shapes_by_their_ids() {
    let Some(gpu) = gpu() else { return };
    // 96 x 64: the window is pixels 24..48 x 16..32, the book 60..84 x 32..48.
    let text = "window rect 0.25,0.25 0.25,0.25
book rect 0.625,0.5 0.25,0.25
";
    let white = Value::Vec4([1.0; 4]);
    let mut rig = Rig::new(
        gpu,
        None,
        &[
            create(
                "/s",
                "tex.shapes",
                &[size()[0].clone(), size()[1].clone(), ("shapes", Value::Text(text.into()))],
            ),
            create(
                "/c",
                "tex.shape_content",
                &[
                    ("mode", Value::Text("fill".into())),
                    ("color_a", white.clone()),
                    ("variety", Value::Float(0.0)),
                ],
            ),
            Op::Connect { from: "/s".parse().unwrap(), output: 4, to: "/c".parse().unwrap(), input: 0 },
        ],
    );
    let fill = rig.pixels("/c");
    let ids = rig.cooker.output(Port { node: rig.id("/s"), output: 4 }).cloned().unwrap();
    let ids = rig.gpu.read(ids.get::<Tex>().unwrap()).unwrap();
    // The id is the table's row; alpha the distance to the own edge (4.5 below the top).
    let v = ids.get(30, 20).unwrap();
    assert!(v[0] == 0.0 && (v[3] + 4.5).abs() < 0.01, "{v:?}");
    assert_eq!(ids.get(70, 40).unwrap()[0], 1.0);
    assert_eq!(fill.get(30, 20).unwrap(), [1.0; 4]);
    assert_eq!(fill.get(70, 40).unwrap(), [1.0; 4]);
    assert_eq!(fill.get(5, 5).unwrap(), [0.0; 4]);

    // Standing still, one segment over the first half of the outline: from the top left
    // clockwise, the top edge is lit, the bottom edge is not, the inside is off the line.
    rig.edit(&[
        set("/c.mode", Value::Text("chase".into())),
        set("/c.speed", Value::Float(0.0)),
        set("/c.scatter", Value::Float(0.0)),
        set("/c.segments", Value::Float(1.0)),
        set("/c.duty", Value::Float(0.5)),
        set("/c.softness", Value::Float(0.0)),
        set("/c.line", Value::Float(4.0)),
    ]);
    let after = rig.version("/c");
    let chase = rig.cook_until_new("/c", after);
    let chase = rig.gpu.read(&chase).unwrap();
    assert_eq!(chase.get(30, 16).unwrap()[3], 1.0, "top edge");
    assert_eq!(chase.get(30, 31).unwrap()[3], 0.0, "bottom edge");
    assert_eq!(chase.get(36, 24).unwrap()[3], 0.0, "inside");
}

#[test]
fn resolution_analysis_walks_back_from_the_projectors() {
    use crate::out::{BoardState, DisplayInfo, ProjectorState};
    use crate::res::{Scene, Verdict, analyze, pixel_params};
    use crate::tex::mapper::Mappers;
    let Some(gpu) = gpu() else { return };
    let dims = |w: i64, h: i64| [("width", Value::Int(w)), ("height", Value::Int(h))];
    let mut mapper = dims(192, 108).to_vec();
    mapper.push(("region", Value::Vec4([0.0, 0.0, 0.5, 1.0])));
    let mut rig = Rig::new(
        gpu,
        None,
        &[
            // Small, scaled up by 2 and blurred, half of it on a 192 x 108 projector: far too
            // coarse. The constant can grow, the transform and the blur follow it.
            create("/c", "tex.constant", &dims(16, 9)),
            create("/t", "tex.transform", &[("scale", Value::Vec2([2.0, 2.0]))]),
            create("/b", "tex.blur", &[]),
            create("/m", "tex.mapper", &mapper),
            create("/p", "out.projector", &[]),
            connect("/c", "/t", 0),
            connect("/t", "/b", 0),
            connect("/b", "/m", 0),
            connect("/m", "/p", 0),
            // Large content through a mapper smaller than its projector: the mapper is too
            // coarse, the content wasted.
            create("/big", "tex.constant", &dims(384, 216)),
            create("/m2", "tex.mapper", &dims(128, 72)),
            create("/p2", "out.projector", &[]),
            connect("/big", "/m2", 0),
            connect("/m2", "/p2", 0),
        ],
    );
    let mappers = Mappers::new();
    rig.cooker.provide(mappers.clone());
    // Both projectors on a display of 192 x 108 (small, so WARP is quick).
    let shown = ProjectorState { display: Some("d".into()), ..ProjectorState::default() };
    let board = BoardState {
        managed: true,
        displays: vec![DisplayInfo {
            id: "d".into(),
            monitor: "test".into(),
            size: [192, 108],
            ..DisplayInfo::default()
        }],
        projectors: [(rig.id("/p"), shown.clone()), (rig.id("/p2"), shown)].into(),
        ..BoardState::default()
    };
    let cook = |rig: &mut Rig| {
        let ids = [rig.id("/p"), rig.id("/p2")];
        for _ in 0..2 {
            rig.cooker.cook(&rig.net, rig.clock.now(), &ids, &[]);
            rig.clock.advance(1);
        }
    };
    let analysis = |rig: &Rig| {
        analyze(&Scene {
            net: &rig.net,
            types: &rig.types,
            cooker: &rig.cooker,
            board: Some(&board),
            max_side: 16384,
        })
    };
    cook(&mut rig);
    let report = analysis(&rig);
    let node = |r: &crate::res::Report, p: &str| r.nodes.iter().find(|n| n.path == p).unwrap().clone();
    assert_eq!(report.projectors.len(), 2);
    assert!(
        report.projectors.iter().all(|p| !p.assumed && p.raster == [192, 108]),
        "{:?}",
        report.projectors
    );
    let m = node(&report, "/m");
    assert_eq!((m.needed, m.verdict), ([192, 108], Verdict::Ok), "{m:?}");
    // Half the surface fills 96 pixels across (fitted, so its height fills 108).
    let b = node(&report, "/b");
    assert_eq!((b.needed, b.verdict, b.sets_size, b.suggested), ([192, 108], Verdict::Coarse, false, None));
    let c = node(&report, "/c");
    assert_eq!(c.needed, [384, 216], "scaled up by 2: twice the surface's need");
    assert_eq!((c.sets_size, c.suggested), (true, Some([384, 216])));
    let (m2, big) = (node(&report, "/m2"), node(&report, "/big"));
    assert_eq!((m2.verdict, m2.suggested), (Verdict::Coarse, Some([192, 108])));
    assert_eq!((big.needed, big.verdict, big.suggested), ([192, 108], Verdict::Wasted, Some([192, 108])));
    // The overlay of /m shows the constant, twice as coarse as the blur it sees.
    let o = report.overlays.get(&rig.id("/m")).unwrap();
    assert!((o.factor - 0.5).abs() < 1e-3 && o.scale == [1.0, 1.0], "{o:?}");
    // The blur's radius is in pixels: its look changes when the constant grows.
    assert_eq!(pixel_params(&rig.net, &rig.types, &rig.cooker, &[rig.id("/c")]), ["/b.size"]);

    // The overlay on the pictures: red where too coarse, through the constant behind /m and
    // through /m2 itself (its surface alone would be blue).
    mappers.set_overlays(report.overlays.clone());
    cook(&mut rig);
    let centre = |rig: &Rig, p: &str| {
        let px = rig.gpu.read(&rig.tex(p).unwrap()).unwrap();
        px.get(px.width / 2, px.height / 2).unwrap()
    };
    let is_red = |c: [f32; 4]| c[0] > 0.2 && c[0] > 4.0 * c[1].max(c[2]);
    let is_blue = |c: [f32; 4]| c[2] > 0.2 && c[2] > 2.0 * c[0].max(c[1]);
    assert!(is_red(centre(&rig, "/m")), "{:?}", centre(&rig, "/m"));
    assert!(is_red(centre(&rig, "/m2")), "{:?}", centre(&rig, "/m2"));
    mappers.set_overlays(Default::default());

    // Optimise, pinning the large constant: the rest gets its suggestion.
    rig.edit(&[set("/big.keep_size", Value::Bool(true))]);
    cook(&mut rig);
    let ops = analysis(&rig).optimize_ops(&rig.net, None);
    assert_eq!(ops.len(), 4, "{ops:?}");
    rig.edit(&ops);
    cook(&mut rig);
    let after = analysis(&rig);
    assert_eq!(node(&after, "/c").verdict, Verdict::Ok);
    assert_eq!(node(&after, "/m2").verdict, Verdict::Ok);
    assert_eq!(node(&after, "/big").verdict, Verdict::Wasted);
    // The surface follows the constant at twice what it needs; the weakest link is the
    // constant, at one to one, so the overlay halves the surface's two.
    assert_eq!(node(&after, "/b").verdict, Verdict::Wasted);
    let o = after.overlays.get(&rig.id("/m")).unwrap();
    assert!((o.factor - 0.5).abs() < 1e-3, "{o:?}");
    // /m2 is fine now; the pinned constant behind it is twice as fine as needed: blue.
    mappers.set_overlays(after.overlays.clone());
    cook(&mut rig);
    assert!(is_blue(centre(&rig, "/m2")), "{:?}", centre(&rig, "/m2"));
    assert!(!is_red(centre(&rig, "/m")) && !is_blue(centre(&rig, "/m")), "{:?}", centre(&rig, "/m"));
}

mod fluid;
mod geo;
mod layers;

/// A grey camera frame of 32 x 16 pixels, luma `y` give or take `noise` codes at random. One
/// blue-difference sample follows the seed's parity, so consecutive seeds never make identical
/// frames (which `tex.video_in` skips as repeats); red does not see it.
fn noisy_frame(y: u8, noise: u8, seed: u32) -> penumbra_camera::CameraFrame {
    let mut state = seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let luma = (0..32 * 16)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let n = i32::from(noise);
            let offset = ((state >> 16) % (2 * n as u32 + 1)) as i32 - n;
            (i32::from(y) + offset).clamp(0, 255) as u8
        })
        .collect();
    penumbra_camera::CameraFrame {
        width: 32,
        height: 16,
        y: luma,
        cb: std::iter::once(128 + (seed % 2) as u8).chain(std::iter::repeat_n(128, 127)).collect(),
        cr: vec![128; 128],
        ..camera_frame(y, 128, 128)
    }
}

/// Feeds a frame and cooks `/dn` until it has made something from it.
fn denoise_frame(rig: &mut Rig, frame: penumbra_camera::CameraFrame) -> penumbra_gpu::Pixels {
    let v = rig.cooker.status(rig.id("/dn")).map_or(0, |s| s.version);
    rig.camera.feed(&penumbra_camera::Key::new("test", ""), frame);
    let tex = rig.cook_until_new("/dn", v);
    rig.gpu.read(&tex).unwrap()
}

/// Mean and standard deviation of the red channel.
fn red_stats(px: &penumbra_gpu::Pixels) -> (f64, f64) {
    let values: Vec<f64> = (0..px.height)
        .flat_map(|y| (0..px.width).map(move |x| (x, y)))
        .map(|(x, y)| f64::from(px.get(x, y).unwrap()[0]))
        .collect();
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
    (mean, var.sqrt())
}

#[test]
fn denoise_calms_noise_follows_motion_and_reaches_small_changes() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        create("/cam", "tex.video_in", &[("device", "test".into())]),
        create("/dn", "tex.denoise", &[]),
        connect("/cam", "/dn", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);

    // Noise in a still picture is averaged away.
    let mut out = None;
    for i in 0..30 {
        out = Some(denoise_frame(&mut rig, noisy_frame(128, 8, i)));
    }
    let out = out.unwrap();
    let raw = red_stats(&rig.pixels("/cam"));
    let calm = red_stats(&out);
    assert!(calm.1 < raw.1 * 0.5, "noise {raw:?} -> {calm:?}");
    assert!((calm.0 - raw.0).abs() < 0.01, "{raw:?} -> {calm:?}");

    // A real change shows at once.
    for i in 0..10 {
        denoise_frame(&mut rig, noisy_frame(60, 0, i));
    }
    let bright = denoise_frame(&mut rig, noisy_frame(200, 0, 0));
    let want = red_stats(&rig.pixels("/cam")).0;
    let got = red_stats(&bright);
    assert!((got.0 - want).abs() < 0.01 && got.1 < 0.01, "{got:?}, want {want}");
    // Smoothed where it moved, and still opaque: written as it is, the smoothing's 0.99999994
    // would become the 16-bit float below 1 on a GPU that rounds toward zero.
    let opaque = (0..32).all(|x| (0..16).all(|y| bright.get(x, y).unwrap()[3] == 1.0));
    assert!(opaque);

    // A change of one code, far below `motion`, is averaged in over many frames, and all the
    // way: rounded to the nearest 16-bit float, the average would stop well short of it.
    rig.edit(&[set("/dn.frames", Value::Float(64.0)), set("/dn.spatial", Value::Float(0.0))]);
    for i in 0..10 {
        denoise_frame(&mut rig, noisy_frame(126, 0, i));
    }
    let low = red_stats(&rig.pixels("/cam")).0;
    let mut last = None;
    for i in 0..400 {
        last = Some(denoise_frame(&mut rig, noisy_frame(127, 0, i)));
    }
    let want = red_stats(&rig.pixels("/cam")).0;
    let got = red_stats(&last.unwrap()).0;
    let step = want - low;
    assert!(step > 0.002, "one code is {step}");
    assert!((got - want).abs() < step * 0.1, "{got}, want {want}");
}

/// Standard deviations of luma and of colour (blue minus luma).
fn luma_colour_noise(px: &penumbra_gpu::Pixels) -> (f64, f64) {
    let (mut luma, mut colour) = (Vec::new(), Vec::new());
    for y in 0..px.height {
        for x in 0..px.width {
            let [r, g, b, _] = px.get(x, y).unwrap().map(f64::from);
            let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            luma.push(l);
            colour.push(b - l);
        }
    }
    let std = |v: &[f64]| {
        let m = v.iter().sum::<f64>() / v.len() as f64;
        (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
    };
    (std(&luma), std(&colour))
}

#[test]
fn denoise_treats_brightness_and_colour_noise_apart() {
    let Some(gpu) = gpu() else { return };
    // Noise in luma and in both chroma planes.
    let frame = |seed: u32| {
        let mut f = noisy_frame(128, 6, seed);
        let mut state = seed.wrapping_mul(2_654_435_761);
        for c in f.cb.iter_mut().chain(f.cr.iter_mut()) {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *c = (122 + (state >> 16) % 13) as u8;
        }
        f
    };
    let run = |params: &[(&str, Value)]| {
        let ops = [
            create("/cam", "tex.video_in", &[("device", "test".into())]),
            create("/dn", "tex.denoise", params),
            connect("/cam", "/dn", 0),
        ];
        let mut rig = Rig::new(gpu.clone(), None, &ops);
        let mut out = None;
        for i in 0..30 {
            out = Some(denoise_frame(&mut rig, frame(i)));
        }
        (luma_colour_noise(&rig.pixels("/cam")), luma_colour_noise(&out.unwrap()))
    };
    let off = |name: &'static str| (name, Value::Float(if name.ends_with("frames") { 1.0 } else { 0.0 }));

    let (raw, only_brightness) = run(&[off("color_frames"), off("color_spatial")]);
    assert!(only_brightness.0 < raw.0 * 0.5, "luma {raw:?} -> {only_brightness:?}");
    assert!(only_brightness.1 > raw.1 * 0.8, "colour {raw:?} -> {only_brightness:?}");

    let (raw, only_colour) = run(&[off("frames"), off("spatial")]);
    assert!(only_colour.0 > raw.0 * 0.8, "luma {raw:?} -> {only_colour:?}");
    assert!(only_colour.1 < raw.1 * 0.5, "colour {raw:?} -> {only_colour:?}");
}

/// A textured grey camera frame of 160 x 96 pixels, the pattern moved by `shift` pixels.
fn moved_frame(shift: [f64; 2]) -> penumbra_camera::CameraFrame {
    lit_frame(shift, 0.0)
}

/// [`moved_frame`], `brighter` codes brighter.
fn lit_frame(shift: [f64; 2], brighter: f64) -> penumbra_camera::CameraFrame {
    let (w, h) = (160u32, 96u32);
    let luma = (0..h)
        .flat_map(|y| (0..w).map(move |x| (f64::from(x) - shift[0], f64::from(y) - shift[1])))
        .map(|(x, y)| {
            let v = 128.0
                + brighter
                + 45.0 * (x * 0.23 + 0.7).sin() * (y * 0.19).cos()
                + 30.0 * ((x + 1.7 * y) * 0.09).sin()
                + 20.0 * ((x * 0.05).cos() * (y * 0.07 + 1.0).sin() * 3.0).sin();
            v.round().clamp(16.0, 235.0) as u8
        })
        .collect();
    penumbra_camera::CameraFrame {
        width: w,
        height: h,
        y: luma,
        cb: vec![128; (w * h / 4) as usize],
        cr: vec![128; (w * h / 4) as usize],
        ..camera_frame(128, 128, 128)
    }
}

/// Feeds a frame and returns `/of`'s motion once it has measured it: the median over the
/// inner part (away from the edges, where content enters and leaves), in input pixels.
fn flow_of(rig: &mut Rig, frame: penumbra_camera::CameraFrame) -> ([f64; 2], penumbra_gpu::Pixels) {
    let v = rig.cooker.status(rig.id("/of")).map_or(0, |s| s.version);
    rig.camera.feed(&penumbra_camera::Key::new("test", ""), frame);
    let tex = rig.cook_until_new("/of", v);
    let px = rig.gpu.read(&tex).unwrap();
    let (w, h) = (px.width, px.height);
    let median = |c: usize, scale: f64| {
        let mut v: Vec<f64> = (12..h - 12)
            .flat_map(|y| (12..w - 12).map(move |x| (x, y)))
            .map(|(x, y)| f64::from(px.get(x, y).unwrap()[c]) * scale)
            .collect();
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    ([median(0, 160.0), median(1, 96.0)], px)
}

#[test]
fn optical_flow_finds_known_motion() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        create("/cam", "tex.video_in", &[("device", "test".into())]),
        create("/of", "tex.optical_flow", &[("size", Value::Int(160)), ("confidence", Value::Bool(true))]),
        connect("/cam", "/of", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);

    // The first frame has nothing to compare with; a still picture does not move.
    let (first, px) = flow_of(&mut rig, moved_frame([0.0, 0.0]));
    assert_eq!((px.width, px.height), (160, 96));
    assert_eq!(first, [0.0, 0.0]);
    // (Not byte for byte the same frame, which the camera node skips.)
    let (still, _) = flow_of(&mut rig, lit_frame([0.0, 0.0], 0.4));
    assert_eq!(still, [0.0, 0.0]);

    // Slow, sub-pixel and fast motion (the last only through the pyramid), right and down
    // positive; also in the opposite direction.
    let mut at = [0.0, 0.0];
    for step in [[1.5, 0.75], [0.6, -0.4], [7.0, 3.0], [-5.0, 4.0]] {
        at = [at[0] + step[0], at[1] + step[1]];
        let (got, px) = flow_of(&mut rig, moved_frame(at));
        let err = ((got[0] - step[0]).powi(2) + (got[1] - step[1]).powi(2)).sqrt();
        assert!(err < 0.15, "moved {step:?}, measured {got:?}");
        let confident = f64::from(px.get(80, 48).unwrap()[2]);
        assert!(confident > 0.5, "confidence {confident}");
        assert_eq!(px.get(80, 48).unwrap()[3], 1.0);
    }

    // A flicker of the light is not motion, and does not throw off motion.
    let (flicker, _) = flow_of(&mut rig, lit_frame(at, 12.0));
    assert!(flicker[0].abs() < 0.1 && flicker[1].abs() < 0.1, "{flicker:?}");
    at[1] += 2.0;
    let (got, _) = flow_of(&mut rig, moved_frame(at));
    assert!((got[0]).abs() < 0.15 && (got[1] - 2.0).abs() < 0.15, "{got:?}");

    // As colours: moving right is mostly red, and bright.
    rig.edit(&[set("/of.colors", Value::Bool(true))]);
    at[0] += 3.0;
    let (_, px) = flow_of(&mut rig, moved_frame(at));
    let [r, g, b, a] = px.get(80, 48).unwrap();
    assert!(r > 0.9 && g < 0.3 && b < 0.3 && a == 1.0, "{:?}", [r, g, b, a]);
}

/// A plain grey square with a textured rim at `at`, over a still textured background, with
/// camera noise: a hand over a room, in short.
fn object_frame(at: [f64; 2], seed: u32) -> penumbra_camera::CameraFrame {
    let (w, h) = (160u32, 96u32);
    let mut state = seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let luma = (0..h)
        .flat_map(|y| (0..w).map(move |x| (f64::from(x), f64::from(y))))
        .map(|(x, y)| {
            let (u, v) = (x - at[0], y - at[1]);
            let inside = (0.0..56.0).contains(&u) && (0.0..44.0).contains(&v);
            let rim = inside && (u < 8.0 || u >= 48.0 || v < 8.0 || v >= 36.0);
            let value = if rim {
                150.0 + 40.0 * (u * 0.9).sin() * (v * 0.8).cos()
            } else if inside {
                150.0 + 3.0 * (u * 0.2).sin()
            } else {
                90.0 + 30.0 * (x * 0.3).sin() * (y * 0.25 + 1.0).cos()
            };
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = f64::from((state >> 16) % 5) - 2.0;
            (value + noise).round().clamp(16.0, 235.0) as u8
        })
        .collect();
    penumbra_camera::CameraFrame {
        width: w,
        height: h,
        y: luma,
        cb: vec![128; (w * h / 4) as usize],
        cr: vec![128; (w * h / 4) as usize],
        ..camera_frame(128, 128, 128)
    }
}

#[test]
fn optical_flow_fills_in_plain_parts_of_a_moving_thing() {
    let Some(gpu) = gpu() else { return };
    let interior_error = |fill: i64| {
        let ops = [
            create("/cam", "tex.video_in", &[("device", "test".into())]),
            create("/of", "tex.optical_flow", &[("size", Value::Int(160)), ("fill", Value::Int(fill))]),
            connect("/cam", "/of", 0),
        ];
        let mut rig = Rig::new(gpu.clone(), None, &ops);
        let step = [3.0, 1.5];
        let mut errors = Vec::new();
        for i in 0..4u32 {
            let at = [40.0 + f64::from(i) * step[0], 20.0 + f64::from(i) * step[1]];
            let (_, px) = flow_of(&mut rig, object_frame(at, i));
            if i == 0 {
                continue;
            }
            // The plain middle of the square, away from its rim.
            for y in (at[1] as u32 + 12)..(at[1] as u32 + 32) {
                for x in (at[0] as u32 + 12)..(at[0] as u32 + 44) {
                    let [r, g, ..] = px.get(x, y).unwrap();
                    let m = [f64::from(r) * 160.0, f64::from(g) * 96.0];
                    errors.push(((m[0] - step[0]).powi(2) + (m[1] - step[1]).powi(2)).sqrt());
                }
            }
        }
        errors.sort_by(f64::total_cmp);
        (errors[errors.len() / 2], errors[errors.len() * 9 / 10])
    };
    // Lucas-Kanade alone is unsure in the plain middle; filled in from the rim, it moves as one.
    let raw = interior_error(0);
    let filled = interior_error(4);
    assert!(raw.1 > 0.8, "{raw:?}");
    assert!(filled.0 < raw.0 * 0.6 && filled.1 < 0.5, "{raw:?} -> {filled:?}");
}

/// Cooks until `path` has channels.
fn chans_of(rig: &mut Rig, path: &str) -> penumbra_core::Chans {
    let id = rig.id(path);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        rig.cook(path);
        if let Some(c) = rig
            .cooker
            .output(Port { node: id, output: 0 })
            .and_then(|d| d.get::<penumbra_core::Chans>().cloned())
        {
            return c;
        }
        let s = rig.cooker.status(id).unwrap();
        assert!(s.error.is_none() && Instant::now() < deadline, "{path} produced nothing: {s:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A shader node writing its texture coordinates, u in red and v in green, at 1000 x 600.
fn uv_picture(dir: &Path) -> Op {
    let uv = "@fragment fn main(v: PnVertex) -> @location(0) vec4f { return vec4f(v.uv, 0.0, 1.0); }";
    std::fs::write(dir.join("uv.wgsl"), uv).unwrap();
    create(
        "/uv",
        "tex.shader",
        &[
            ("file", "uv.wgsl".into()),
            ("resolution", "custom".into()),
            ("width", Value::Int(1000)),
            ("height", Value::Int(600)),
        ],
    )
}

#[test]
fn a_picture_becomes_channels_of_its_mean_per_cell() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("from_tex");
    let ops = [
        uv_picture(&dir),
        create("/c", "chan.from_tex", &[("columns", Value::Int(4)), ("rows", Value::Int(2))]),
        connect("/uv", "/c", 0),
        create("/k", "tex.constant", &[("color", Value::Vec4([0.25, 0.5, 0.75, 1.0]))]),
        create("/mean", "chan.from_tex", &[("components", "luminance".into())]),
        connect("/k", "/mean", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let c = chans_of(&mut rig, "/c");
    let names: Vec<&str> = c.names().map(|n| n.as_str()).collect();
    assert_eq!(names, ["r0", "g0", "b0", "a0", "r1", "g1", "b1", "a1"]);
    assert_eq!(c.len(), 4);
    let near = |a: f64, b: f64| (a - b).abs() < 0.01;
    for (x, r) in c.get("r1").unwrap().samples.iter().enumerate() {
        assert!(near(*r, (x as f64 + 0.5) / 4.0), "column {x}: {r}");
    }
    assert!(c.get("g0").unwrap().samples.iter().all(|g| near(*g, 0.25)), "{c:?}");
    assert!(c.get("g1").unwrap().samples.iter().all(|g| near(*g, 0.75)), "{c:?}");
    assert!(c.get("a0").unwrap().samples.iter().all(|a| near(*a, 1.0)));
    let mean = chans_of(&mut rig, "/mean");
    let lum = 0.2126 * 0.25 + 0.7152 * 0.5 + 0.0722 * 0.75;
    assert!(near(mean.value(Some("lum")).unwrap(), lum), "{mean:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_mean_colour_comes_back_as_a_colour_swatch() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("mean_swatch");
    let ops = [
        create("/k", "tex.constant", &[("color", Value::Vec4([0.25, 0.5, 0.75, 1.0]))]),
        create("/mean", "chan.from_tex", &[]),
        connect("/k", "/mean", 0),
        create("/swatch", "tex.from_chan", &[]),
        connect("/mean", "/swatch", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    chans_of(&mut rig, "/mean");
    let px = rig.pixels("/swatch");
    assert_eq!((px.width, px.height), (1, 1));
    let [r, g, b, a] = px.get(0, 0).unwrap();
    let near = |x: f32, y: f32| (x - y).abs() < 0.01;
    assert!(near(r, 0.25) && near(g, 0.5) && near(b, 0.75) && near(a, 1.0), "{:?}", px.get(0, 0));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn channels_become_a_picture_and_back() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("from_chan");
    let ops = [
        create(
            "/k",
            "chan.constant",
            &[
                ("rows", Value::Int(2)),
                ("value0", Value::Float(0.5)),
                ("name1", "b".into()),
                ("value1", Value::Float(3.0)),
            ],
        ),
        create("/t", "tex.from_chan", &[("high", Value::Float(2.0))]),
        connect("/k", "/t", 0),
        // Round trip: the u ramp read as 8 samples of red, drawn as a row again.
        uv_picture(&dir),
        create("/c", "chan.from_tex", &[("columns", Value::Int(8)), ("components", "red".into())]),
        connect("/uv", "/c", 0),
        create("/back", "tex.from_chan", &[]),
        connect("/c", "/back", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let px = rig.pixels("/t");
    assert_eq!((px.width, px.height), (1, 2));
    assert_eq!(px.get(0, 0), Some([0.25, 0.25, 0.25, 1.0]));
    assert_eq!(px.get(0, 1), Some([1.5, 1.5, 1.5, 1.0]));
    chans_of(&mut rig, "/c");
    let px = rig.pixels("/back");
    assert_eq!((px.width, px.height), (8, 1));
    for x in 0..8 {
        let [r, g, b, a] = px.get(x, 0).unwrap();
        let want = (x as f32 + 0.5) / 8.0;
        assert!((r - want).abs() < 0.01 && r == g && g == b && a == 1.0, "pixel {x}: {r}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Whether the level meter's peak line (the UI's text grey) crosses the level well near 90 %
/// of its height (y about 18), in a 320 x 180 picture with a spectrum, laid out at scale 1.
fn peak_line_high(px: &Pixels) -> bool {
    (14..23).any(|y| px.get(19, y).is_some_and(|p| p[0] > 0.5 && (p[0] - p[1]).abs() < 0.05))
}

/// An audio input fed by the test, into an analysis at 320 x 180 whose levels are the input's
/// own (no normalising), so a block of 0.9 reads as level 0.9.
fn analysis_rig(gpu: Gpu, more: &[Op]) -> Rig {
    let mut ops = vec![
        create("/in", "chan.audio_in", &[("device", "test".into())]),
        create(
            "/a",
            "chan.audio_analysis",
            &[("width", Value::Int(320)), ("height", Value::Int(180)), ("normalize", Value::Bool(false))],
        ),
        connect("/in", "/a", 0),
    ];
    ops.extend_from_slice(more);
    Rig::new(gpu, None, &ops)
}

/// The analysis' meters output with `level` fed and shown (as a viewer shows it), once it has
/// a picture whose text is set (the glyph jobs done).
fn meters_shown(rig: &mut Rig, level: f32) -> Tex {
    let a = rig.id("/a");
    let meters = Port { node: a, output: 2 };
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut last = None;
    loop {
        rig.audio.feed("test", 48_000, 2, &[level; 1600]).unwrap();
        rig.clock.advance(1);
        rig.cooker.cook_showing(&rig.net, rig.clock.now(), &[], &[], &[meters]);
        let s = rig.cooker.status(a).unwrap();
        assert_eq!(s.error, None);
        let idle = !matches!(
            s.job,
            Some(penumbra_graph::JobStatus::Queued | penumbra_graph::JobStatus::Running { .. })
        );
        if let Some(t) = rig.cooker.output(meters).and_then(|d| d.get::<Tex>()).cloned() {
            if idle && last.is_some() {
                return t;
            }
            last = Some(t);
        }
        assert!(Instant::now() < deadline, "no meters: {s:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn analysis_meters_are_drawn_only_while_read() {
    let Some(gpu) = gpu() else { return };
    let mut rig = analysis_rig(
        gpu,
        &[create("/sink", "chan.null", &[]), connect("/a", "/sink", 0), create("/view", "tex.null", &[])],
    );
    let a = rig.id("/a");
    let meters = |rig: &Rig| rig.cooker.output(Port { node: a, output: 2 }).cloned();
    // Only the channels are read: no picture, not even the drawing state.
    for _ in 0..3 {
        rig.audio.feed("test", 48_000, 2, &[0.9; 1600]).unwrap();
        rig.clock.advance(1);
        rig.cook("/sink");
    }
    let m = meters(&rig).expect("an output in the meters slot");
    assert!(m.is_skipped() && m.get::<Tex>().is_none());
    let s = rig.cooker.status(a).unwrap();
    assert_eq!((s.error, s.warning), (None, None));

    // Wired into a node that cooks: drawn, with nothing new from the audio.
    rig.edit(&[Op::Connect {
        from: "/a".parse().unwrap(),
        output: 2,
        to: "/view".parse().unwrap(),
        input: 0,
    }]);
    rig.cook("/view");
    let tex = rig.tex("/view").expect("the meters through tex.null");
    assert_eq!(tex.size(), (320, 180));
    let s = rig.cooker.status(a).unwrap();
    assert_eq!((s.error, s.warning), (None, None));
    assert!(!s.time_dependent, "the analysis follows the audio, not the clock");

    // tex.null no longer cooks: skipped again.
    rig.audio.feed("test", 48_000, 2, &[0.9; 1600]).unwrap();
    rig.cook("/sink");
    assert!(meters(&rig).unwrap().is_skipped());
}

#[test]
fn analysis_meters_show_the_levels_on_transparent_black_and_start_over_on_reset() {
    let Some(gpu) = gpu() else { return };
    let mut rig = analysis_rig(gpu, &[]);
    let tex = meters_shown(&mut rig, 0.9);
    assert_eq!(tex.size(), (320, 180));
    let px = rig.gpu.read(&tex).unwrap();
    // The level bar: green, from the bottom of its well up to 90 %.
    let [r, g, b, a] = px.get(19, 80).unwrap();
    assert!(g > r && g > b && a > 0.9, "level green: {:?}", [r, g, b, a]);
    assert!(peak_line_high(&px), "the peak sits on top of a loud level");
    // Nothing behind the meters: the corners are transparent.
    for (x, y) in [(0, 0), (319, 0), (0, 179), (319, 179)] {
        assert_eq!(px.get(x, y).map(|p| p[3]), Some(0.0), "corner {x}, {y}");
    }
    let s = rig.cooker.status(rig.id("/a")).unwrap();
    assert_eq!((s.error, s.warning), (None, None));

    // Quiet now: the peak holds.
    let tex = meters_shown(&mut rig, 0.0);
    assert!(peak_line_high(&rig.gpu.read(&tex).unwrap()), "held");

    // Reset: the memory is gone, the peak is where the level is.
    assert_eq!(rig.cooker.reset(rig.id("/a")), penumbra_graph::ResetOutcome::Reset);
    let tex = meters_shown(&mut rig, 0.0);
    assert!(!peak_line_high(&rig.gpu.read(&tex).unwrap()), "reset forgets the peak");

    // The size follows the params.
    rig.edit(&[set("/a.width", Value::Int(200)), set("/a.height", Value::Int(100))]);
    assert_eq!(meters_shown(&mut rig, 0.0).size(), (200, 100));
}

/// Waits until `node`'s recording has finished its file.
fn recorded(rig: &Rig, node: NodeId) -> penumbra_record::Status {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let s = rig.record.status(node).expect("a recording");
        if s.done {
            return s;
        }
        assert!(Instant::now() < deadline, "the recording did not finish: {s:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Mean 8-bit luma of the left and the right quarter of a decoded frame.
fn luma_sides(frame: &penumbra_video::Frame) -> (f64, f64) {
    let px = frame.pixels().unwrap();
    let w = px.width as usize;
    let (plane, scale): (Vec<f64>, f64) = match &px.data {
        penumbra_video::PixelData::Yuv8 { planes, .. } => {
            (planes[0].iter().map(|&v| f64::from(v)).collect(), 1.0)
        }
        penumbra_video::PixelData::Yuv16 { planes, bits, .. } => {
            (planes[0].iter().map(|&v| f64::from(v)).collect(), f64::from(1u32 << (bits - 8)))
        }
        other => panic!("unexpected pixels {other:?}"),
    };
    let side = |left: bool| {
        let vals: Vec<f64> = plane
            .chunks(w)
            .flat_map(|row| if left { &row[..w / 4] } else { &row[w - w / 4..] })
            .copied()
            .collect();
        vals.iter().sum::<f64>() / vals.len() as f64 / scale
    };
    (side(true), side(false))
}

#[test]
fn out_video_records_what_it_is_shown() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("video_out");
    for (codec, file) in [("h264", "ramp.mp4"), ("prores_4444", "ramp")] {
        let mut rig = Rig::new(
            gpu.clone(),
            Some(&dir),
            &[
                create("/ramp", "tex.ramp", &size()),
                create(
                    "/rec",
                    "out.video",
                    &[("codec", codec.into()), ("file", file.into()), ("rate", Value::Float(30.0))],
                ),
                connect("/ramp", "/rec", 0),
            ],
        );
        rig.clock.play();
        let rec = rig.id("/rec");
        rig.cook("/rec");
        assert_eq!(rig.record.status(rec), None, "not recording before `record` turns on");
        rig.edit(&[set("/rec.record", Value::Bool(true))]);
        // 20 show frames at 60 Hz: 10 frames at 30.
        for _ in 0..20 {
            rig.cook("/rec");
            rig.clock.advance(1);
            std::thread::sleep(Duration::from_millis(5));
        }
        rig.edit(&[set("/rec.record", Value::Bool(false))]);
        rig.cook("/rec");
        let s = recorded(&rig, rec);
        assert_eq!(s.error, None, "{codec}");
        assert_eq!((s.frames, s.dropped, s.skipped), (10, 0, 0), "{codec}: {s:?}");
        let path = PathBuf::from(&s.file);
        let ext = if codec == "h264" { "mp4" } else { "mov" };
        assert_eq!(path, dir.join(format!("ramp.{ext}")), "an extension is added when missing");
        let mut d = penumbra_video::open(&path, &penumbra_video::Limits::default()).unwrap();
        let info = d.info().clone();
        assert_eq!((info.width, info.height, info.frames), (W as u32, H as u32, 10), "{codec}: {info:?}");
        assert!((info.rate() - 30.0).abs() < 1e-6);
        // The ramp runs from black to white in linear light: sRGB-encoded, the left quarter
        // averages about 0.39 (luma 101), the right one about 0.94 (222).
        let (left, right) = luma_sides(&d.frame(5).unwrap());
        assert!(
            (left - 101.0).abs() < 8.0 && (right - 222.0).abs() < 5.0,
            "{codec}: left {left}, right {right}"
        );
        // Read back through tex.video, the recording is the ramp again.
        let mut rig2 = Rig::new(
            gpu.clone(),
            Some(&dir),
            &[create("/play", "tex.video", &[("file", s.file.clone().into()), ("play", Value::Bool(false))])],
        );
        let back = rig2.pixels("/play");
        let want = rig.pixels("/ramp");
        let worst = back
            .data
            .iter()
            .zip(&want.data)
            .map(|(a, b)| (0..3).map(|c| (a[c] - b[c]).abs()).fold(0.0f32, f32::max))
            .fold(0.0f32, f32::max);
        assert!(worst < 0.03, "{codec}: off by up to {worst} (linear)");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn out_video_stops_harmlessly_and_reports_bad_paths() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("video_out_errors");
    std::fs::write(dir.join("blocker"), b"a file, not a folder").unwrap();
    let mut rig = Rig::new(
        gpu,
        Some(&dir),
        &[
            create("/noise", "tex.noise", &size()),
            create("/rec", "out.video", &[("file", "blocker/x.mp4".into())]),
            connect("/noise", "/rec", 0),
        ],
    );
    rig.clock.play();
    let rec = rig.id("/rec");
    rig.cook("/rec");
    let record = |rig: &mut Rig, on: bool| {
        rig.edit(&[set("/rec.record", Value::Bool(on))]);
        rig.clock.advance(1);
        rig.cook("/rec");
    };
    // Stopping what never started does nothing.
    record(&mut rig, false);
    assert_eq!(rig.record.status(rec), None);
    assert!(rig.cooker.status(rec).unwrap().error.is_none());
    // A file below a file cannot be created: the node shows why while `record` stays on, and
    // does not try again by itself.
    record(&mut rig, true);
    rig.clock.advance(1);
    rig.cook("/rec");
    let error = rig.cooker.status(rec).unwrap().error.clone().unwrap_or_default();
    assert!(error.contains("cannot create"), "{error}");
    let tries = rig.record.status(rec).map(|s| s.file);
    rig.clock.advance(1);
    rig.cook("/rec");
    assert_eq!(rig.record.status(rec).map(|s| s.file), tries, "not tried again");
    // Odd sizes are rounded down, too large ones refused; off and on tries again.
    record(&mut rig, false);
    assert!(rig.cooker.status(rec).unwrap().error.is_none(), "off clears the error");
    rig.edit(&[
        set("/rec.file", "ok.mov".into()),
        set("/rec.codec", "prores_422".into()),
        set("/rec.size", "fixed".into()),
        set("/rec.width", Value::Int(33)),
        set("/rec.height", Value::Int(17)),
    ]);
    record(&mut rig, true);
    for _ in 0..3 {
        rig.clock.advance(1);
        rig.cook("/rec");
    }
    record(&mut rig, false);
    let s = recorded(&rig, rec);
    assert_eq!((s.width, s.height, s.error.clone()), (32, 16, None), "{s:?}");
    rig.edit(&[
        set("/rec.codec", "prores_4444".into()),
        set("/rec.width", Value::Int(8192)),
        set("/rec.height", Value::Int(8192)),
    ]);
    record(&mut rig, true);
    let error = rig.cooker.status(rec).unwrap().error.clone().unwrap_or_default();
    assert!(error.contains("too large"), "{error}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `out.spout` to `tex.spout_in` in one process, through the same shared texture another
/// program would open: 8 bits straight (rounded to an sRGB step) and 16-bit float.
#[cfg(windows)]
#[test]
fn spout_round_trip() {
    let Some(gpu) = gpu() else { return };
    let name = format!("penumbra test round trip {}", std::process::id());
    let color = |c: [f64; 4]| ("color", Value::Vec4(c));
    let mut rig = Rig::new(
        gpu,
        None,
        &[
            create(
                "/c",
                "tex.constant",
                &[("width", Value::Int(8)), ("height", Value::Int(4)), color([1.0, 0.5, 0.25, 0.5])],
            ),
            create("/out", "out.spout", &[("name", name.as_str().into())]),
            connect("/c", "/out", 0),
            create("/in", "tex.spout_in", &[("sender", name.as_str().into())]),
        ],
    );
    rig.cook("/out");
    let px = rig.pixels("/in");
    assert_eq!((px.width, px.height), (8, 4));
    let want = [0.5, 0.25, 0.125, 0.5];
    for p in &px.data {
        assert!(p.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.01), "{p:?}, expected {want:?}");
    }

    // A new picture arrives as a new frame (the sender counts frames).
    let before = rig.version("/in");
    rig.edit(&[set("/c.color", Value::Vec4([0.0, 1.0, 0.0, 1.0])), set("/out.format", "16bit_float".into())]);
    rig.cook("/out");
    let tex = rig.cook_until_new("/in", before);
    let px = rig.gpu.read(&tex).unwrap();
    for p in &px.data {
        assert!(p.iter().zip([0.0, 1.0, 0.0, 1.0]).all(|(a, b)| (a - b).abs() < 3e-3), "{p:?}");
    }
    let status = rig.cooker.status(rig.id("/out")).unwrap();
    assert_eq!((status.error, status.warning), (None, None));
}

/// `out.ndi` to `tex.ndi_in` in one process through the installed NDI runtime: straight alpha
/// both ways, so colour and transparency come back.
#[test]
#[ignore = "needs the NDI runtime and the local network"]
fn ndi_round_trip() {
    let Some(gpu) = gpu() else { return };
    let name = format!("penumbra test round trip {}", std::process::id());
    let color = |c: [f64; 4]| set("/c.color", Value::Vec4(c));
    let mut rig = Rig::new(
        gpu,
        None,
        &[
            create("/c", "tex.constant", &[("width", Value::Int(64)), ("height", Value::Int(32))]),
            create("/out", "out.ndi", &[("name", name.as_str().into()), ("alpha", "straight".into())]),
            connect("/c", "/out", 0),
            create("/in", "tex.ndi_in", &[("source", name.as_str().into())]),
        ],
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut i = 0u32;
    loop {
        // A slightly different picture every frame, so the sender keeps sending.
        i += 1;
        rig.edit(&[color([1.0, 0.5, 0.25, if i.is_multiple_of(2) { 0.5 } else { 0.501 }])]);
        rig.cook("/out");
        rig.gpu.poll();
        rig.cook("/in");
        if rig.tex("/in").is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "nothing received: {:?}", rig.cooker.status(rig.id("/in")));
        std::thread::sleep(Duration::from_millis(16));
    }
    let px = rig.gpu.read(&rig.tex("/in").unwrap()).unwrap();
    assert_eq!((px.width, px.height), (64, 32));
    let p = px.get(31, 15).unwrap();
    let want = [0.5, 0.25, 0.125, 0.5];
    assert!(p.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.02), "{p:?}, expected {want:?}");
    let status = rig.cooker.status(rig.id("/out")).unwrap();
    assert_eq!((status.error, status.warning), (None, None));
}

#[test]
fn scan_pattern_matches_the_decoders_patterns_pixel_for_pixel() {
    let Some(gpu) = gpu() else { return };
    // An odd size, so the codes run past the edges: 6 column bits, 5 row bits, 24 pictures.
    let (w, h) = (37u32, 21u32);
    let patterns = penumbra_calib::scan::Patterns::new(w, h).unwrap();
    assert_eq!(patterns.count(), 24);
    let mut rig = Rig::new(
        gpu,
        None,
        &[create(
            "/p",
            "tex.scan_pattern",
            &[("width", Value::Int(w.into())), ("height", Value::Int(h.into()))],
        )],
    );
    for k in 0..=patterns.count() {
        if k > 0 {
            rig.edit(&[set("/p.pattern", Value::Int(k as i64))]);
        }
        let after = if k == 0 { 0 } else { rig.version("/p") };
        let tex = rig.cook_until_new("/p", after);
        let px = rig.gpu.read(&tex).unwrap();
        for y in 0..h {
            for x in 0..w {
                let want = patterns.get(k).is_some_and(|p| patterns.lit(p, x, y));
                let got = px.get(x, y).unwrap();
                assert_eq!(
                    got,
                    if want { [1.0; 4] } else { [0.0, 0.0, 0.0, 1.0] },
                    "picture {k} at {x}, {y}"
                );
            }
        }
    }
}
