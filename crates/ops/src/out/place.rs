//! Where a projector's texture lands in its window: viewport and fit, as pure arithmetic.

use super::Fit;

/// A rectangle in window pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// What to draw: the target rectangle and the part of the texture (offset and scale in texture
/// coordinates) that goes into it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub dest: Rect,
    pub uv_offset: [f32; 2],
    pub uv_scale: [f32; 2],
}

/// Places a `tex`-sized texture into the `viewport` (x, y, w, h in window sizes) of a
/// `window`-sized surface. `None` when nothing would be visible.
pub fn place(fit: Fit, viewport: [f64; 4], window: (u32, u32), tex: (u32, u32)) -> Option<Placement> {
    let (ww, wh) = (window.0 as f32, window.1 as f32);
    let [vx, vy, vw, vh] = viewport.map(|v| (v as f32).clamp(0.0, 1.0));
    let x0 = vx * ww;
    let y0 = vy * wh;
    let x1 = ((vx + vw) * ww).min(ww);
    let y1 = ((vy + vh) * wh).min(wh);
    let view = Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
    if view.w < 1.0 || view.h < 1.0 || tex.0 == 0 || tex.1 == 0 {
        return None;
    }
    let tex_aspect = tex.0 as f32 / tex.1 as f32;
    let view_aspect = view.w / view.h;
    let full = Placement { dest: view, uv_offset: [0.0, 0.0], uv_scale: [1.0, 1.0] };
    Some(match fit {
        Fit::Stretch => full,
        Fit::Fit if tex_aspect > view_aspect => {
            let h = view.w / tex_aspect;
            Placement { dest: Rect { y: view.y + (view.h - h) / 2.0, h, ..view }, ..full }
        }
        Fit::Fit => {
            let w = view.h * tex_aspect;
            Placement { dest: Rect { x: view.x + (view.w - w) / 2.0, w, ..view }, ..full }
        }
        Fit::Fill if tex_aspect > view_aspect => {
            let s = view_aspect / tex_aspect;
            Placement { uv_offset: [(1.0 - s) / 2.0, 0.0], uv_scale: [s, 1.0], ..full }
        }
        Fit::Fill => {
            let s = tex_aspect / view_aspect;
            Placement { uv_offset: [0.0, (1.0 - s) / 2.0], uv_scale: [1.0, s], ..full }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [f64; 4] = [0.0, 0.0, 1.0, 1.0];

    #[test]
    fn same_shape_fills_the_window() {
        for fit in [Fit::Fit, Fit::Fill, Fit::Stretch] {
            let p = place(fit, ALL, (1920, 1080), (1280, 720)).unwrap();
            assert_eq!(p.dest, Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 });
            assert_eq!((p.uv_offset, p.uv_scale), ([0.0, 0.0], [1.0, 1.0]));
        }
    }

    #[test]
    fn square_into_wide() {
        let fit = place(Fit::Fit, ALL, (1920, 1080), (100, 100)).unwrap();
        assert_eq!(fit.dest, Rect { x: 420.0, y: 0.0, w: 1080.0, h: 1080.0 });
        let fill = place(Fit::Fill, ALL, (1920, 1080), (100, 100)).unwrap();
        assert_eq!(fill.dest.w, 1920.0);
        assert_eq!(fill.uv_scale, [1.0, 0.5625]);
        assert!((fill.uv_offset[1] - 0.21875).abs() < 1e-6);
    }

    #[test]
    fn quarter_of_a_splitter() {
        let p = place(Fit::Stretch, [0.5, 0.5, 0.5, 0.5], (3840, 2160), (1920, 1080)).unwrap();
        assert_eq!(p.dest, Rect { x: 1920.0, y: 1080.0, w: 1920.0, h: 1080.0 });
        let wide = place(Fit::Fit, [0.0, 0.0, 0.5, 1.0], (2000, 1000), (2000, 1000)).unwrap();
        assert_eq!(wide.dest, Rect { x: 0.0, y: 250.0, w: 1000.0, h: 500.0 });
    }

    #[test]
    fn nothing_visible() {
        assert!(place(Fit::Fit, [0.0, 0.0, 0.0, 1.0], (1920, 1080), (10, 10)).is_none());
        assert!(place(Fit::Fit, [1.0, 0.0, 0.5, 1.0], (1920, 1080), (10, 10)).is_none());
        assert!(place(Fit::Fit, ALL, (1920, 1080), (0, 10)).is_none());
        // Outside the window is cut off.
        let p = place(Fit::Stretch, [0.75, 0.0, 0.5, 1.0], (1000, 100), (10, 10)).unwrap();
        assert_eq!(p.dest.w, 250.0);
    }
}
