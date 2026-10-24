//! `tex.image_file`: an image file, decoded and uploaded off the cook thread, reloaded when the file
//! changes on disk.

use penumbra_core::{Error, ParamSpec};
use penumbra_gpu::Tex;
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDesc,
    NodeTypes, Operator,
};

/// Extensions the file picker offers; decoding goes by content, not by name.
const EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "tif", "tiff", "exr", "hdr", "webp", "bmp", "tga", "gif", "qoi", "pfm", "ppm",
    "pgm",
];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            "tex.image_file",
            "A picture from a file (PNG, JPEG, EXR, ...); saving the file again updates it",
        )
        .param(
            ParamSpec::file("file", EXTENSIONS)
                .doc("The picture file, relative to the project directory; saving it again reloads the node"),
        )
        .output("out", DataKind::Tex),
        || Box::new(Image::default()) as Box<dyn Operator>,
    ))
}

#[derive(Default)]
struct Image {
    slot: AsyncSlot<Tex>,
    watch: Option<FileWatch>,
}

impl Operator for Image {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let fresh = self.slot.poll();
        // Before anything is remembered: a cook before the GPU arrives must not count as the
        // load started, or the file never loads once it is there.
        let gpu = super::gpu(cx)?;
        let path = cx.resolve_path(cx.text("file")?)?;
        let moved = self.watch.as_ref().is_none_or(|w| w.path() != path);
        if moved || self.watch.as_ref().is_some_and(FileWatch::take_changed) {
            if moved {
                self.watch = Some(cx.watch_file(&path));
            }
            let gpu = gpu.clone();
            let label = path.display().to_string();
            let p = path.clone();
            self.slot.start(cx.spawn(move |_job| {
                let decoded =
                    penumbra_gpu::image::decode_file(&p, gpu.max_texture_size()).map_err(|e| e.0)?;
                gpu.upload(&decoded, &label).map_err(|e| e.0)
            }));
        }
        if let Some(e) = self.slot.error() {
            return Err(CookError::failed(e));
        }
        match (fresh, self.slot.latest()) {
            (true, Some(tex)) => Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, tex.clone())])),
            (_, None) => Err(CookError::Pending),
            (false, Some(_)) => Ok(Cooked::Unchanged),
        }
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.slot.status())
    }
}
