//! Layered pictures into textures: Photoshop files (PSD, PSB) through `photocraft-psd`, and layer
//! directories (a directory of image files, one layer each, bottom first by name).
//!
//! A [`Source`] is opened once (the PSD parsed, channel data still compressed; the directory
//! listed); single layers are decoded on demand. Pixels come out as every texture: linear,
//! premultiplied half floats. 8- and 16-bit PSD samples are taken as sRGB-encoded, 32-bit as
//! linear (Photoshop's convention); all go through 8 bits for now. Layer opacity and the user
//! mask go into alpha; blend modes are reported, not applied.
//!
//! Hostile files: the canvas and every layer are checked against [`MAX_PIXELS`] and the device's
//! texture size before anything is decoded; `photocraft-psd` checks its own lengths.

use std::path::{Path, PathBuf};

use half::f16;
use penumbra_core::Error;
use photocraft_psd::PsdFile;
use photocraft_psd::tree::LayerNode;

use crate::image::{Decoded, MAX_FILE_BYTES, MAX_PIXELS};
use crate::readback::srgb_to_linear;

/// Image files a layer directory reads (as `tex.image`).
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "tif", "tiff", "exr", "hdr", "webp", "bmp", "tga", "gif", "qoi", "pfm", "ppm",
    "pgm",
];
/// Most layers listed.
pub const MAX_LAYERS: usize = 4096;
/// Most decoded bytes (half floats) a directory's merged picture may hold while compositing.
pub const MAX_MERGE_BYTES: u64 = 1 << 30;

/// One layer as the `layers` table lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerInfo {
    /// Position in the list: bottom first.
    pub index: usize,
    pub name: String,
    /// Group names and the layer's, joined with `/`.
    pub path: String,
    pub group: bool,
    pub visible: bool,
    pub opacity: f32,
    pub blend: String,
    /// Left, top, width, height on the canvas (unknown for directories until decoded: 0).
    pub rect: [i64; 4],
    /// The record index in the PSD, or the file in the directory.
    record: Record,
}

#[derive(Clone, Debug, PartialEq)]
enum Record {
    Psd(usize),
    File(PathBuf),
}

pub enum Source {
    Psd { file: Box<PsdFile>, layers: Vec<LayerInfo> },
    Dir { layers: Vec<LayerInfo> },
}

impl Source {
    /// Opens a `.psd`/`.psb` file or a layer directory.
    pub fn open(path: &Path, max_side: u32) -> Result<Source, Error> {
        let ctx = |e: Error| e.context(path.display());
        if path.is_dir() {
            return open_dir(path).map_err(ctx);
        }
        let len = std::fs::metadata(path).map_err(|e| Error::new(format!("{}: {e}", path.display())))?.len();
        if len > MAX_FILE_BYTES {
            return Err(ctx(Error::new(format!("{len} bytes is too large"))));
        }
        let bytes = std::fs::read(path).map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
        Source::psd(&bytes, max_side).map_err(ctx)
    }

    /// A PSD or PSB from its bytes.
    pub fn psd(bytes: &[u8], max_side: u32) -> Result<Source, Error> {
        let file = PsdFile::from_bytes(bytes).map_err(|e| Error::new(e.to_string()))?;
        let (w, h) = (file.header.width, file.header.height);
        check_size(u64::from(w), u64::from(h), max_side, "the canvas")?;
        if file.layers().len() > MAX_LAYERS {
            return Err(Error::new(format!("more than {MAX_LAYERS} layers")));
        }
        let mut layers = Vec::new();
        list(&file, &file.layer_tree(), "", true, &mut layers);
        for (k, l) in layers.iter_mut().enumerate() {
            l.index = k;
        }
        Ok(Source::Psd { file: Box::new(file), layers })
    }

    pub fn layers(&self) -> &[LayerInfo] {
        match self {
            Source::Psd { layers, .. } | Source::Dir { layers } => layers,
        }
    }

