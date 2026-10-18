//! `out.ndi`: offers its input on the network as an NDI® source ([`penumbra_ndi`]); OBS,
//! vMix, Resolume, TouchDesigner, NDI Studio Monitor and other programs list it by name. NDI® is
//! a registered trademark of Vizrt NDI AB (<https://ndi.video/>).
//!
//! The node sends a frame whenever its input changes, announced at the show rate; receivers
//! keep the last one meanwhile. The conversion runs on the GPU and the sending on a thread of
//! its own; a frame that finds the readback ring full is left out. Needs the NDI runtime (NDI
//! Tools or the NDI Runtime installer); without it the node fails with where to get it.

use penumbra_core::{Error, ParamSpec, Value};
use penumbra_gpu::{Alpha, Gpu};
use penumbra_graph::{CookContext, CookError, Cooked, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator};
use penumbra_ndi::Sender;

use crate::out::spout::alpha_param;

/// The node kind.
pub const NDI_OUT: &str = "out.ndi";

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    // Alpha doubles the sending CPU and adds about 10 ms (measured at 1080p60), and most
    // receivers ignore it, so this node sends none unless asked.
    let mut alpha = alpha_param(
        "How transparency travels to the receivers. With alpha, NDI sends an extra plane, which \
         costs about twice the CPU and some latency",
    );
    alpha.default = Value::Text("opaque".into());
    types.register(FnNodeType::new(
        NodeDesc::new(
            NDI_OUT,
            "Offers its input on the network as an NDI\u{ae} source (needs the NDI runtime, ndi.video)",
        )
        .param(ParamSpec::text("name", "").doc(
            "The source name receivers pick; NDI puts the machine's name before it, as in `PC (name)`. \
             Empty: `penumbra` and the node's name, such as `penumbra ndi1`",
        ))
        .param(alpha)
        .input("in", DataKind::Tex)
        .describe_input("in", "The picture to send"),
        || Box::new(NdiOut::default()) as Box<dyn Operator>,
    ))
}

#[derive(Default)]
struct NdiOut {
    sender: Option<Sender>,
    /// The name the sender was made for.
    wanted: String,
    /// Why making a sender failed: not tried again until a param changes.
    failed: Option<String>,
}

impl Operator for NdiOut {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = cx.resource::<Gpu>().ok_or_else(|| CookError::failed("no GPU"))?;
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
            match Sender::new(&wanted) {
                Ok(s) => self.sender = Some(s),
                Err(e) => {
                    self.failed = Some(e.clone());
                    return Err(CookError::failed(e));
                }
            }
        }
        let Some(sender) = self.sender.as_mut() else { return Err(CookError::failed("no sender")) };
        if let Some(e) = sender.error() {
            if sender.name().is_none() {
                // The source could not be announced: the send thread has ended.
                self.sender = None;
                self.failed = Some(e.clone());
                return Err(CookError::failed(e));
            }
            cx.warn(e);
        }
        let Some(tex) = cx.input_as::<penumbra_gpu::Tex>(0) else {
            cx.warn("nothing connected: receivers see the last picture");
            return Ok(Cooked::Outputs(Vec::new()));
        };
        let alpha = Alpha::parse(cx.text("alpha")?).unwrap_or(Alpha::Opaque);
        let rate = cx.rate();
        sender.send(gpu, tex, alpha, (rate.num(), rate.den())).map_err(CookError::failed)?;
        Ok(Cooked::Outputs(Vec::new()))
    }
}
