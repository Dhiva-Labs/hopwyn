//! Screen arrangement geometry: where a neighbour's screen touches this
//! one, and how a point on the shared edge maps between the two screens.
//!
//! Each side keeps, for each neighbour, the edge it sits beyond and an
//! `offset`: how far (in this screen's logical pixels) the neighbour's top
//! or left starts from this desktop's top or left along that edge. Screens
//! of different sizes and shifted screens then line up the way they do on
//! the desk, and the cursor crosses only where they actually touch.

use hopwyn_proto::Edge;

/// A desktop's size in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

/// Length of `size` along `edge` (height for left/right, width for top/bottom).
pub fn edge_len(size: Size, edge: Edge) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    match edge {
        Edge::Left | Edge::Right => size.height as f32,
        Edge::Top | Edge::Bottom => size.width as f32,
    }
}

/// How a neighbour lines up along one of this desktop's edges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    /// This desktop's length along the edge.
    pub mine: f32,
    /// The neighbour's length along its facing edge.
    pub theirs: f32,
    /// Where the neighbour starts along this edge, in this desktop's pixels.
    pub offset: f32,
}

impl Span {
    /// The span for a neighbour beyond `edge`. With either size unknown the
    /// screens are treated as the same size and aligned (identity mapping).
    #[allow(clippy::cast_precision_loss)]
    pub fn new(mine: Option<Size>, theirs: Option<Size>, edge: Edge, offset: i32) -> Self {
        match (mine, theirs) {
            (Some(m), Some(t)) => Self {
                mine: edge_len(m, edge).max(1.0),
                theirs: edge_len(t, edge).max(1.0),
                offset: offset as f32,
            },
            _ => Self {
                mine: 1.0,
                theirs: 1.0,
                offset: 0.0,
            },
        }
    }

    /// A crossing at `p` (0..1 along this edge) as a position along the
    /// neighbour's edge, or `None` where the screens don't touch.
    pub fn across(&self, p: f32) -> Option<f32> {
        let along = p.clamp(0.0, 1.0) * self.mine - self.offset;
        // A pixel of slack, so the very end of a touching edge still counts.
        if along < -1.0 || along > self.theirs + 1.0 {
            return None;
        }
        Some((along / self.theirs).clamp(0.0, 1.0))
    }

    /// A position along the neighbour's edge as a point on this edge (the
    /// inverse of `across`; the neighbour does this mapping itself).
    #[cfg(test)]
    pub fn back(&self, q: f32) -> f32 {
        ((q.clamp(0.0, 1.0) * self.theirs + self.offset) / self.mine).clamp(0.0, 1.0)
    }

    /// The part of this edge the neighbour touches, as fractions.
    pub fn touching(&self) -> Option<(f32, f32)> {
        let start = (self.offset / self.mine).clamp(0.0, 1.0);
        let end = ((self.offset + self.theirs) / self.mine).clamp(0.0, 1.0);
        (end > start).then_some((start, end))
    }
}

/// The offset that keeps a neighbour touching this edge: at least a tenth
/// of the shorter screen overlaps, so it can always be reached.
pub fn clamp_offset(mine: f32, theirs: f32, offset: f32) -> f32 {
    let overlap = (mine.min(theirs) * 0.1).max(1.0);
    offset.clamp(overlap - theirs, mine - overlap)
}