    /// The canvas size (a directory has none until its layers are decoded).
    pub fn canvas(&self) -> Option<(u32, u32)> {
        match self {
            Source::Psd { file, .. } => Some((file.header.width, file.header.height)),
            Source::Dir { .. } => None,
        }
    }

    /// The layer `spec` names: `#3` (its index), a path `Group/Layer`, or a name (the topmost
    /// layer of that name). Groups cannot be shown on their own.
    pub fn find(&self, spec: &str) -> Result<&LayerInfo, Error> {
        let spec = spec.trim();
        let layers = self.layers();
        let found = if let Some(i) = spec.strip_prefix('#').and_then(|n| n.parse::<usize>().ok()) {
            layers.get(i)
        } else {
            layers
                .iter()
                .rev()
                .find(|l| l.path == spec)
                .or_else(|| layers.iter().rev().find(|l| l.name == spec))
        };
        let l = found.ok_or_else(|| {
            let names: Vec<&str> =
                layers.iter().filter(|l| !l.group).take(12).map(|l| l.path.as_str()).collect();
            Error::new(format!("no layer `{spec}`; layers: {}", names.join(", ")))
        })?;
        if l.group {
            return Err(Error::new(format!("`{}` is a group; name one of its layers", l.path)));
        }
        Ok(l)
    }

    /// One layer's pixels: on the canvas where it sits (`placed`, the canvas's size) or only its
    /// own rectangle.
    pub fn layer(&self, layer: &LayerInfo, placed: bool, max_side: u32) -> Result<Decoded, Error> {
        match (&layer.record, self) {
            (Record::Psd(index), Source::Psd { file, .. }) => psd_layer(file, *index, placed, max_side),
            (Record::File(path), Source::Dir { .. }) => crate::image::decode_file(path, max_side),
            _ => Err(Error::new("not a layer of this source")),
        }
    }

    /// The whole picture: a PSD's merged image as saved, a directory's layers over each other.
    pub fn merged(&self, max_side: u32) -> Result<(Decoded, Option<String>), Error> {
        match self {
            Source::Psd { file, .. } => {
                let warning = (file.has_real_merged_data() == Some(false)).then(|| {
                    "the file was saved without a merged picture (Maximize Compatibility off)".to_string()
                });
                let img = file.composite_rgba8().map_err(|e| Error::new(e.to_string()))?;
                Ok((rgba8(&img.data, img.width, img.height, file.header.depth, 1.0, None)?, warning))
            }
            Source::Dir { layers } => {
                let mut acc: Option<Decoded> = None;
                let mut spent = 0u64;
                for l in layers {
                    let Record::File(path) = &l.record else { continue };
                    let d = crate::image::decode_file(path, max_side)?;
                    spent += u64::from(d.width) * u64::from(d.height) * 8;
                    if spent > MAX_MERGE_BYTES {
                        return Err(Error::new("the layers are too large to merge; pick one with `layer`"));
                    }
                    acc = Some(match acc {
                        None => d,
                        Some(below) => over(below, &d),
                    });
                }
                acc.map(|d| (d, None)).ok_or_else(|| Error::new("the directory holds no images"))
            }
        }
    }
}

fn check_size(w: u64, h: u64, max_side: u32, what: &str) -> Result<(), Error> {
    if w == 0 || h == 0 {
        return Err(Error::new(format!("{what} is empty")));
    }
    if w > u64::from(max_side) || h > u64::from(max_side) || w * h > MAX_PIXELS {
        return Err(Error::new(format!(
            "{what} is {w}x{h}, more than {max_side} on a side or {MAX_PIXELS} pixels"
        )));
    }
    Ok(())
}

