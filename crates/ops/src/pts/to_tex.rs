//! `tex.from_pts`: one attribute of points as a picture, one texel per point, TouchDesigner's
//! POP to TOP. Points made as a grid keep its rows and columns; others fill rows of a chosen
//! width. Alpha is 1 where a point is and 0 in the texels past the last point, so `tex.shader`,
//! `tex.scatter` and `geo.tex_instance` can tell the two apart.

use penumbra_core::{Error, ParamSpec};
use penumbra_gpu::{BufferRing, Points, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::program::attr_name;
use crate::gpu;

pub const KIND: &str = "tex.from_pts";

/// Choices of `layout`.
pub const LAYOUTS: [&str; 2] = ["auto", "rows"];

/// Widest row the param offers; the device's texture limit caps it further.
const MAX_WIDTH: u32 = 16384;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "One attribute of points as a picture of 32-bit floats, one texel per point: positions, colours or any other attribute for tex.shader, tex.scatter and geo.tex_instance",
        )
        .param(
            ParamSpec::text("attribute", "position")
                .doc("The attribute that fills the picture, TouchDesigner's names too (P, Color, PointScale). One number becomes grey, two red and green, three red, green and blue. Four numbers fill alpha too"),
        )
        .param(
            ParamSpec::choice("layout", &LAYOUTS)
                .doc("Where each point's texel lies")
                .choice_doc("auto", "points made as a grid keep its rows and columns: a grid of 100 rows and 200 columns gives a picture 200 wide and 100 high. Other points fill rows as `rows` does")
                .choice_doc("rows", "point 0 at the top left, then along the row, `width` texels to a row"),
        )
        .param(
            ParamSpec::int("width", 0)
                .clamp(0.0, f64::from(MAX_WIDTH))
                .range(0.0, 4096.0)
                .special(0.0, "square")
                .doc("Texels in a row when the points fill rows. 0 makes the picture about square"),
        )
        .input("in", DataKind::Pts)
        .output("out", DataKind::Tex)
        .describe_output("out", "The picture. Alpha is 1 at each point (a four-number attribute brings its own) and 0 in the texels past the last point"),
        || Box::new(ToTex::default()) as Box<dyn Operator>,
    ))
}

/// The picture size for `count` points: the grid's own where `dims` has one and `grid` is
/// asked, else rows of `width` (about square for 0). At least 1 by 1.
pub fn picture_size(count: u32, dims: Option<[u32; 3]>, grid: bool, width: u32) -> (u32, u32) {
    if grid && let Some([rows, cols, slices]) = dims {
        return (cols.max(1), rows.saturating_mul(slices).max(1));
    }
    let w = if width == 0 { (f64::from(count).sqrt().ceil() as u32).max(1) } else { width };
    (w, count.div_ceil(w).max(1))
}

#[derive(Default)]
struct ToTex {
    textures: TexRing,
    scratch: BufferRing,
}

impl Operator for ToTex {
    fn gpu_bytes(&self) -> u64 {
        self.textures.bytes() + self.scratch.bytes()
    }

    fn trim(&mut self) {
        self.textures.trim();
        self.scratch.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let points: &Points = match cx.input_as::<Points>(0) {
            Some(p) => p,
            None if cx.def().inputs.first().is_some_and(Option::is_some) => return Err(CookError::Pending),
            None => return Err(CookError::failed("connect points to `in`")),
        };
        let name = attr_name(cx, "attribute")?;
        let width = u32::try_from(cx.int("width")?.max(0)).unwrap_or(0);
        let grid = cx.text("layout")? == "auto";
        let count = points.count().at_most();
        let (w, h) = picture_size(count, points.dims(), grid, width);
        let most = gpu.max_texture_size();
        if w > most || h > most {
            return Err(CookError::failed(format!(
                "{count} points need a picture of {w}x{h}; this GPU makes pictures up to {most} on a side. Choose another `width`"
            )));
        }
        let tex = points.attr_picture(gpu, name, (w, h), &mut self.textures, &mut self.scratch)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, tex)]))
    }
}
