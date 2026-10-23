//! Platform independent part of the edge glow: geometry, pixels and timing.

use input_event::edge::{Edge, Rect, edge_point};

/// glow size across the edge (into the screen), in logical units
pub const GLOW_DEPTH: f64 = 64.0;
/// glow size along the edge, in logical units
pub const GLOW_LENGTH: f64 = 320.0;

/// fade in (ease-out), milliseconds
#[cfg(any(target_os = "windows", test))]
const FADE_IN_MS: f64 = 40.0;
/// fade out (ease-in), milliseconds
#[cfg(any(target_os = "windows", test))]
const FADE_OUT_MS: f64 = 220.0;

/// glow color
const COLOR: [f64; 3] = [80.0, 160.0, 255.0];

/// Overall opacity (0..1) of the glow `elapsed` after it was triggered,
/// `None` once the animation is over.
#[cfg(any(target_os = "windows", test))]
pub fn glow_opacity(elapsed: std::time::Duration) -> Option<f64> {
    let t = elapsed.as_secs_f64() * 1000.0;
    if t < FADE_IN_MS {
        let p = t / FADE_IN_MS;
        Some(1.0 - (1.0 - p) * (1.0 - p))
    } else if t < FADE_IN_MS + FADE_OUT_MS {
        let p = (t - FADE_IN_MS) / FADE_OUT_MS;
        Some(1.0 - p * p)
    } else {
        None
    }
}

/// Window position and the position of the glow center inside of it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// window rectangle in the coordinates of the displays
    pub rect: Rect,
    /// offset of the glow center from the start of the window along the edge
    pub center_along: f64,
}

/// Find the display containing the point at `fraction` along `edge`,
/// together with that point.
pub fn locate(displays: &[Rect], edge: Edge, fraction: f64) -> Option<(Rect, (f64, f64))> {
    let (x, y) = edge_point(displays, edge, fraction, 0.0)?;
    let display = displays
        .iter()
        .find(|d| d.x_min <= x && x < d.x_max && d.y_min <= y && y < d.y_max)?;
    Some((*display, (x, y)))
}

/// Window of `depth` x `length` attached to `edge` of `display`, centered at
/// `point` along the edge and clipped to the display.
pub fn place(display: Rect, edge: Edge, point: (f64, f64), depth: f64, length: f64) -> Placement {
    let (d_lo, d_hi, center) = match edge {
        Edge::Left | Edge::Right => (display.y_min, display.y_max, point.1),
        Edge::Top | Edge::Bottom => (display.x_min, display.x_max, point.0),
    };
    let lo = (center - length / 2.0).round().max(d_lo);
    let hi = (center + length / 2.0).round().min(d_hi).max(lo + 1.0);
    let depth_max = match edge {
        Edge::Left | Edge::Right => display.x_max - display.x_min,
        Edge::Top | Edge::Bottom => display.y_max - display.y_min,
    };
    let depth = depth.round().clamp(1.0, depth_max.max(1.0));
    let rect = match edge {
        Edge::Left => Rect {
            x_min: display.x_min,
            y_min: lo,
            x_max: display.x_min + depth,
            y_max: hi,
        },
        Edge::Right => Rect {
            x_min: display.x_max - depth,
            y_min: lo,
            x_max: display.x_max,
            y_max: hi,
        },
        Edge::Top => Rect {
            x_min: lo,
            y_min: display.y_min,
            x_max: hi,
            y_max: display.y_min + depth,
        },
        Edge::Bottom => Rect {
            x_min: lo,
            y_min: display.y_max - depth,
            x_max: hi,
            y_max: display.y_max,
        },
    };
    Placement {
        rect,
        // the point is a pixel coordinate, the glow is centered on the pixel
        center_along: center + 0.5 - lo,
    }
}

/// Render the glow as premultiplied RGBA8 (`width * height * 4` bytes).
///
/// The side of the image facing `edge` is the edge of the screen. `center_along`
/// is the position of the glow center along that side in pixels. The glow
/// scales with the image depth (the size across the edge): it is
/// [`GLOW_LENGTH`] long at a depth of [`GLOW_DEPTH`].
pub fn render_glow(edge: Edge, width: u32, height: u32, center_along: f64) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let depth = match edge {
        Edge::Left | Edge::Right => width,
        Edge::Top | Edge::Bottom => height,
    } as f64;
    let length = GLOW_LENGTH * depth / GLOW_DEPTH;

    let mut buf = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            // distance from the screen edge and offset along it
            let (d, s) = match edge {
                Edge::Left => (px, py - center_along),
                Edge::Right => (width as f64 - px, py - center_along),
                Edge::Top => (py, px - center_along),
                Edge::Bottom => (height as f64 - py, px - center_along),
            };
            let core = 0.85
                * (-3.0 * (d / depth).powi(2)).exp()
                * (-3.0 * (s / (length / 2.0)).powi(2)).exp();
            let hot = 0.5
                * (-(d / (0.18 * depth)).powi(2)).exp()
                * (-3.0 * (s / (0.35 * length)).powi(2)).exp();
            let alpha = (core + hot).clamp(0.0, 1.0);

            let i = (y * w + x) * 4;
            buf[i] = (COLOR[0] * alpha).round() as u8;
            buf[i + 1] = (COLOR[1] * alpha).round() as u8;
            buf[i + 2] = (COLOR[2] * alpha).round() as u8;
            buf[i + 3] = (alpha * 255.0).round() as u8;
        }
    }
    buf
}

