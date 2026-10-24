//! `tex.sequence`: the picture of a show. Its `sequence` input takes a `chan.sequence`, whose
//! `from`, `to` and `mix` say which of its inputs are shown: piece k is `in<k>`, black and white
//! are made here unless `black` or `white` are wired. Only the one or two inputs shown cook (the
//! choice reads the channels before anything upstream of the pictures cooks); one input alone
//! is passed on without a copy.

use std::sync::Arc;

use penumbra_core::{Chans, Error, ParamSpec, Value};
use penumbra_gpu::{Pass, PassInput, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, InputChoice, NodeDesc, NodeTypes, Operator,
};

use super::gpu;
use crate::sequence::{Entry, shown};

/// Picture inputs of a `tex.sequence`, one per piece.
pub const SEQUENCE_PIECES: usize = 24;
/// The inputs before the pieces: the channels, black, white.
const SEQ: usize = 0;
const BLACK: usize = 1;
const WHITE: usize = 2;
const FIRST: usize = 3;
/// Black and white as made here, premultiplied.
const BLACK_RGBA: [f64; 4] = [0.0, 0.0, 0.0, 1.0];
const WHITE_RGBA: [f64; 4] = [1.0, 1.0, 1.0, 1.0];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let mut desc = NodeDesc::new(
        "tex.sequence",
        "The picture of a timed show: plays the pieces a chan.sequence lists, with cuts and fades. Only the piece on screen runs",
    )
    .param(
        ParamSpec::int("width", 1920)
            .clamp(1.0, 16384.0)
            .range(64.0, 3840.0)
            .more()
            .doc("Width of the black or white made while no picture is shown to take the size from"),
    )
    .param(
        ParamSpec::int("height", 1080)
            .clamp(1.0, 16384.0)
            .range(64.0, 2160.0)
            .more()
            .doc("Height of the black or white made while no picture is shown to take the size from"),
    )
    .input_steering("sequence")
    .input("black", DataKind::Tex)
    .input("white", DataKind::Tex);
    for k in 0..SEQUENCE_PIECES {
        desc = desc.input(&format!("in{k}"), DataKind::Tex);
    }
    let desc = desc.output("out", DataKind::Tex).describe_input(
        "sequence",
        "A chan.sequence: its from, to and mix pick the inputs (piece k is in<k>, -1 black, -2 white)",
    );
    types.register(
        FnNodeType::new(desc, || {
            Box::new(SequencePicture { ring: TexRing::new(), pass: None, still: None }) as Box<dyn Operator>
        })
        .choose_inputs(choose),
    )
}

/// The input that shows `entry`, if it is wired; `None` for black or white made here (also for a
/// piece whose input is not wired: it shows as black).
fn input_of(entry: Entry, connected: impl Fn(usize) -> bool) -> Option<usize> {
    let i = match entry {
        Entry::Black => BLACK,
        Entry::White => WHITE,
        Entry::Piece(k) if k < SEQUENCE_PIECES => FIRST + k,
        Entry::Piece(_) => return None,
    };
    connected(i).then_some(i)
}

fn choose(cx: &InputChoice<'_>) -> Option<Vec<usize>> {
    let (from, to, mix) = shown(cx.chans(SEQ));
    let mut picks = vec![SEQ];
    picks.extend(input_of(from, |i| cx.connected(i)));
    if mix > 0.0 {
        picks.extend(input_of(to, |i| cx.connected(i)));
    }
    Some(picks)
}

/// One side of what is shown: an input's picture or a colour made here.
enum Side<'a> {
    Picture(&'a Tex, &'a Data),
    Color([f64; 4]),
}

struct SequencePicture {
    ring: TexRing,
    pass: Option<Arc<Pass>>,
    /// The colour and size of the plain black or white made last, while it still stands.
    still: Option<([u64; 4], u32, u32)>,
}

impl SequencePicture {
    fn pass(&mut self, cx: &CookContext<'_>) -> Result<Arc<Pass>, CookError> {
        if let Some(p) = &self.pass {
            return Ok(p.clone());
        }
        let params = [
            ParamSpec::float("amount", 0.0),
            ParamSpec::color("color_a", BLACK_RGBA),
            ParamSpec::color("color_b", BLACK_RGBA),
            ParamSpec::float("has_a", 0.0),
            ParamSpec::float("has_b", 0.0),
        ];
        let p = gpu(cx)?.cached_pass("tex.sequence", include_str!("sequence.wgsl"), &params)?;
        self.pass = Some(p.clone());
        Ok(p)
    }

