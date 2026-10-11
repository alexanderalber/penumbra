//! Binary pictures: morphology with square windows and connected regions.

/// One byte per pixel, 0 or 1, rows top to bottom.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mask {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl Mask {
    pub fn empty(width: usize, height: usize) -> Self {
        Mask { width, height, data: vec![0; width.saturating_mul(height)] }
    }

    pub fn get(&self, x: usize, y: usize) -> bool {
        x < self.width
            && y.checked_mul(self.width)
                .and_then(|i| i.checked_add(x))
                .and_then(|i| self.data.get(i))
                .is_some_and(|v| *v != 0)
    }

    /// Pixels set.
    pub fn count(&self) -> usize {
        self.data.iter().filter(|v| **v != 0).count()
    }

    /// Erosion, then dilation, by a square `2 * radius + 1` wide: removes regions narrower than
    /// the square and keeps the shape of the rest.
    pub fn open(&mut self, radius: usize) {
        if radius > 0 {
            self.erode(radius);
            self.dilate(radius);
        }
    }

    /// Dilation, then erosion: fills holes and gaps narrower than the square. Done on a copy
    /// with an empty margin of `radius`, so a region near the edge does not grow into it (the
    /// erosion keeps whatever touches the edge).
    pub fn close(&mut self, radius: usize) {
        let (w, h) = (self.width, self.height);
        if radius == 0 || w == 0 || h == 0 || self.data.len() != w * h {
            return;
        }
        let pw = w + 2 * radius;
        let mut padded = Mask::empty(pw, h + 2 * radius);
        for (src, dst) in self.data.chunks_exact(w).zip(padded.data.chunks_exact_mut(pw).skip(radius)) {
            if let Some(d) = dst.get_mut(radius..radius + w) {
                d.copy_from_slice(src);
            }
        }
        padded.dilate(radius);
        padded.erode(radius);
        for (dst, src) in self.data.chunks_exact_mut(w).zip(padded.data.chunks_exact(pw).skip(radius)) {
            if let Some(s) = src.get(radius..radius + w) {
                dst.copy_from_slice(s);
            }
        }
    }

    /// A pixel stays set when its whole square is set. Beyond the edges counts as set, so a
    /// region touching the border does not shrink away from it.
    pub fn erode(&mut self, radius: usize) {
        self.separable(radius, true);
    }

    /// A pixel becomes set when anything in its square is.
    pub fn dilate(&mut self, radius: usize) {
        self.separable(radius, false);
    }

    /// A square window is a row window followed by a column window, both on the mask packed
    /// into 64-bit words (a pixel per bit), so a radius costs a few shifts per word.
    fn separable(&mut self, radius: usize, erode: bool) {
        let (w, h) = (self.width, self.height);
        if w == 0 || h == 0 || radius == 0 || self.data.len() != w * h {
            return;
        }
        let r = radius.min(MAX_RADIUS);
        let words = w.div_ceil(64);
        let mut bits = vec![0u64; words * h];
        for (row, packed) in self.data.chunks_exact(w).zip(bits.chunks_exact_mut(words)) {
            for (chunk, word) in row.chunks(64).zip(packed.iter_mut()) {
                *word = chunk.iter().enumerate().fold(0, |acc, (i, v)| acc | (u64::from(*v != 0) << i));
            }
        }
        // Bits beyond the last column count as outside: set for an erosion, clear otherwise.
        let tail = if w % 64 == 0 { 0 } else { !0u64 << (w % 64) };
        let fill = if erode { !0u64 } else { 0 };
        let mut line = vec![0u64; words];
        for packed in bits.chunks_exact_mut(words) {
            if let Some(last) = packed.last_mut() {
                *last = if erode { *last | tail } else { *last & !tail };
            }
            line.copy_from_slice(packed);
            for k in 1..=r {
                for (j, out) in packed.iter_mut().enumerate() {
                    let at = |i: Option<usize>| i.and_then(|i| line.get(i)).copied().unwrap_or(fill);
                    let here = at(Some(j));
                    // result[x] = line[x + k] and line[x - k].
                    let up = (here >> k) | (at(j.checked_add(1)) << (64 - k));
                    let down = (here << k) | (at(j.checked_sub(1)) >> (64 - k));
                    *out = if erode { *out & up & down } else { *out | up | down };
                }
            }
        }
        let rows = bits.clone();
        for (y, packed) in bits.chunks_exact_mut(words).enumerate() {
            let lo = y.saturating_sub(r);
            let hi = (y + r + 1).min(h);
            for other in rows.chunks_exact(words).take(hi).skip(lo) {
                for (out, v) in packed.iter_mut().zip(other) {
                    *out = if erode { *out & v } else { *out | v };
                }
            }
        }
        for (row, packed) in self.data.chunks_exact_mut(w).zip(bits.chunks_exact(words)) {
            for (chunk, word) in row.chunks_mut(64).zip(packed) {
                for (i, v) in chunk.iter_mut().enumerate() {
                    *v = (word >> i & 1) as u8;
                }
            }
        }
    }
}

