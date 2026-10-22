//! Small raster tools for the scan's pictures: masks and their morphology, connected parts,
//! distances, median and Gaussian filters. Row-major, `w * h` pixels; callers check sizes before
//! they allocate.

/// A distance larger than any in a picture.
const FAR: u32 = u32::MAX / 4;

/// City-block distance from every pixel to the nearest `source` pixel ([`FAR`] when there is
/// none). The outside of the picture is no source.
pub fn city_distance(w: usize, h: usize, source: &[bool]) -> Vec<u32> {
    let mut d: Vec<u32> = source.iter().map(|s| if *s { 0 } else { FAR }).collect();
    if d.len() != w * h {
        return d;
    }
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut v = d.get(i).copied().unwrap_or(0);
            if x > 0 {
                v = v.min(d.get(i - 1).copied().unwrap_or(FAR).saturating_add(1));
            }
            if y > 0 {
                v = v.min(d.get(i - w).copied().unwrap_or(FAR).saturating_add(1));
            }
            if let Some(slot) = d.get_mut(i) {
                *slot = v;
            }
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            let mut v = d.get(i).copied().unwrap_or(0);
            if x + 1 < w {
                v = v.min(d.get(i + 1).copied().unwrap_or(FAR).saturating_add(1));
            }
            if y + 1 < h {
                v = v.min(d.get(i + w).copied().unwrap_or(FAR).saturating_add(1));
            }
            if let Some(slot) = d.get_mut(i) {
                *slot = v;
            }
        }
    }
    d
}

/// Erosion by a diamond of radius `r` (`r` steps of the 4-neighbour cross). The outside of the
/// picture counts as inside, so a part cut by the frame keeps its edge there.
pub fn erode(w: usize, h: usize, mask: &[bool], r: u32) -> Vec<bool> {
    if r == 0 {
        return mask.to_vec();
    }
    let outside: Vec<bool> = mask.iter().map(|m| !m).collect();
    city_distance(w, h, &outside).iter().map(|d| *d > r).collect()
}

/// Dilation by a diamond of radius `r`.
pub fn dilate(w: usize, h: usize, mask: &[bool], r: u32) -> Vec<bool> {
    if r == 0 {
        return mask.to_vec();
    }
    city_distance(w, h, mask).iter().map(|d| *d <= r).collect()
}

pub fn open(w: usize, h: usize, mask: &[bool], r: u32) -> Vec<bool> {
    dilate(w, h, &erode(w, h, mask, r), r)
}

pub fn close(w: usize, h: usize, mask: &[bool], r: u32) -> Vec<bool> {
    erode(w, h, &dilate(w, h, mask, r), r)
}

/// The mask with every hole filled: background not connected (4 neighbours) to the border.
pub fn fill_holes(w: usize, h: usize, mask: &[bool]) -> Vec<bool> {
    let mut reached = vec![false; mask.len()];
    let mut stack: Vec<usize> = Vec::new();
    let push = |i: usize, reached: &mut Vec<bool>, stack: &mut Vec<usize>| {
        if mask.get(i) == Some(&false)
            && let Some(r) = reached.get_mut(i)
            && !*r
        {
            *r = true;
            stack.push(i);
        }
    };
    for x in 0..w {
        push(x, &mut reached, &mut stack);
        push((h.saturating_sub(1)) * w + x, &mut reached, &mut stack);
    }
    for y in 0..h {
        push(y * w, &mut reached, &mut stack);
        push(y * w + w.saturating_sub(1), &mut reached, &mut stack);
    }
    while let Some(i) = stack.pop() {
        for j in neighbours4(i, w, h).into_iter().flatten() {
            push(j, &mut reached, &mut stack);
        }
    }
    reached.iter().map(|r| !r).collect()
}

/// The four neighbours of pixel `i` that lie in the picture.
pub fn neighbours4(i: usize, w: usize, h: usize) -> [Option<usize>; 4] {
    let (x, y) = (i % w.max(1), i / w.max(1));
    [(x > 0).then(|| i - 1), (x + 1 < w).then(|| i + 1), (y > 0).then(|| i - w), (y + 1 < h).then(|| i + w)]
}

