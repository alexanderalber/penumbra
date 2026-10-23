//! `out.spout`: shares its input with other programs on this machine through Spout
//! ([`penumbra_spout`]): TouchDesigner, Resolume, OBS and others list it as a sender by its name.
//!
//! The node sends a frame whenever its input changes; receivers keep the last one meanwhile.
//! The sender is listed from the node's first cook until the node is deleted or its project
//! closed. A name another program already sends under gets a number (`name_1`), as Spout does,
//! and the node warns. Windows only.

use penumbra_core::{Error, ParamSpec};
use penumbra_gpu::Tex;
use penumbra_graph::{CookContext, CookError, Cooked, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator};
use penumbra_spout::{Alpha, Format, Sender, Sent};

/// The node kind.
pub const SPOUT_OUT: &str = "out.spout";

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            SPOUT_OUT,
            "Shares a picture with other programs on this machine through Spout (Windows)",
        )
        .param(ParamSpec::text("name", "").doc(
            "The sender name receivers pick, ASCII only; empty: `penumbra` and the node's name, such as \
                 `penumbra spout1`. A name another program has taken gets a number (`name_1`)",
        ))
        .param(
            ParamSpec::choice("format", &Format::NAMES)
                .doc("The pixel format of the shared texture")
                .choice_doc("8bit", "BGRA, 8 bits per channel: what every Spout program takes")
                .choice_doc(
                    "16bit_float",
                    "RGBA, 16-bit float: finer steps and values above 1, for receivers that take it \
                         (TouchDesigner does)",
                ),
        )
        .param(alpha_param("How transparency travels to the receivers"))
        .input("in", DataKind::Tex)
        .describe_input("in", "The picture to share"),
        || Box::new(SpoutOut::default()) as Box<dyn Operator>,
    ))
}

/// The `alpha` param of both Spout nodes.
pub fn alpha_param(doc: &str) -> ParamSpec {
    ParamSpec::choice("alpha", &Alpha::NAMES)
        .doc(doc)
        .choice_doc("straight", "colour and alpha apart, as most programs take it")
        .choice_doc("premultiplied", "colour multiplied by alpha")
        .choice_doc("opaque", "no transparency: sent over black, or a received alpha ignored")
}

#[derive(Default)]
struct SpoutOut {
    sender: Option<Sender>,
    /// The name the sender was made for.
    wanted: String,
    /// Why making a sender for `wanted` failed: not tried again until a param changes.
    failed: Option<String>,
}

impl Operator for SpoutOut {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = crate::gpu(cx)?;
        let set = cx.text("name")?.trim();
        let wanted =
            if set.is_empty() { format!("penumbra {}", cx.def().name.as_str()) } else { set.to_string() };
        if wanted != self.wanted {
            self.sender = None;
            self.failed = None;
            self.wanted = wanted.clone();
        }
        if cx.params_changed() {
            self.failed = None;
        }
        if let Some(e) = &self.failed {
            return Err(CookError::failed(e.clone()));
        }
        if self.sender.is_none() {
            match Sender::new(gpu, &wanted) {
                Ok(s) => self.sender = Some(s),
                Err(e) => {
                    self.failed = Some(e.clone());
                    return Err(CookError::failed(e));
                }
            }
        }
        let Some(sender) = self.sender.as_mut() else { return Err(CookError::failed("no sender")) };
        if sender.name() != wanted {
            cx.warn(format!("another program sends as `{wanted}`: this one is `{}`", sender.name()));
        }
        let Some(tex) = cx.input_as::<Tex>(0) else {
            cx.warn("nothing connected: receivers see the last picture");
            return Ok(Cooked::Outputs(Vec::new()));
        };
        let format = cx.text("format")?;
        let format =
            Format::parse(format).ok_or_else(|| CookError::failed(format!("unknown format `{format}`")))?;
        let alpha = cx.text("alpha")?;
        let alpha =
            Alpha::parse(alpha).ok_or_else(|| CookError::failed(format!("unknown alpha `{alpha}`")))?;
        match sender.send(gpu, tex, format, alpha) {
            Ok(Sent::Frame | Sent::Busy) => Ok(Cooked::Outputs(Vec::new())),
            Err(e) => Err(CookError::failed(e)),
        }
    }
}