/// Largest radius [`Mask::open`] and [`Mask::close`] use (a shift stays within one word).
pub const MAX_RADIUS: usize = 32;

/// A connected region of a [`Mask`] (8-connected), in pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    /// Pixels in it.
    pub area: usize,
    /// Mean of the pixel centres.
    pub centre: [f64; 2],
    /// Bounding box: first and last column, first and last row, inclusive.
    pub min: [usize; 2],
    pub max: [usize; 2],
    /// Sum of the pixels' colours (of the picture handed to [`components`]).
    pub color_sum: [f64; 3],
}

/// The 8-connected regions of `mask`, largest first, with the colours summed from `colors`
/// (one per pixel, same layout; pass an empty slice for none). Two passes with union-find.
pub fn components(mask: &Mask, colors: &[[f32; 3]]) -> Vec<Region> {
    let (w, h) = (mask.width, mask.height);
    if w == 0 || h == 0 || mask.data.len() != w * h {
        return Vec::new();
    }
    // Provisional labels, 0 = background; `parent[l]` for union-find (index 0 unused).
    let mut labels = vec![0u32; w * h];
    let mut parent: Vec<u32> = vec![0];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if mask.data.get(i).copied().unwrap_or(0) == 0 {
                continue;
            }
            let at = |dx: isize, dy: isize| -> u32 {
                let (Some(nx), Some(ny)) = (x.checked_add_signed(dx), y.checked_add_signed(dy)) else {
                    return 0;
                };
                if nx >= w || ny >= h {
                    return 0;
                }
                labels.get(ny * w + nx).copied().unwrap_or(0)
            };
            // Neighbours already visited: left, and the three above.
            let mut label = 0;
            for n in [at(-1, 0), at(-1, -1), at(0, -1), at(1, -1)] {
                if n == 0 {
                    continue;
                }
                if label == 0 {
                    label = find(&mut parent, n);
                } else {
                    label = union(&mut parent, label, n);
                }
            }
            if label == 0 {
                label = parent.len() as u32;
                parent.push(label);
            }
            if let Some(l) = labels.get_mut(i) {
                *l = label;
            }
        }
    }
    let mut slot = vec![usize::MAX; parent.len()];
    let mut regions: Vec<Region> = Vec::new();
    for (i, l) in labels.iter().enumerate() {
        if *l == 0 {
            continue;
        }
        let root = find(&mut parent, *l) as usize;
        let Some(s) = slot.get_mut(root) else { continue };
        if *s == usize::MAX {
            *s = regions.len();
            regions.push(Region {
                area: 0,
                centre: [0.0; 2],
                min: [usize::MAX; 2],
                max: [0; 2],
                color_sum: [0.0; 3],
            });
        }
        let Some(r) = regions.get_mut(*s) else { continue };
        let (x, y) = (i % w, i / w);
        r.area += 1;
        r.centre[0] += x as f64 + 0.5;
        r.centre[1] += y as f64 + 0.5;
        r.min = [r.min[0].min(x), r.min[1].min(y)];
        r.max = [r.max[0].max(x), r.max[1].max(y)];
        if let Some(c) = colors.get(i) {
            for (sum, v) in r.color_sum.iter_mut().zip(c) {
                *sum += f64::from(*v);
            }
        }
    }
    for r in &mut regions {
        let n = r.area.max(1) as f64;
        r.centre = r.centre.map(|c| c / n);
    }
    regions.sort_by(|a, b| b.area.cmp(&a.area).then(a.min.cmp(&b.min)));
    regions
}

fn find(parent: &mut [u32], mut l: u32) -> u32 {
    let mut root = l;
    while let Some(&p) = parent.get(root as usize) {
        if p == root {
            break;
        }
        root = p;
    }
    // Path compression.
    while let Some(p) = parent.get_mut(l as usize) {
        if *p == root {
            break;
        }
        let next = *p;
        *p = root;
        l = next;
    }
    root
}

fn union(parent: &mut [u32], a: u32, b: u32) -> u32 {
    let (ra, rb) = (find(parent, a), find(parent, b));
    let (lo, hi) = (ra.min(rb), ra.max(rb));
    if let Some(p) = parent.get_mut(hi as usize) {
        *p = lo;
    }
    lo
}