/// Connected parts of a mask (4 neighbours): per pixel its part (0 outside, 1, 2, ... in the
/// order first met) and the size of each part (index 0 for part 1).
pub fn components(w: usize, h: usize, mask: &[bool]) -> (Vec<u32>, Vec<usize>) {
    let mut label = vec![0u32; mask.len()];
    let mut sizes: Vec<usize> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..mask.len() {
        if mask.get(start) != Some(&true) || label.get(start) != Some(&0) {
            continue;
        }
        let id = u32::try_from(sizes.len() + 1).unwrap_or(u32::MAX);
        let mut size = 0usize;
        if let Some(l) = label.get_mut(start) {
            *l = id;
        }
        stack.push(start);
        while let Some(i) = stack.pop() {
            size += 1;
            for j in neighbours4(i, w, h).into_iter().flatten() {
                if mask.get(j) == Some(&true)
                    && let Some(l) = label.get_mut(j)
                    && *l == 0
                {
                    *l = id;
                    stack.push(j);
                }
            }
        }
        sizes.push(size);
    }
    (label, sizes)
}

/// Grows the labels (0 = none) over the pixels of `within`, breadth first through 4 neighbours,
/// so each such pixel takes the label of the nearest labelled pixel reachable inside `within`.
/// Pixels it cannot reach stay 0.
pub fn grow(w: usize, h: usize, labels: &mut [u16], within: &[bool]) {
    let mut front: Vec<usize> =
        (0..labels.len()).filter(|i| labels.get(*i).is_some_and(|l| *l > 0)).collect();
    let mut next: Vec<usize> = Vec::new();
    while !front.is_empty() {
        for &i in &front {
            let l = labels.get(i).copied().unwrap_or(0);
            for j in neighbours4(i, w, h).into_iter().flatten() {
                if within.get(j) == Some(&true)
                    && let Some(t) = labels.get_mut(j)
                    && *t == 0
                {
                    *t = l;
                    next.push(j);
                }
            }
        }
        std::mem::swap(&mut front, &mut next);
        next.clear();
    }
}

/// Euclidean distance from every pixel to the nearest `source` pixel, and that pixel's index
/// (`None` and infinity when there is no source). Felzenszwalb and Huttenlocher's exact
/// transform: one pass down the columns, one along the rows.
pub fn distance(w: usize, h: usize, source: &[bool]) -> (Vec<f32>, Vec<Option<u32>>) {
    const INF: f64 = 1e20;
    let n = w * h;
    if source.len() != n || n == 0 {
        return (vec![f32::INFINITY; source.len()], vec![None; source.len()]);
    }
    // Per column: the squared distance to the nearest source in the column and its row.
    let mut col_d = vec![INF; n];
    let mut col_y = vec![u32::MAX; n];
    let mut f = vec![0f64; h.max(w)];
    let mut out_d = vec![0f64; h.max(w)];
    let mut out_a = vec![0usize; h.max(w)];
    let mut v = vec![0usize; h.max(w)];
    let mut z = vec![0f64; h.max(w) + 1];
    for x in 0..w {
        for y in 0..h {
            if let Some(s) = f.get_mut(y) {
                *s = if source.get(y * w + x) == Some(&true) { 0.0 } else { INF };
            }
        }
        dt1d(f.get(..h).unwrap_or(&[]), &mut out_d, &mut out_a, &mut v, &mut z);
        for y in 0..h {
            let i = y * w + x;
            let d = out_d.get(y).copied().unwrap_or(INF);
            if let Some(c) = col_d.get_mut(i) {
                *c = d;
            }
            if d < INF / 2.0
                && let Some(c) = col_y.get_mut(i)
            {
                *c = out_a.get(y).copied().unwrap_or(0) as u32;
            }
        }
    }
    let mut dist = vec![f32::INFINITY; n];
    let mut nearest: Vec<Option<u32>> = vec![None; n];
    for y in 0..h {
        for x in 0..w {
            if let Some(s) = f.get_mut(x) {
                *s = col_d.get(y * w + x).copied().unwrap_or(INF);
            }
        }
        dt1d(f.get(..w).unwrap_or(&[]), &mut out_d, &mut out_a, &mut v, &mut z);
        for x in 0..w {
            let i = y * w + x;
            let d = out_d.get(x).copied().unwrap_or(INF);
            if d >= INF / 2.0 {
                continue;
            }
            let sx = out_a.get(x).copied().unwrap_or(0);
            let sy = col_y.get(y * w + sx).copied().unwrap_or(u32::MAX);
            if sy == u32::MAX {
                continue;
            }
            if let Some(o) = dist.get_mut(i) {
                *o = d.sqrt() as f32;
            }
            if let Some(o) = nearest.get_mut(i) {
                *o = u32::try_from(sy as usize * w + sx).ok();
            }
        }
    }
    (dist, nearest)
}

