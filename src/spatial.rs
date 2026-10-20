#![allow(clippy::needless_range_loop)]
//! Immutable XY bounding-volume hierarchy; broad phase only, never acoustic pruning.
use crate::geometry::{dot, sub};
use crate::kernels::{self, Boxes, Segment, WIDTH};
use crate::types::Point;

/// Inward half-space in local coordinates, avoiding GIS-sized plane offsets.
#[derive(Clone, Copy, Debug)]
pub struct HalfSpace {
    pub origin: Point,
    pub normal: Point,
}
pub const BEAM_EPS: f64 = 1e-6;
impl HalfSpace {
    pub fn signed(&self, p: Point) -> f64 {
        dot(sub(p, self.origin), self.normal)
    }
}

#[derive(Clone, Copy, Debug)]
struct Box3 {
    min: Point,
    max: Point,
}
impl Box3 {
    fn from_points(points: &[Point]) -> Self {
        let mut out = Self {
            min: [f64::INFINITY; 3],
            max: [f64::NEG_INFINITY; 3],
        };
        for p in points {
            for k in 0..3 {
                out.min[k] = out.min[k].min(p[k]);
                out.max[k] = out.max[k].max(p[k]);
            }
        }
        out
    }
    fn union(self, other: Self) -> Self {
        Self {
            min: std::array::from_fn(|k| self.min[k].min(other.min[k])),
            max: std::array::from_fn(|k| self.max[k].max(other.max[k])),
        }
    }
    fn intersects(&self, planes: &[HalfSpace]) -> bool {
        planes.iter().all(|h| {
            let extreme = std::array::from_fn(|k| {
                if h.normal[k] >= 0. {
                    self.max[k]
                } else {
                    self.min[k]
                }
            });
            h.signed(extreme) >= -BEAM_EPS
        })
    }
}
#[derive(Debug)]
enum VolumeNode {
    Leaf(Box3, Vec<(usize, Box3)>),
    Branch(Box3, Box<VolumeNode>, Box<VolumeNode>),
}
impl VolumeNode {
    fn build(mut entries: Vec<(usize, Box3)>) -> Self {
        let bounds = entries.iter().map(|e| e.1).reduce(Box3::union).unwrap();
        if entries.len() <= 4 {
            return Self::Leaf(bounds, entries);
        }
        let axis = (0..3)
            .max_by(|a, b| {
                (bounds.max[*a] - bounds.min[*a]).total_cmp(&(bounds.max[*b] - bounds.min[*b]))
            })
            .unwrap();
        entries.sort_by(|a, b| {
            (a.1.min[axis] + a.1.max[axis])
                .total_cmp(&(b.1.min[axis] + b.1.max[axis]))
                .then(a.0.cmp(&b.0))
        });
        let right = entries.split_off(entries.len() / 2);
        Self::Branch(
            bounds,
            Box::new(Self::build(entries)),
            Box::new(Self::build(right)),
        )
    }
    fn query(&self, planes: &[HalfSpace], out: &mut Vec<usize>) {
        match self {
            Self::Leaf(bounds, entries) => {
                if bounds.intersects(planes) {
                    out.extend(
                        entries
                            .iter()
                            .filter(|(_, b)| b.intersects(planes))
                            .map(|(i, _)| *i),
                    );
                }
            }
            Self::Branch(bounds, left, right) => {
                if bounds.intersects(planes) {
                    left.query(planes, out);
                    right.query(planes, out);
                }
            }
        }
    }
    fn any(&self, planes: &[HalfSpace]) -> bool {
        match self {
            Self::Leaf(bounds, entries) => {
                bounds.intersects(planes) && entries.iter().any(|(_, b)| b.intersects(planes))
            }
            Self::Branch(bounds, left, right) => {
                bounds.intersects(planes) && (left.any(planes) || right.any(planes))
            }
        }
    }
}
/// Three-dimensional BVH for finite reflector portals and point receivers.
/// Whole subtrees behind/outside a beam are rejected before individual tests.
#[derive(Debug)]
pub struct VolumeIndex {
    root: Option<VolumeNode>,
}
impl VolumeIndex {
    pub fn new<'a>(objects: impl Iterator<Item = &'a [Point]>) -> Self {
        let entries: Vec<_> = objects
            .enumerate()
            .map(|(i, p)| (i, Box3::from_points(p)))
            .collect();
        Self {
            root: if entries.is_empty() {
                None
            } else {
                Some(VolumeNode::build(entries))
            },
        }
    }
    pub fn candidates(&self, planes: &[HalfSpace]) -> Vec<usize> {
        let mut out = vec![];
        if let Some(root) = &self.root {
            root.query(planes, &mut out);
        }
        out.sort_unstable();
        out
    }
    /// Whether `candidates` would return anything, without collecting them.
    pub fn any(&self, planes: &[HalfSpace]) -> bool {
        self.root.as_ref().is_some_and(|root| root.any(planes))
    }
    /// Smallest `exact(i)` over all entries, by branch and bound: `lower`
    /// must not exceed `exact` for any entry inside the given box (min, max).
    /// Infinite when the index is empty.
    pub fn nearest(
        &self,
        lower: &dyn Fn(Point, Point) -> f64,
        exact: &dyn Fn(usize) -> f64,
    ) -> f64 {
        let mut best = f64::INFINITY;
        if let Some(root) = &self.root {
            root.nearest(lower, exact, &mut best);
        }
        best
    }
}
impl VolumeNode {
    fn nearest(
        &self,
        lower: &dyn Fn(Point, Point) -> f64,
        exact: &dyn Fn(usize) -> f64,
        best: &mut f64,
    ) {
        match self {
            Self::Leaf(bounds, entries) => {
                if lower(bounds.min, bounds.max) < *best {
                    for (i, b) in entries {
                        if lower(b.min, b.max) < *best {
                            *best = best.min(exact(*i));
                        }
                    }
                }
            }
            Self::Branch(bounds, left, right) => {
                if lower(bounds.min, bounds.max) >= *best {
                    return;
                }
                let bound = |n: &VolumeNode| match n {
                    Self::Leaf(b, _) | Self::Branch(b, _, _) => lower(b.min, b.max),
                };
                let (first, second) = if bound(left) <= bound(right) {
                    (left, right)
                } else {
                    (right, left)
                };
                first.nearest(lower, exact, best);
                second.nearest(lower, exact, best);
            }
        }
    }
}
#[derive(Clone, Copy, Debug)]
struct Bounds {
    min: [f64; 2],
    max: [f64; 2],
}
impl Bounds {
    fn union(self, b: Self) -> Self {
        Self {
            min: [self.min[0].min(b.min[0]), self.min[1].min(b.min[1])],
            max: [self.max[0].max(b.max[0]), self.max[1].max(b.max[1])],
        }
    }
}
/// Default XY margin of segment queries, in metres.
pub const SEGMENT_MARGIN: f64 = 1e-7;
/// Query results; inline for the usual handful of candidates.
pub type Candidates = smallvec::SmallVec<[usize; 16]>;
#[derive(Clone, Copy, Debug)]
enum Slot {
    Object(usize),
    Node(usize),
}
/// Up to eight child boxes, tested together.
#[derive(Clone, Debug)]
struct Wide {
    boxes: Boxes,
    slots: [Slot; WIDTH],
}
impl Wide {
    /// Lanes whose box meets the segment, as a bit mask.
    fn hits(&self, segment: &Segment) -> u8 {
        match kernels::installed() {
            Some(k) => k.box_hits(&self.boxes, segment),
            None => kernels::box_hits(&self.boxes, segment),
        }
    }
}
/// Eight-wide bounding-volume tree. A parent's bounds are the exact union of
/// its children's and every step of the slab test is monotone, so an object
/// is reported exactly when its own box passes, whatever the tree shape.
#[derive(Debug, Default)]
pub struct SpatialIndex {
    nodes: Vec<Wide>,
}
fn union_of(entries: &[(usize, Bounds)]) -> Bounds {
    entries
        .iter()
        .map(|(_, b)| *b)
        .reduce(Bounds::union)
        .unwrap()
}
/// Median split along the longer axis, ties broken by object index.
fn halve(mut entries: Vec<(usize, Bounds)>) -> [Vec<(usize, Bounds)>; 2] {
    let bounds = union_of(&entries);
    let axis = usize::from(bounds.max[1] - bounds.min[1] > bounds.max[0] - bounds.min[0]);
    entries.sort_by(|(ai, a), (bi, b)| {
        (a.min[axis] + a.max[axis])
            .total_cmp(&(b.min[axis] + b.max[axis]))
            .then(ai.cmp(bi))
    });
    let right = entries.split_off(entries.len() / 2);
    [entries, right]
}
impl SpatialIndex {
    fn build(&mut self, entries: Vec<(usize, Bounds)>) -> usize {
        let groups = if entries.len() <= WIDTH {
            entries.into_iter().map(|e| vec![e]).collect()
        } else {
            // Three median splits give eight nonempty groups.
            let mut groups = vec![entries];
            for _ in 0..3 {
                groups = groups.into_iter().flat_map(halve).collect();
            }
            groups
        };
        let index = self.nodes.len();
        self.nodes.push(Wide {
            boxes: Boxes {
                min: [[f64::INFINITY; WIDTH]; 2],
                max: [[f64::NEG_INFINITY; WIDTH]; 2],
                len: groups.len(),
            },
            slots: [Slot::Object(0); WIDTH],
        });
        for (lane, group) in groups.into_iter().enumerate() {
            let bounds = union_of(&group);
            let slot = if group.len() == 1 {
                Slot::Object(group[0].0)
            } else {
                Slot::Node(self.build(group))
            };
            let node = &mut self.nodes[index];
            for k in 0..2 {
                node.boxes.min[k][lane] = bounds.min[k];
                node.boxes.max[k][lane] = bounds.max[k];
            }
            node.slots[lane] = slot;
        }
        index
    }
    pub fn new<'a>(polygons: impl Iterator<Item = &'a [Point]>) -> Self {
        let entries = polygons
            .enumerate()
            .map(|(i, p)| {
                let mut b = Bounds {
                    min: [f64::INFINITY; 2],
                    max: [f64::NEG_INFINITY; 2],
                };
                for v in p {
                    for k in 0..2 {
                        b.min[k] = b.min[k].min(v[k]);
                        b.max[k] = b.max[k].max(v[k])
                    }
                }
                (i, b)
            })
            .collect::<Vec<_>>();
        let mut index = Self::default();
        if !entries.is_empty() {
            index.build(entries);
        }
        index
    }
    pub fn candidates(&self, a: Point, b: Point) -> Candidates {
        self.candidates_within(a, b, SEGMENT_MARGIN)
    }
    /// Objects whose XY bounds come within `margin` metres of segment a-b.
    pub fn candidates_within(&self, a: Point, b: Point, margin: f64) -> Candidates {
        let mut out = Candidates::new();
        self.candidates_into(a, b, margin, &mut out);
        out
    }
    /// `candidates_within` into a caller-owned buffer, avoiding the copy of
    /// an inline result in hot loops.
    pub fn candidates_into(&self, a: Point, b: Point, margin: f64, out: &mut Candidates) {
        out.clear();
        if self.nodes.is_empty() {
            return;
        }
        let segment = Segment {
            a,
            d: [b[0] - a[0], b[1] - a[1]],
            eps: margin,
        };
        let mut stack: smallvec::SmallVec<[usize; 32]> = smallvec::smallvec![0];
        while let Some(index) = stack.pop() {
            let node = &self.nodes[index];
            let mut hits = node.hits(&segment);
            while hits != 0 {
                match node.slots[hits.trailing_zeros() as usize] {
                    Slot::Object(i) => out.push(i),
                    Slot::Node(n) => stack.push(n),
                }
                hits &= hits - 1;
            }
        }
        out.sort_unstable();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn broad_phase_never_drops_segment_contact() {
        let polygons = (0..100)
            .map(|i| vec![[i as f64 * 10., 0., 0.], [i as f64 * 10. + 1., 1., 1.]])
            .collect::<Vec<_>>();
        let index = SpatialIndex::new(polygons.iter().map(Vec::as_slice));
        for i in 0..100 {
            let x = i as f64 * 10.;
            assert!(
                index
                    .candidates([x, -5., 100.], [x, 5., -100.])
                    .contains(&i)
            );
            assert!(index.candidates([x, 0., 0.], [x, 0., 0.]).contains(&i));
        }
        assert!(index.candidates([-100., 0., 0.], [-50., 0., 0.]).is_empty());
    }

    /// Deterministic pseudo-random scene: boxes of many sizes, segments
    /// including axis-parallel, degenerate and grazing ones.
    type Queries = Vec<(Point, Point, f64)>;
    fn random_scene(seed: u64) -> (Vec<Vec<Point>>, Queries) {
        let mut state = seed;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 11) as f64 / (1u64 << 53) as f64
        };
        let polygons: Vec<Vec<Point>> = (0..300)
            .map(|_| {
                let (x, y) = (next() * 1000. + 500_000., next() * 1000. + 4_000_000.);
                let (w, h) = (next().powi(3) * 60., next().powi(3) * 60.);
                vec![[x, y, 0.], [x + w, y, 0.], [x + w, y + h, 0.]]
            })
            .collect();
        let mut segments = vec![];
        for i in 0..2000 {
            let a = [next() * 1000. + 500_000., next() * 1000. + 4_000_000., 0.];
            let mut b = [next() * 1000. + 500_000., next() * 1000. + 4_000_000., 0.];
            match i % 4 {
                1 => b[0] = a[0],
                2 => b[1] = a[1],
                3 => b = a,
                _ => {}
            }
            // Corners of a box, exactly and within the margin.
            if i % 7 == 0 {
                let p = &polygons[i % polygons.len()];
                b = [p[1][0] + 1e-7, p[2][1], 0.];
            }
            segments.push((a, b, [1e-7, 1e-6, 0.][i % 3]));
        }
        (polygons, segments)
    }

    #[test]
    fn wide_tree_reports_exactly_the_boxes_that_meet_the_segment() {
        let (polygons, segments) = random_scene(17);
        let index = SpatialIndex::new(polygons.iter().map(Vec::as_slice));
        let single = SpatialIndex::new(polygons.iter().take(1).map(Vec::as_slice));
        for (a, b, eps) in segments {
            let segment = Segment {
                a,
                d: [b[0] - a[0], b[1] - a[1]],
                eps,
            };
            let exhaustive: Vec<usize> = polygons
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    let mut bounds = Bounds {
                        min: [f64::INFINITY; 2],
                        max: [f64::NEG_INFINITY; 2],
                    };
                    for v in p.iter() {
                        for k in 0..2 {
                            bounds.min[k] = bounds.min[k].min(v[k]);
                            bounds.max[k] = bounds.max[k].max(v[k]);
                        }
                    }
                    kernels::box_meets(bounds.min, bounds.max, &segment)
                })
                .map(|(i, _)| i)
                .collect();
            assert_eq!(index.candidates_within(a, b, eps).to_vec(), exhaustive);
            assert_eq!(
                single.candidates_within(a, b, eps).to_vec(),
                exhaustive
                    .iter()
                    .copied()
                    .filter(|i| *i == 0)
                    .collect::<Vec<_>>()
            );
        }
    }
}