    /// Renders `a` mixed into `b` by `amount`.
    fn render(
        &mut self,
        cx: &CookContext<'_>,
        a: &Side<'_>,
        b: &Side<'_>,
        amount: f64,
    ) -> Result<Tex, CookError> {
        let pass = self.pass(cx)?;
        let gpu = gpu(cx)?;
        let picture = |s: &Side<'_>| match s {
            Side::Picture(t, _) => Some((*t).clone()),
            Side::Color(_) => None,
        };
        let color = |s: &Side<'_>| match s {
            Side::Picture(..) => BLACK_RGBA,
            Side::Color(c) => *c,
        };
        let (ta, tb) = (picture(a), picture(b));
        let (w, h) = match ta.as_ref().or(tb.as_ref()) {
            Some(t) => t.size(),
            None => size(cx)?,
        };
        let (ca, cb) = (color(a), color(b));
        let (has_a, has_b) = (f64::from(u8::from(ta.is_some())), f64::from(u8::from(tb.is_some())));
        let target = self.ring.next(gpu, w, h, "tex.sequence")?;
        let param = move |name: &str| match name {
            "amount" => Some(Value::Float(amount)),
            "color_a" => Some(Value::Vec4(ca)),
            "color_b" => Some(Value::Vec4(cb)),
            "has_a" => Some(Value::Float(has_a)),
            "has_b" => Some(Value::Float(has_b)),
            _ => None,
        };
        pass.render(
            gpu,
            &target,
            &PassInput { inputs: &[ta.as_ref(), tb.as_ref()], seconds: 0.0, frame: 0, param: &param },
        )?;
        Ok(target)
    }
}

/// The size of black or white made while no picture gives one.
fn size(cx: &CookContext<'_>) -> Result<(u32, u32), CookError> {
    let dim = |name: &str| -> Result<u32, CookError> {
        Ok(u32::try_from(cx.int(name)?.clamp(1, 16384)).unwrap_or(1))
    };
    Ok((dim("width")?, dim("height")?))
}

fn tex_out(target: Tex) -> Cooked {
    Cooked::Outputs(vec![Data::new(DataKind::Tex, target)])
}

impl Operator for SequencePicture {
    fn trim(&mut self) {
        self.ring.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        if cx.def().inputs.get(SEQ).is_none_or(Option::is_none) {
            return Err(CookError::failed("connect a chan.sequence to `sequence`"));
        }
        let Some(chans) = cx.input_as::<Chans>(SEQ) else { return Err(CookError::Pending) };
        let (from, to, mix) = shown(Some(chans));
        let wired = |i: usize| cx.def().inputs.get(i).is_some_and(Option::is_some);
        let side = |e: Entry| -> Result<Side<'_>, CookError> {
            match input_of(e, wired) {
                Some(i) => {
                    let data = cx.input(i).ok_or(CookError::Pending)?;
                    let tex = data.get::<Tex>().ok_or_else(|| CookError::failed("not a texture"))?;
                    Ok(Side::Picture(tex, data))
                }
                None => Ok(Side::Color(if e == Entry::White { WHITE_RGBA } else { BLACK_RGBA })),
            }
        };
        let a = side(from)?;
        if mix <= 0.0 {
            return match a {
                Side::Picture(_, data) => {
                    self.still = None;
                    Ok(Cooked::Outputs(vec![data.clone()]))
                }
                Side::Color(c) => {
                    // Black or white alone stands until it changes: not made again each frame.
                    let (w, h) = size(cx)?;
                    let key = (c.map(f64::to_bits), w, h);
                    if self.still == Some(key) {
                        return Ok(Cooked::Unchanged);
                    }
                    let target = self.render(cx, &a, &a, 0.0)?;
                    self.still = Some(key);
                    Ok(tex_out(target))
                }
            };
        }
        let b = side(to)?;
        self.still = None;
        if let (Side::Picture(_, x), Side::Picture(_, y)) = (&a, &b)
            && x.same(y)
        {
            return Ok(Cooked::Outputs(vec![(*x).clone()]));
        }
        let target = self.render(cx, &a, &b, mix)?;
        Ok(tex_out(target))
    }
}