/// The 1-D squared distance transform of `f` (lower envelope of parabolas), with the position
/// each value comes from. The scratch slices are at least `f.len()` (`z` one more) long.
fn dt1d(f: &[f64], d: &mut [f64], arg: &mut [usize], v: &mut [usize], z: &mut [f64]) {
    let n = f.len();
    if n == 0 || d.len() < n || arg.len() < n || v.len() < n || z.len() < n + 1 {
        return;
    }
    let at = |s: &[f64], i: usize| s.get(i).copied().unwrap_or(0.0);
    let mut k = 0usize;
    if let Some(s) = v.get_mut(0) {
        *s = 0;
    }
    if let Some(s) = z.get_mut(0) {
        *s = f64::NEG_INFINITY;
    }
    if let Some(s) = z.get_mut(1) {
        *s = f64::INFINITY;
    }
    for q in 1..n {
        let qf = q as f64;
        loop {
            let p = v.get(k).copied().unwrap_or(0);
            let pf = p as f64;
            let s = ((at(f, q) + qf * qf) - (at(f, p) + pf * pf)) / (2.0 * qf - 2.0 * pf);
            if s <= at(z, k) && k > 0 {
                k -= 1;
                continue;
            }
            if s <= at(z, k) {
                // k == 0: the new parabola replaces the first.
                if let Some(slot) = v.get_mut(0) {
                    *slot = q;
                }
                if let Some(slot) = z.get_mut(0) {
                    *slot = f64::NEG_INFINITY;
                }
                if let Some(slot) = z.get_mut(1) {
                    *slot = f64::INFINITY;
                }
                break;
            }
            k += 1;
            if let Some(slot) = v.get_mut(k) {
                *slot = q;
            }
            if let Some(slot) = z.get_mut(k) {
                *slot = s;
            }
            if let Some(slot) = z.get_mut(k + 1) {
                *slot = f64::INFINITY;
            }
            break;
        }
    }
    k = 0;
    for q in 0..n {
        let qf = q as f64;
        while at(z, k + 1) < qf {
            k += 1;
        }
        let p = v.get(k).copied().unwrap_or(0);
        let pf = p as f64;
        if let Some(slot) = d.get_mut(q) {
            *slot = (qf - pf) * (qf - pf) + at(f, p);
        }
        if let Some(slot) = arg.get_mut(q) {
            *slot = p;
        }
    }
}

/// Runs `row(y, out_row)` for every row of an output of `w * h` values, on a few threads.
fn par_rows<T: Send>(w: usize, h: usize, out: &mut [T], row: impl Fn(usize, &mut [T]) + Sync) {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 16);
    let rows_per = h.div_ceil(threads).max(1);
    if w == 0 || out.len() != w * h {
        return;
    }
    std::thread::scope(|s| {
        for (c, chunk) in out.chunks_mut(rows_per * w).enumerate() {
            let row = &row;
            s.spawn(move || {
                for (k, r) in chunk.chunks_mut(w).enumerate() {
                    row(c * rows_per + k, r);
                }
            });
        }
    });
}

/// Mirrors an index into `0..n` (`d c b | a b c d | c b a`, scipy's `reflect`).
fn reflect(i: isize, n: usize) -> usize {
    let n = n as isize;
    if n <= 1 {
        return 0;
    }
    let period = 2 * n;
    let m = i.rem_euclid(period);
    (if m < n { m } else { period - 1 - m }) as usize
}