/// Lists the tree top first (as Photoshop's layers panel), with group paths; `visible` is
/// whether every enclosing group is.
fn list(file: &PsdFile, nodes: &[LayerNode], prefix: &str, visible: bool, out: &mut Vec<LayerInfo>) {
    for node in nodes.iter().rev() {
        let index = node.index();
        let Some(rec) = file.layers().get(index) else { continue };
        let name = rec.name();
        let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        let shown = visible && !rec.flags.hidden();
        let (w, h) = (rec.rect.width(), rec.rect.height());
        out.push(LayerInfo {
            index: 0,
            name,
            path: path.clone(),
            group: node.is_group(),
            visible: shown,
            opacity: f32::from(rec.opacity) / 255.0,
            blend: format!("{:?}", rec.blend_mode).to_lowercase(),
            rect: [i64::from(rec.rect.left), i64::from(rec.rect.top), w, h],
            record: Record::Psd(index),
        });
        if out.len() >= MAX_LAYERS {
            return;
        }
        list(file, node.children(), &path, shown, out);
    }
    // Bottom first, as the rest of penumbra orders layers (and indices count from the bottom).
    if prefix.is_empty() {
        out.reverse();
    }
}

fn open_dir(dir: &Path) -> Result<Source, Error> {
    let entries = std::fs::read_dir(dir).map_err(|e| Error::new(e.to_string()))?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        })
        .take(MAX_LAYERS + 1)
        .collect();
    if files.len() > MAX_LAYERS {
        return Err(Error::new(format!("more than {MAX_LAYERS} images")));
    }
    files.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    let layers = files
        .into_iter()
        .enumerate()
        .map(|(index, path)| {
            let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            LayerInfo {
                index,
                path: name.clone(),
                name,
                group: false,
                visible: true,
                opacity: 1.0,
                blend: "normal".into(),
                rect: [0; 4],
                record: Record::File(path),
            }
        })
        .collect();
    Ok(Source::Dir { layers })
}

fn psd_layer(file: &PsdFile, index: usize, placed: bool, max_side: u32) -> Result<Decoded, Error> {
    let layer = file.layer(index).ok_or_else(|| Error::new("no such layer"))?;
    let rect = layer.record.rect;
    let (lw, lh) = (rect.width(), rect.height());
    if lw <= 0 || lh <= 0 {
        return Err(Error::new(format!(
            "layer `{}` is empty (a shape, text or adjustment layer?)",
            layer.name()
        )));
    }
    check_size(lw as u64, lh as u64, u32::MAX, "the layer")?;
    let img = layer.rgba8().map_err(|e| Error::new(e.to_string()))?;
    let mask = match layer.user_mask() {
        Some(m) if !layer.record.layer_mask().is_some_and(|l| l.disabled()) => {
            Some(m.map_err(|e| Error::new(e.to_string()))?)
        }
        _ => None,
    };
    let opacity = f32::from(layer.record.opacity) / 255.0;
    let depth = file.header.depth;
    let mask_at = |x: i64, y: i64| -> f32 {
        mask.as_ref().map_or(1.0, |m| {
            let (mx, my) = (x - i64::from(m.left), y - i64::from(m.top));
            let v = if mx >= 0 && my >= 0 && mx < i64::from(m.width) && my < i64::from(m.height) {
                m.data.get((my * i64::from(m.width) + mx) as usize).copied().unwrap_or(m.default_value)
            } else {
                m.default_value
            };
            f32::from(v) / 255.0
        })
    };
    let (ox, oy) = (i64::from(img.left), i64::from(img.top));
    if !placed {
        check_size(u64::from(img.width), u64::from(img.height), max_side, "the layer")?;
        return rgba8(
            &img.data,
            img.width,
            img.height,
            depth,
            opacity,
            Some(&|x, y| mask_at(x + ox, y + oy)),
        );
    }
    // On the canvas: the layer's pixels where it overlaps, transparent elsewhere.
    let (cw, ch) = (file.header.width, file.header.height);
    check_size(u64::from(cw), u64::from(ch), max_side, "the canvas")?;
    let mut canvas = vec![0u8; cw as usize * ch as usize * 4];
    let mut masks = vec![0.0f32; cw as usize * ch as usize];
    for y in 0..i64::from(img.height) {
        let cy = y + oy;
        if cy < 0 || cy >= i64::from(ch) {
            continue;
        }
        for x in 0..i64::from(img.width) {
            let cx = x + ox;
            if cx < 0 || cx >= i64::from(cw) {
                continue;
            }
            let src = ((y * i64::from(img.width) + x) * 4) as usize;
            let dst = ((cy * i64::from(cw) + cx) * 4) as usize;
            if let (Some(s), Some(d)) = (img.data.get(src..src + 4), canvas.get_mut(dst..dst + 4)) {
                d.copy_from_slice(s);
            }
            if let Some(m) = masks.get_mut((cy * i64::from(cw) + cx) as usize) {
                *m = mask_at(cx, cy);
            }
        }
    }
    rgba8(
        &canvas,
        cw,
        ch,
        depth,
        opacity,
        Some(&|x, y| masks.get((y * i64::from(cw) + x) as usize).copied().unwrap_or(0.0)),
    )
}

