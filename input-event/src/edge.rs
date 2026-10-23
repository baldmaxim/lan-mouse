//! Geometry helpers for transferring the cursor position along a screen edge.
//!
//! All functions operate on the union of all displays: the fraction is
//! measured along the combined extent of the displays, not along a single one.

/// A screen edge
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

/// A display rectangle, `x_max` / `y_max` are exclusive (like `RECT.right`)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x_min: f64,
    pub y_min: f64,
    pub x_max: f64,
    pub y_max: f64,
}

/// the axis range `(min, max)` of a rect along the given edge
fn edge_range(rect: &Rect, edge: Edge) -> (f64, f64) {
    match edge {
        Edge::Left | Edge::Right => (rect.y_min, rect.y_max),
        Edge::Top | Edge::Bottom => (rect.x_min, rect.x_max),
    }
}

/// the range `(min, max)` of the union of all displays along the given edge
fn union_range(displays: &[Rect], edge: Edge) -> Option<(f64, f64)> {
    displays
        .iter()
        .map(|d| edge_range(d, edge))
        .reduce(|(a_min, a_max), (b_min, b_max)| (a_min.min(b_min), a_max.max(b_max)))
}

/// Fraction (0..=1) of `point` along the given edge, relative to the union of all displays.
/// For [`Edge::Left`] / [`Edge::Right`] this is measured along y, for
/// [`Edge::Top`] / [`Edge::Bottom`] along x.
/// Returns `None` if there are no displays.
pub fn edge_fraction(displays: &[Rect], edge: Edge, point: (f64, f64)) -> Option<f64> {
    let (min, max) = union_range(displays, edge)?;
    let coord = match edge {
        Edge::Left | Edge::Right => point.1,
        Edge::Top | Edge::Bottom => point.0,
    };
    let len = max - 1.0 - min;
    if len <= 0.0 {
        return Some(0.5);
    }
    Some(((coord - min) / len).clamp(0.0, 1.0))
}