/// Median over a `size` x `size` window (odd sizes; even ones act as the next odd), mirrored at
/// the border.
pub fn median(w: usize, h: usize, data: &[f32], size: usize) -> Vec<f32> {
    let r = size / 2;
    if r == 0 || data.len() != w * h {
        return data.to_vec();
    }
    let mut out = vec![0f32; data.len()];
    par_rows(w, h, &mut out, |y, row| {
        let mut window: Vec<f32> = Vec::with_capacity((2 * r + 1) * (2 * r + 1));
        for (x, o) in row.iter_mut().enumerate() {
            window.clear();
            for dy in -(r as isize)..=(r as isize) {
                let yy = reflect(y as isize + dy, h);
                for dx in -(r as isize)..=(r as isize) {
                    let xx = reflect(x as isize + dx, w);
                    window.push(data.get(yy * w + xx).copied().unwrap_or(0.0));
                }
            }
            let mid = window.len() / 2;
            if !window.is_empty() {
                let (_, m, _) = window.select_nth_unstable_by(mid, f32::total_cmp);
                *o = *m;
            }
        }
    });
    out
}

/// Gaussian blur with standard deviation `sigma` (pixels), kernel to 3 sigma, mirrored at the
/// border. `sigma` <= 0 leaves the data as it is.
pub fn gaussian(w: usize, h: usize, data: &[f32], sigma: f32) -> Vec<f32> {
    if sigma.is_nan() || sigma <= 0.0 || data.len() != w * h {
        return data.to_vec();
    }
    let r = ((3.0 * sigma).ceil() as usize).clamp(1, 256);
    let mut kernel: Vec<f32> =
        (0..=2 * r).map(|k| (-((k as f32 - r as f32).powi(2)) / (2.0 * sigma * sigma)).exp()).collect();
    let sum: f32 = kernel.iter().sum();
    kernel.iter_mut().for_each(|k| *k /= sum);
    let mut tmp = vec![0f32; data.len()];
    par_rows(w, h, &mut tmp, |y, row| {
        for (x, o) in row.iter_mut().enumerate() {
            let mut acc = 0f32;
            for (k, kv) in kernel.iter().enumerate() {
                let xx = reflect(x as isize + k as isize - r as isize, w);
                acc += kv * data.get(y * w + xx).copied().unwrap_or(0.0);
            }
            *o = acc;
        }
    });
    let mut out = vec![0f32; data.len()];
    par_rows(w, h, &mut out, |y, row| {
        for (x, o) in row.iter_mut().enumerate() {
            let mut acc = 0f32;
            for (k, kv) in kernel.iter().enumerate() {
                let yy = reflect(y as isize + k as isize - r as isize, h);
                acc += kv * tmp.get(yy * w + x).copied().unwrap_or(0.0);
            }
            *o = acc;
        }
    });
    out
}

/// A rectangle of a picture: `x0..x1`, `y0..y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}

impl Rect {
    pub fn width(&self) -> usize {
        self.x1.saturating_sub(self.x0)
    }

    pub fn height(&self) -> usize {
        self.y1.saturating_sub(self.y0)
    }

    /// Grown by `m` on every side, kept inside a `w` x `h` picture.
    pub fn grown(&self, m: usize, w: usize, h: usize) -> Rect {
        Rect {
            x0: self.x0.saturating_sub(m),
            y0: self.y0.saturating_sub(m),
            x1: (self.x1 + m).min(w),
            y1: (self.y1 + m).min(h),
        }
    }

    /// The pixels of `data` (`w` wide) inside the rectangle, row by row.
    pub fn crop<T: Copy + Default>(&self, w: usize, data: &[T]) -> Vec<T> {
        let mut out = Vec::with_capacity(self.width() * self.height());
        for y in self.y0..self.y1 {
            match data.get(y * w + self.x0..y * w + self.x1) {
                Some(row) => out.extend_from_slice(row),
                None => out.extend(std::iter::repeat_n(T::default(), self.width())),
            }
        }
        out
    }
}

/// The bounding rectangle of the pixels where `pick` holds, `None` when there are none.
pub fn bounds<T>(w: usize, data: &[T], pick: impl Fn(&T) -> bool) -> Option<Rect> {
    let mut r: Option<Rect> = None;
    for (i, v) in data.iter().enumerate() {
        if !pick(v) {
            continue;
        }
        let (x, y) = (i % w.max(1), i / w.max(1));
        r = Some(match r {
            None => Rect { x0: x, y0: y, x1: x + 1, y1: y + 1 },
            Some(r) => Rect { x0: r.x0.min(x), y0: r.y0.min(y), x1: r.x1.max(x + 1), y1: r.y1.max(y + 1) },
        });
    }
    r
}