/// Where a neighbour dropped at `rect` (x, y, width, height in this
/// desktop's logical pixels, its top-left at 0,0) ends up: the edge it is
/// furthest out past, and its offset along that edge, kept reachable.
pub fn snap(mine: Size, rect: (f64, f64, f64, f64)) -> (Edge, i32) {
    let (x, y, w, h) = rect;
    let (mw, mh) = (
        f64::from(mine.width).max(1.0),
        f64::from(mine.height).max(1.0),
    );
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let candidates = [
        (Edge::Right, (cx - mw) / mw),
        (Edge::Left, -cx / mw),
        (Edge::Top, -cy / mh),
        (Edge::Bottom, (cy - mh) / mh),
    ];
    let edge = candidates
        .into_iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map_or(Edge::Right, |(e, _)| e);
    let (along, mine_len, theirs_len) = match edge {
        Edge::Left | Edge::Right => (y, mh, h),
        Edge::Top | Edge::Bottom => (x, mw, w),
    };
    #[allow(clippy::cast_possible_truncation)]
    let offset = clamp_offset(mine_len as f32, theirs_len as f32, along as f32).round() as i32;
    (edge, offset)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::float_cmp)]

    use super::*;

    const FHD: Size = Size {
        width: 1920,
        height: 1080,
    };
    const QHD: Size = Size {
        width: 2560,
        height: 1440,
    };

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn same_size_aligned_screens_map_one_to_one() {
        let s = Span::new(Some(FHD), Some(FHD), Edge::Right, 0);
        for p in [0.0, 0.25, 0.5, 1.0] {
            assert!(near(s.across(p).unwrap(), p));
            assert!(near(s.back(p), p));
        }
        assert_eq!(s.touching(), Some((0.0, 1.0)));
    }

    #[test]
    fn unknown_sizes_map_one_to_one() {
        let s = Span::new(None, Some(QHD), Edge::Right, 500);
        assert!(near(s.across(0.3).unwrap(), 0.3));
    }

    #[test]
    fn a_shorter_neighbour_shifted_down_is_reached_only_where_it_touches() {
        // A 1080-high laptop beside a 1440-high monitor, bottoms aligned.
        let s = Span::new(Some(QHD), Some(FHD), Edge::Right, 360);
        assert_eq!(s.across(0.1), None); // above the laptop
        assert!(near(s.across(0.25).unwrap(), 0.0)); // its top
        assert!(near(s.across(1.0).unwrap(), 1.0)); // its bottom
        assert!(near(s.across(0.625).unwrap(), 0.5)); // its middle
        let (a, b) = s.touching().unwrap();
        assert!(near(a, 0.25) && near(b, 1.0));
    }

    #[test]
    fn coming_back_lands_at_the_matching_spot() {
        let s = Span::new(Some(QHD), Some(FHD), Edge::Right, 360);
        for q in [0.0, 0.3, 0.9, 1.0] {
            let p = s.back(q);
            assert!(near(s.across(p).unwrap(), q), "q {q}");
        }
    }

    #[test]
    fn dropping_snaps_to_the_nearest_side_keeping_the_slide() {
        // Dropped to the right, a bit lower: right side, offset kept.
        assert_eq!(
            snap(QHD, (2700.0, 300.0, 1920.0, 1080.0)),
            (Edge::Right, 300)
        );
        // Dropped above, shifted right.
        assert_eq!(
            snap(QHD, (400.0, -1200.0, 1920.0, 1080.0)),
            (Edge::Top, 400)
        );
        // Dropped far left and too low: left side, clamped to stay reachable.
        assert_eq!(
            snap(QHD, (-2500.0, 1400.0, 1920.0, 1080.0)),
            (Edge::Left, 1440 - 108)
        );
        // Further below than to the left: it goes below.
        assert_eq!(snap(QHD, (-2500.0, 5000.0, 1920.0, 1080.0)).0, Edge::Bottom);
        // Dropped below.
        assert_eq!(snap(QHD, (0.0, 1500.0, 1920.0, 1080.0)), (Edge::Bottom, 0));
    }

    #[test]
    fn offsets_are_clamped_so_the_neighbour_stays_reachable() {
        assert_eq!(clamp_offset(1440.0, 1080.0, 5000.0), 1440.0 - 108.0);
        assert_eq!(clamp_offset(1440.0, 1080.0, -5000.0), 108.0 - 1080.0);
        assert_eq!(clamp_offset(1440.0, 1080.0, 200.0), 200.0);
    }
}