/// Straight 8-bit RGBA into linear premultiplied half floats; `depth` 32 means the samples are
/// linear already. `mask` multiplies alpha per pixel.
fn rgba8(
    data: &[u8],
    w: u32,
    h: u32,
    depth: u16,
    opacity: f32,
    mask: Option<&dyn Fn(i64, i64) -> f32>,
) -> Result<Decoded, Error> {
    let n = w as usize * h as usize;
    if data.len() != n * 4 {
        return Err(Error::new("pixel data of the wrong size"));
    }
    let lut: Vec<f32> = (0..256)
        .map(|v| if depth == 32 { v as f32 / 255.0 } else { srgb_to_linear(v as f32 / 255.0) })
        .collect();
    let lin = |c: u8| lut.get(usize::from(c)).copied().unwrap_or(0.0);
    let mut out = Vec::with_capacity(n * 4);
    for (i, [r, g, b, a]) in data.as_chunks::<4>().0.iter().enumerate() {
        let (x, y) = ((i % w as usize) as i64, (i / w as usize) as i64);
        let alpha = f32::from(*a) / 255.0 * opacity * mask.map_or(1.0, |m| m(x, y));
        out.extend([lin(*r), lin(*g), lin(*b)].map(|c| f16::from_f32(c * alpha)));
        out.push(f16::from_f32(alpha));
    }
    let source = format!("rgba {}-bit (read at 8 bits)", depth);
    Ok(Decoded { width: w, height: h, rgba: out, source })
}

/// `top` over `bottom` (premultiplied), on a canvas of the larger size, both at the top left.
fn over(bottom: Decoded, top: &Decoded) -> Decoded {
    let (w, h) = (bottom.width.max(top.width), bottom.height.max(top.height));
    if (w, h) == (bottom.width, bottom.height) && (w, h) == (top.width, top.height) {
        let mut b = bottom;
        for (d, s) in b.rgba.as_chunks_mut::<4>().0.iter_mut().zip(top.rgba.as_chunks::<4>().0) {
            let ta = s[3].to_f32();
            for (dc, sc) in d.iter_mut().zip(s) {
                *dc = f16::from_f32(sc.to_f32() + dc.to_f32() * (1.0 - ta));
            }
        }
        return b;
    }
    let mut out = vec![f16::ZERO; w as usize * h as usize * 4];
    for img in [&bottom, top] {
        for y in 0..img.height as usize {
            for x in 0..img.width as usize {
                let s = (y * img.width as usize + x) * 4;
                let d = (y * w as usize + x) * 4;
                if let (Some(src), Some(dst)) = (img.rgba.get(s..s + 4), out.get_mut(d..d + 4)) {
                    let ta = src.get(3).map_or(0.0, |a| a.to_f32());
                    for (dc, sc) in dst.iter_mut().zip(src) {
                        *dc = f16::from_f32(sc.to_f32() + dc.to_f32() * (1.0 - ta));
                    }
                }
            }
        }
    }
    Decoded { width: w, height: h, rgba: out, source: bottom.source }
}