/// Convert RGBA to BGRA (and back) in place.
#[cfg(any(target_os = "windows", test))]
pub fn swap_red_blue(buf: &mut [u8]) {
    for i in 0..buf.len() / 4 {
        buf.swap(i * 4, i * 4 + 2);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const fn rect(x_min: f64, y_min: f64, x_max: f64, y_max: f64) -> Rect {
        Rect {
            x_min,
            y_min,
            x_max,
            y_max,
        }
    }

    fn alpha_at(buf: &[u8], width: u32, x: u32, y: u32) -> u8 {
        buf[((y * width + x) * 4 + 3) as usize]
    }

    #[test]
    fn buffer_size() {
        for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            assert_eq!(render_glow(edge, 64, 320, 160.0).len(), 64 * 320 * 4);
            assert_eq!(render_glow(edge, 320, 64, 160.0).len(), 320 * 64 * 4);
        }
    }

    #[test]
    fn fades_out_away_from_the_edge() {
        // left edge: the screen edge is x = 0
        let buf = render_glow(Edge::Left, 64, 320, 160.0);
        assert!(alpha_at(&buf, 64, 63, 160) <= 14);
        assert!(alpha_at(&buf, 64, 0, 160) > 200);
        // and away from the center along the edge
        assert!(alpha_at(&buf, 64, 0, 0) <= 14);
        assert!(alpha_at(&buf, 64, 0, 319) <= 14);
    }

    #[test]
    fn maximum_at_edge_center() {
        for (edge, w, h) in [
            (Edge::Left, 64, 320),
            (Edge::Right, 64, 320),
            (Edge::Top, 320, 64),
            (Edge::Bottom, 320, 64),
        ] {
            let center = 100.5;
            let buf = render_glow(edge, w, h, center);
            let max = buf.chunks_exact(4).map(|px| px[3]).max().unwrap();
            // pixel at the screen edge, at the center along it
            // (and its counterpart on the far side of the image)
            let c = center as u32;
            let (near, far) = match edge {
                Edge::Left => ((0, c), (w - 1, c)),
                Edge::Right => ((w - 1, c), (0, c)),
                Edge::Top => ((c, 0), (c, h - 1)),
                Edge::Bottom => ((c, h - 1), (c, 0)),
            };
            assert_eq!(alpha_at(&buf, w, near.0, near.1), max, "{edge:?}");
            assert!(alpha_at(&buf, w, far.0, far.1) <= 14, "{edge:?}");
        }
    }

    #[test]
    fn premultiplied() {
        let buf = render_glow(Edge::Bottom, 320, 64, 100.0);
        for px in buf.chunks_exact(4) {
            assert!(px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3]);
        }
    }

    #[test]
    fn scales_with_depth() {
        // at twice the depth the glow is twice as long
        let buf = render_glow(Edge::Left, 128, 640, 320.0);
        assert!(alpha_at(&buf, 128, 0, 320) > 200);
        assert!(alpha_at(&buf, 128, 0, 0) <= 14);
    }

    #[test]
    fn red_blue_swap() {
        let mut px = [1u8, 2, 3, 4];
        swap_red_blue(&mut px);
        assert_eq!(px, [3, 2, 1, 4]);
    }

    #[test]
    fn opacity_envelope() {
        let at = |ms: u64| glow_opacity(Duration::from_millis(ms));
        assert_eq!(at(0), Some(0.0));
        assert!(at(20).unwrap() > 0.5);
        assert!((at(40).unwrap() - 1.0).abs() < 1e-9);
        assert!(at(150).unwrap() < at(100).unwrap());
        assert!(at(259).unwrap() < 0.01);
        assert_eq!(at(260), None);
    }

    #[test]
    fn placement_is_clipped_to_display() {
        let d = rect(0.0, 0.0, 1920.0, 1080.0);
        let (found, point) = locate(&[d], Edge::Right, 0.5).unwrap();
        assert_eq!(found, d);
        assert_eq!(point.0, 1919.0);

        let p = place(d, Edge::Right, point, 64.0, 320.0);
        assert_eq!(p.rect, rect(1856.0, 380.0, 1920.0, 700.0));
        assert!((p.center_along - 160.0).abs() <= 1.0);

        // near the top: window clipped, center moves accordingly
        let (_, point) = locate(&[d], Edge::Left, 0.0).unwrap();
        let p = place(d, Edge::Left, point, 64.0, 320.0);
        assert_eq!(p.rect, rect(0.0, 0.0, 64.0, 160.0));
        assert_eq!(p.center_along, 0.5);

        let (_, point) = locate(&[d], Edge::Bottom, 1.0).unwrap();
        let p = place(d, Edge::Bottom, point, 64.0, 320.0);
        assert_eq!(p.rect, rect(1759.0, 1016.0, 1920.0, 1080.0));
    }

    #[test]
    fn locate_picks_display_at_the_edge() {
        let a = rect(0.0, 0.0, 1920.0, 1080.0);
        let b = rect(1920.0, 0.0, 3840.0, 1080.0);
        // right edge of the union is the right edge of `b`
        let (found, point) = locate(&[a, b], Edge::Right, 0.5).unwrap();
        assert_eq!(found, b);
        assert_eq!(point.0, 3839.0);
        // fraction is measured along the union: top edge at 25% lies on `a`
        let (found, _) = locate(&[a, b], Edge::Top, 0.25).unwrap();
        assert_eq!(found, a);
        assert!(locate(&[], Edge::Left, 0.5).is_none());
    }
}