/// Point on the given edge at `fraction` along the union of all displays,
/// moved `inset` pixels towards the inside of the display.
/// Returns `None` if there are no displays.
pub fn edge_point(displays: &[Rect], edge: Edge, fraction: f64, inset: f64) -> Option<(f64, f64)> {
    let (min, max) = union_range(displays, edge)?;
    let fraction = fraction.clamp(0.0, 1.0);
    let mut t = min + fraction * (max - 1.0 - min).max(0.0);

    let mut candidates: Vec<&Rect> = displays
        .iter()
        .filter(|d| {
            let (d_min, d_max) = edge_range(d, edge);
            d_min <= t && t < d_max
        })
        .collect();

    if candidates.is_empty() {
        // t falls into a gap between displays: use the closest display
        // and clamp t into its range
        let dist = |d: &Rect| {
            let (d_min, d_max) = edge_range(d, edge);
            if t < d_min {
                d_min - t
            } else {
                t - (d_max - 1.0)
            }
        };
        let closest = displays.iter().min_by(|a, b| dist(a).total_cmp(&dist(b)))?;
        let (d_min, d_max) = edge_range(closest, edge);
        t = t.clamp(d_min, (d_max - 1.0).max(d_min));
        candidates.push(closest);
    }

    // the candidate that is the outermost towards the edge
    let d = match edge {
        Edge::Left => candidates
            .into_iter()
            .min_by(|a, b| a.x_min.total_cmp(&b.x_min)),
        Edge::Right => candidates
            .into_iter()
            .max_by(|a, b| a.x_max.total_cmp(&b.x_max)),
        Edge::Top => candidates
            .into_iter()
            .min_by(|a, b| a.y_min.total_cmp(&b.y_min)),
        Edge::Bottom => candidates
            .into_iter()
            .max_by(|a, b| a.y_max.total_cmp(&b.y_max)),
    }?;

    Some(match edge {
        Edge::Left => (d.x_min + inset, t),
        Edge::Right => (d.x_max - 1.0 - inset, t),
        Edge::Top => (t, d.y_min + inset),
        Edge::Bottom => (t, d.y_max - 1.0 - inset),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn rect(x_min: f64, y_min: f64, x_max: f64, y_max: f64) -> Rect {
        Rect {
            x_min,
            y_min,
            x_max,
            y_max,
        }
    }

    #[test]
    fn single_display() {
        let d = [rect(0.0, 0.0, 1920.0, 1080.0)];
        assert_eq!(edge_fraction(&d, Edge::Left, (0.0, 0.0)), Some(0.0));
        assert_eq!(edge_fraction(&d, Edge::Left, (0.0, 1079.0)), Some(1.0));
        let f = edge_fraction(&d, Edge::Right, (1919.0, 539.5)).unwrap();
        assert!((f - 0.5).abs() < 1e-9);
        assert_eq!(edge_fraction(&d, Edge::Top, (1919.0, 0.0)), Some(1.0));
        // out of range is clamped
        assert_eq!(edge_fraction(&d, Edge::Left, (0.0, -50.0)), Some(0.0));
        assert_eq!(edge_fraction(&d, Edge::Bottom, (5000.0, 0.0)), Some(1.0));

        assert_eq!(edge_point(&d, Edge::Left, 0.5, 2.0), Some((2.0, 539.5)));
        assert_eq!(edge_point(&d, Edge::Right, 0.0, 2.0), Some((1917.0, 0.0)));
        assert_eq!(edge_point(&d, Edge::Top, 1.0, 2.0), Some((1919.0, 2.0)));
        assert_eq!(edge_point(&d, Edge::Bottom, 0.0, 0.0), Some((0.0, 1079.0)));
    }

    #[test]
    fn no_displays() {
        assert_eq!(edge_fraction(&[], Edge::Left, (0.0, 0.0)), None);
        assert_eq!(edge_point(&[], Edge::Left, 0.5, 2.0), None);
    }

    #[test]
    fn zero_length_axis() {
        let d = [rect(0.0, 0.0, 1.0, 1.0)];
        assert_eq!(edge_fraction(&d, Edge::Left, (0.0, 0.0)), Some(0.5));
        assert_eq!(edge_point(&d, Edge::Left, 0.5, 0.0), Some((0.0, 0.0)));
    }

    #[test]
    fn side_by_side_different_height() {
        // left: 1920x1080, right: 1000x1500 (taller), top aligned
        let d = [
            rect(0.0, 0.0, 1920.0, 1080.0),
            rect(1920.0, 0.0, 2920.0, 1500.0),
        ];
        // fraction along the union height (1500)
        let f = edge_fraction(&d, Edge::Right, (2919.0, 749.5)).unwrap();
        assert!((f - 0.5).abs() < 1e-9);
        // left edge: only the left display exists, y beyond its height clamps to the union
        let f = edge_fraction(&d, Edge::Left, (0.0, 1079.0)).unwrap();
        assert!((f - 1079.0 / 1499.0).abs() < 1e-9);

        // Right edge at 0.5 -> y = 749.5, only the right display contains it
        assert_eq!(edge_point(&d, Edge::Right, 0.5, 2.0), Some((2917.0, 749.5)));
        // Left edge at 0.25 -> y = 374.75, both contain it, leftmost wins
        assert_eq!(edge_point(&d, Edge::Left, 0.25, 2.0), Some((2.0, 374.75)));
        // Left edge at 1.0 -> y = 1499, only the right display contains it
        assert_eq!(edge_point(&d, Edge::Left, 1.0, 2.0), Some((1922.0, 1499.0)));
    }

    #[test]
    fn stacked_displays() {
        let d = [
            rect(0.0, 0.0, 1920.0, 1080.0),
            rect(0.0, 1080.0, 1920.0, 2160.0),
        ];
        let f = edge_fraction(&d, Edge::Left, (0.0, 1080.0)).unwrap();
        assert!((f - 1080.0 / 2159.0).abs() < 1e-9);
        // top edge belongs to the upper display, bottom edge to the lower one
        assert_eq!(edge_point(&d, Edge::Top, 0.5, 2.0), Some((959.5, 2.0)));
        assert_eq!(
            edge_point(&d, Edge::Bottom, 0.5, 2.0),
            Some((959.5, 2157.0))
        );
        // left edge at 0.75 -> lower display
        let (x, y) = edge_point(&d, Edge::Left, 0.75, 2.0).unwrap();
        assert_eq!(x, 2.0);
        assert!((y - 1619.25).abs() < 1e-9);
    }

    #[test]
    fn no_candidate_clamps_to_closest() {
        // a gap in y between 1000 and 1200 on the shared left edge
        let d = [
            rect(0.0, 0.0, 1000.0, 1000.0),
            rect(1000.0, 1200.0, 2000.0, 2000.0),
        ];
        // union y: 0..2000, fraction 0.55 -> t = 1099.45, in the gap
        // the closest display (first, distance 99.45 vs 100.55) is used and t clamped to 999
        assert_eq!(edge_point(&d, Edge::Left, 0.55, 2.0), Some((2.0, 999.0)));
        // fraction 0.57 -> t = 1139.43, closer to the second display
        assert_eq!(
            edge_point(&d, Edge::Right, 0.57, 2.0),
            Some((1997.0, 1200.0))
        );
    }

    #[test]
    fn fraction_bounds() {
        let d = [rect(100.0, 50.0, 1100.0, 850.0)];
        assert_eq!(edge_point(&d, Edge::Left, 0.0, 2.0), Some((102.0, 50.0)));
        assert_eq!(edge_point(&d, Edge::Left, 1.0, 2.0), Some((102.0, 849.0)));
        assert_eq!(edge_point(&d, Edge::Top, 0.0, 2.0), Some((100.0, 52.0)));
        assert_eq!(edge_point(&d, Edge::Top, 1.0, 2.0), Some((1099.0, 52.0)));
        // fractions outside 0..1 are clamped
        assert_eq!(edge_point(&d, Edge::Top, 2.0, 2.0), Some((1099.0, 52.0)));
        assert_eq!(edge_point(&d, Edge::Top, -1.0, 2.0), Some((100.0, 52.0)));
    }
}
