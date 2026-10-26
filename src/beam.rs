//! Source-first finite-portal beam search. Each admitted receiver spawns a terminal stub.
//! Convex portal/source envelopes are conservative; exact image/aperture tests
//! decide point connections and integration resolves extended-source coverage.
use crate::{geometry::*, path::*, scene::*, spatial::*, types::*};
use std::sync::atomic::AtomicBool;

pub struct SearchIndex {
    pub receivers: VolumeIndex,
    faces: VolumeIndex,
}
impl SearchIndex {
    pub fn new(scene: &Scene) -> Self {
        Self {
            receivers: VolumeIndex::new(
                scene
                    .receivers
                    .iter()
                    .map(|r| std::slice::from_ref(&r.point)),
            ),
            faces: VolumeIndex::new(scene.faces.iter().map(|f| f.vertices.as_slice())),
        }
    }
}
/// One terminal calculation task. Owns its receiver-specific geometry and band mask;
/// the parent beam supplies immutable reflection history shared by all its children.
/// Terminal receiver stubs never query reflectors or spawn reflection branches.
pub struct ReceiverStub {
    pub receiver: usize,
    pub reference: Option<PreparedReference>,
    /// A pathway error met while preparing this connection's reference. The
    /// stub is still evaluated so the receiver records the failed pathway.
    pub failure: Option<Diagnostic>,
}
pub struct Stub {
    pub family: Family,
    pub receiver_stubs: Vec<ReceiverStub>,
}
#[derive(Default)]
pub struct SearchStats {
    pub checked: usize,
    pub empty: usize,
    pub receiver_candidates: usize,
    pub geometry_rejected: usize,
    pub reflector_candidates: usize,
    /// Reflection beams dropped before expansion because every band's prefix
    /// bound was below the absolute cutoff.
    pub prefix_pruned: usize,
}
pub struct Search {
    pub stubs: Vec<Stub>,
    pub stats: SearchStats,
}
struct Beam {
    images: Vec<Point>,
    planes: Vec<HalfSpace>,
    point_images: Option<Vec<Point>>,
    /// Product of (1 - absorption) over the reflections so far.
    material: Bands,
    /// Bands whose prefix bound has stayed at or above the cutoff.
    active: [bool; 8],
}

fn source_envelope(source: &Source) -> Vec<Point> {
    if source.kind == "point" {
        return vec![source.zones[0].cells[0].centroid()];
    }
    let mut low = [f64::INFINITY; 3];
    let mut high = [f64::NEG_INFINITY; 3];
    for zone in &source.zones {
        for cell in &zone.cells {
            for p in cell.vertices() {
                for k in 0..3 {
                    low[k] = low[k].min(p[k]);
                    high[k] = high[k].max(p[k]);
                }
            }
        }
    }
    (0..8)
        .map(|i| std::array::from_fn(|k| if i & (1 << k) == 0 { low[k] } else { high[k] }))
        .collect()
}

fn segment_distance(p: Point, a: Point, b: Point) -> f64 {
    let ab = sub(b, a);
    let t = (dot(sub(p, a), ab) / dot(ab, ab).max(f64::MIN_POSITIVE)).clamp(0., 1.);
    distance(p, add(a, scale(ab, t)))
}

/// Shortest distance from `p` to a planar convex polygon.
fn polygon_distance(p: Point, polygon: &[Point], normal: Point) -> f64 {
    let n = polygon.len();
    let side = |i: usize| {
        dot(
            cross(sub(polygon[(i + 1) % n], polygon[i]), sub(p, polygon[i])),
            normal,
        )
    };
    if (0..n).all(|i| side(i) >= 0.) || (0..n).all(|i| side(i) <= 0.) {
        return dot(sub(p, polygon[0]), normal).abs();
    }
    (0..n)
        .map(|i| segment_distance(p, polygon[i], polygon[(i + 1) % n]))
        .fold(f64::INFINITY, f64::min)
}

/// Lower bound on the length of every route through `face` to any receiver:
/// the straight distance from the previous image domain to the face (the
/// unfolded prefix, with any diffraction, is no shorter) plus the straight
/// distance from the face to the nearest receiver (any continuation, with
/// further reflections or diffraction, is no shorter). A non-point domain is
/// enclosed by the bounding sphere of its imaged box.
fn reach(images: &[Point], face: &Face, scene: &Scene, index: &SearchIndex) -> (f64, f64) {
    let portal: Vec<Point> = {
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        for p in &face.projected {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        [
            [lo[0], lo[1]],
            [hi[0], lo[1]],
            [hi[0], hi[1]],
            [lo[0], hi[1]],
        ]
        .iter()
        .map(|p| add(face.origin, add(scale(face.u, p[0]), scale(face.v, p[1]))))
        .collect()
    };
    let centre = scale(
        images.iter().fold([0.; 3], |a, p| add(a, *p)),
        1. / images.len() as f64,
    );
    let radius = images
        .iter()
        .map(|p| distance(*p, centre))
        .fold(0., f64::max);
    let scale_m = portal
        .iter()
        .chain(images)
        .flatten()
        .map(|x| x.abs())
        .fold(1., f64::max);
    // Nearest receiver to the face portal; box-to-box distance prunes the tree.
    let (plo, phi) = portal.iter().fold(
        ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]),
        |(lo, hi), p| {
            (
                std::array::from_fn(|k| lo[k].min(p[k])),
                std::array::from_fn(|k| hi[k].max(p[k])),
            )
        },
    );
    let onward = index.receivers.nearest(
        &|lo: Point, hi: Point| {
            (0..3)
                .map(|k| (lo[k] - phi[k]).max(plo[k] - hi[k]).max(0.).powi(2))
                .sum::<f64>()
                .sqrt()
        },
        &|ri| polygon_distance(scene.receivers[ri].point, &portal, face.normal),
    );
    let margin = 1e-9 * scale_m;
    (
        (polygon_distance(centre, &portal, face.normal) - radius - margin).max(0.),
        (onward - margin).max(0.),
    )
}

/// Bands of a reflection prefix that may still reach the cutoff. Same
/// conservative terms as the integration cell bound: whole-source power,
/// maximum directivity, -6 dB net ground allowance, divergence and air
/// absorption over `reach`, and the reflections' material loss. Screening,
/// meteorology and miscellaneous losses are taken as zero.
///
/// A band is also dropped when the face cannot satisfy the ISO 9613-2:2024
/// equation (26) size criterion for any continuation: its effective size is
/// at most its smaller in-plane extent, and the required size only grows with
/// the source- and receiver-side path lengths bounded below here.
fn prefix_active(
    scene: &Scene,
    cutoffs: &Cutoffs,
    source: &Source,
    beam: &Beam,
    face: &Face,
    (prefix, onward): (f64, f64),
) -> [bool; 8] {
    let reach = prefix + onward;
    if !pruning(cutoffs) || reach <= 0. {
        return beam.active;
    }
    // Upper bounds of the `path::reflection_mask` effective-size terms.
    let range = |values: &mut dyn Iterator<Item = f64>| {
        let (lo, hi) = values.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), x| {
            (lo.min(x), hi.max(x))
        });
        hi - lo
    };
    let size = if face.vertical() {
        let along = [face.normal[1], -face.normal[0], 0.];
        range(&mut face.vertices.iter().map(|q| dot(*q, along)))
            .min(range(&mut face.vertices.iter().map(|q| q[2])))
    } else {
        range(&mut face.projected.iter().map(|q| q[0]))
            .min(range(&mut face.projected.iter().map(|q| q[1])))
    };
    std::array::from_fn(|b| {
        if !beam.active[b] {
            return false;
        }
        if prefix > 0. && onward > 0. {
            let harmonic = 2. / (prefix.recip() + onward.recip());
            if size <= ((340. / crate::acoustics::FREQUENCIES[b]) * harmonic).sqrt() {
                return false;
            }
        }
        let power: f64 = source.zones.iter().map(|z| z.power[b]).sum();
        let directivity = source
            .zones
            .iter()
            .map(|z| {
                z.directivity
                    .as_ref()
                    .map_or(0., |d| d.maximum_correction()[b])
            })
            .fold(f64::NEG_INFINITY, f64::max);
        if power <= 0. || beam.material[b] <= 0. {
            return false;
        }
        let logarithm = power.log10() + beam.material[b].log10() + (directivity + 6. - 11.) / 10.
            - 2. * reach.log10()
            - scene.alpha[b] * reach / 10.;
        let bound = 10f64.powf(logarithm) * (1. + 1e-12);
        !below_cutoff(cutoffs, b, bound)
    })
}

fn clip(mut polygon: Vec<Point>, planes: &[HalfSpace]) -> Vec<Point> {
    for h in planes {
        if polygon.is_empty() {
            break;
        }
        let mut next = vec![];
        for i in 0..polygon.len() {
            let a = polygon[i];
            let b = polygon[(i + 1) % polygon.len()];
            let da = h.signed(a) + BEAM_EPS;
            let db = h.signed(b) + BEAM_EPS;
            if da >= 0. {
                next.push(a);
            }
            if (da >= 0.) != (db >= 0.) {
                next.push(lerp(a, b, da / (da - db)));
            }
        }
        polygon = next;
    }
    polygon
}

fn extend(parent: &Beam, face: &Face) -> Option<Beam> {
    if !parent
        .images
        .iter()
        .any(|p| face_accepts_endpoint(face, *p))
    {
        return None;
    }
    if parent.images.len() == 1 && face.signed(parent.images[0]).abs() < 1e-9 {
        return None;
    }
    // A rectangular portal encloses any authored concave polygon; never use its
    // centroid as an aperture. Final exact checks reject the excess coverage.
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for p in &face.projected {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    for k in 0..2 {
        lo[k] -= BEAM_EPS;
        hi[k] += BEAM_EPS;
    }
    let portal: Vec<_> = [
        [lo[0], lo[1]],
        [hi[0], lo[1]],
        [hi[0], hi[1]],
        [lo[0], hi[1]],
    ]
    .iter()
    .map(|p| add(face.origin, add(scale(face.u, p[0]), scale(face.v, p[1]))))
    .collect();
    let portal = clip(portal, &parent.planes);
    if portal.len() < 3 {
        return None;
    }
    let images: Vec<_> = parent.images.iter().map(|p| face.mirror(*p)).collect();
    let mut planes = vec![];
    let side = match face.side.as_str() {
        "front" => Some(1.),
        "back" => Some(-1.),
        _ if parent.images.iter().all(|p| face.signed(*p) > 0.) => Some(1.),
        _ if parent.images.iter().all(|p| face.signed(*p) < 0.) => Some(-1.),
        _ => None,
    };
    if let Some(side) = side {
        planes.push(HalfSpace {
            origin: face.origin,
            normal: scale(face.normal, side),
        });
    }
    let depth = side
        .map(|side| {
            images
                .iter()
                .map(|p| -side * face.signed(*p))
                .fold(f64::INFINITY, f64::min)
        })
        .unwrap_or(0.);
    for i in 0..portal.len() {
        let a = portal[i];
        let b = portal[(i + 1) % portal.len()];
        for image in &images {
            let n = cross(sub(b, a), sub(*image, a));
            let size = norm(n);
            if size < 1e-12 {
                continue;
            }
            for sign in [1., -1.] {
                let h = HalfSpace {
                    origin: a,
                    normal: scale(n, sign / size),
                };
                // For r = I + t(P-I), t>=1: H(P)>=0 and H(I)<=0
                // imply H(r)>=0 over every source point and portal point.
                if portal.iter().all(|p| h.signed(*p) >= -BEAM_EPS)
                    && images.iter().all(|p| h.signed(*p) <= BEAM_EPS)
                {
                    // Widen away from both numerical boundaries. A constraint
                    // is used only after the affine enclosure inequalities hold
                    // without a sign tolerance for the complete portal/domain.
                    // Near-plane or straddling source domains retain work.
                    if depth > BEAM_EPS {
                        let widened = add(
                            h.normal,
                            scale(face.normal, side.unwrap() * 4. * BEAM_EPS / depth),
                        );
                        let normal = scale(widened, 1. / norm(widened));
                        let h = HalfSpace {
                            origin: sub(a, scale(normal, BEAM_EPS)),
                            normal,
                        };
                        if portal.iter().all(|p| h.signed(*p) >= 0.)
                            && images.iter().all(|p| h.signed(*p) <= 0.)
                        {
                            planes.push(h);
                        }
                    }
                    break;
                }
            }
        }
    }
    let point_images = parent.point_images.as_ref().map(|previous| {
        let mut chain = previous.clone();
        chain.push(images[0]);
        chain
    });
    Some(Beam {
        images,
        planes,
        point_images,
        material: std::array::from_fn(|b| parent.material[b] * (1. - face.absorption[b])),
        active: parent.active,
    })
}

/// A contiguous range of receiver indices whose connections one work item
/// evaluates. Tracing (beam construction and prefix pruning) always uses the
/// full receiver set, so it is identical in every block; only the receivers
/// a node connects to are restricted.
pub struct ReceiverBlock {
    pub range: std::ops::Range<usize>,
    /// Index over the block's receivers (local indices); `None` for all.
    local: Option<VolumeIndex>,
}
impl ReceiverBlock {
    pub fn new(scene: &Scene, range: std::ops::Range<usize>) -> Self {
        let local = (range != (0..scene.receivers.len())).then(|| {
            VolumeIndex::new(
                scene.receivers[range.clone()]
                    .iter()
                    .map(|r| std::slice::from_ref(&r.point)),
            )
        });
        Self { range, local }
    }
    pub fn whole(&self) -> bool {
        self.local.is_none()
    }
    fn candidates(&self, index: &SearchIndex, planes: &[HalfSpace]) -> Vec<usize> {
        match &self.local {
            None => index.receivers.candidates(planes),
            Some(local) => {
                let start = self.range.start;
                local
                    .candidates(planes)
                    .into_iter()
                    .map(|i| i + start)
                    .collect()
            }
        }
    }
}

/// One node of a source's reflection tree: the faces reflected so far and the
/// beam they bound. Expanding a node never depends on other nodes, so nodes
/// can be evaluated in any order or in parallel.
pub struct Node {
    pub family: Family,
    beam: Beam,
}
impl Node {
    pub fn root(source: &Source) -> Self {
        let images = source_envelope(source);
        let point_images = (source.kind == "point").then(|| images.clone());
        Self {
            family: vec![],
            beam: Beam {
                images,
                planes: vec![],
                point_images,
                material: [1.; 8],
                active: [true; 8],
            },
        }
    }
}

/// The result of expanding one node for one receiver block.
pub struct Expansion {
    pub receiver_stubs: Vec<ReceiverStub>,
    pub children: Vec<Node>,
    pub stats: SearchStats,
}

/// Connect `node` to the receivers of `block` and construct its child beams.
/// With `only_order`, only nodes with exactly that many reflections connect
/// (shorter prefixes still expand). Tracing statistics that do not depend on
/// receivers (`checked`, `empty`, `reflector_candidates`, `prefix_pruned`) are
/// filled in; callers that trace the same node for several blocks count them
/// once. `empty` means no receiver of the whole set lies in the beam volume.
#[allow(clippy::too_many_arguments)]
pub fn expand(
    scene: &Scene,
    source: &Source,
    index: &SearchIndex,
    block: &ReceiverBlock,
    node: &Node,
    only_order: Option<usize>,
    cutoffs: &Cutoffs,
    cancel: &AtomicBool,
) -> Result<Expansion, Diagnostic> {
    check_cancel(cancel)?;
    let (family, beam) = (&node.family, &node.beam);
    let mut stats = SearchStats {
        checked: 1,
        ..SearchStats::default()
    };
    let mut receiver_stubs = vec![];
    // Staged evaluation connects receivers only at the requested order.
    let connect = only_order.is_none_or(|k| family.len() == k);
    let deepest = only_order.map_or(scene.settings.order, |k| k.min(scene.settings.order));
    let receivers = if connect {
        block.candidates(index, &beam.planes)
    } else {
        vec![]
    };
    // Empty: no receiver anywhere lies in the beam (before exact geometry
    // tests), so the count is the same however receivers are divided.
    if !connect || (receivers.is_empty() && (block.whole() || !index.receivers.any(&beam.planes))) {
        stats.empty = 1;
    }
    for ri in receivers {
        check_cancel(cancel)?;
        stats.receiver_candidates += 1;
        let receiver = &scene.receivers[ri];
        let mut failure = None;
        let reference = if source.kind == "point" {
            match prepare_reference_images(
                scene,
                source.zones[0].cells[0].centroid(),
                receiver.point,
                family,
                beam.point_images.as_ref().unwrap(),
            ) {
                Ok(Some(reference)) => Some(reference),
                Ok(None) => {
                    stats.geometry_rejected += 1;
                    continue;
                }
                // A numerical failure of one connection's reference is that
                // pathway's failure, reported at the receiver, not the job's.
                Err(error) if crate::solver::pathway_error(&error.code) => {
                    failure = Some(error);
                    None
                }
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        receiver_stubs.push(ReceiverStub {
            receiver: ri,
            reference,
            failure,
        });
    }
    let mut children = vec![];
    // A terminal stub queries receivers ONLY. No next-face query, even if
    // the beam misses all receivers. Empty earlier beams may still turn
    // towards a receiver after a further allowed reflection.
    if family.len() < deepest {
        for fi in index.faces.candidates(&beam.planes).into_iter().rev() {
            check_cancel(cancel)?;
            let face = &scene.faces[fi];
            if !face.reflects {
                continue;
            }
            if family.last().is_some_and(|last| {
                let previous = &scene.faces[*last];
                norm(cross(previous.normal, face.normal)) < 1e-10
                    && previous.signed(face.origin).abs() < 1e-8
            }) {
                continue;
            }
            stats.reflector_candidates += 1;
            if let Some(mut child) = extend(beam, face) {
                if pruning(cutoffs) {
                    let reach = reach(&beam.images, face, scene, index);
                    child.active = prefix_active(scene, cutoffs, source, &child, face, reach);
                    if !child.active.iter().any(|a| *a) {
                        stats.prefix_pruned += 1;
                        continue;
                    }
                }
                let mut next = family.clone();
                next.push(fi);
                children.push(Node {
                    family: next,
                    beam: child,
                });
            }
        }
    }
    Ok(Expansion {
        receiver_stubs,
        children,
        stats,
    })
}

/// Deterministic order of families: by reflection count, then face indices.
pub fn family_order(a: &Family, b: &Family) -> std::cmp::Ordering {
    a.len().cmp(&b.len()).then(a.cmp(b))
}

pub fn search(
    scene: &Scene,
    source: &Source,
    index: &SearchIndex,
    cancel: &AtomicBool,
) -> Result<Search, Diagnostic> {
    search_with(scene, source, index, None, &scene.settings.cutoffs, cancel)
}

/// Complete search of one source over every receiver, collecting all stubs.
/// The production solver expands nodes incrementally instead (see `expand`).
pub fn search_with(
    scene: &Scene,
    source: &Source,
    index: &SearchIndex,
    only_order: Option<usize>,
    cutoffs: &Cutoffs,
    cancel: &AtomicBool,
) -> Result<Search, Diagnostic> {
    let mut result = Search {
        stubs: vec![],
        stats: SearchStats::default(),
    };
    if scene.receivers.is_empty() {
        return Ok(result);
    }
    let block = ReceiverBlock::new(scene, 0..scene.receivers.len());
    let mut pending = vec![Node::root(source)];
    while let Some(node) = pending.pop() {
        let expansion = expand(
            scene, source, index, &block, &node, only_order, cutoffs, cancel,
        )?;
        let stats = &mut result.stats;
        stats.checked += expansion.stats.checked;
        stats.empty += expansion.stats.empty;
        stats.receiver_candidates += expansion.stats.receiver_candidates;
        stats.geometry_rejected += expansion.stats.geometry_rejected;
        stats.reflector_candidates += expansion.stats.reflector_candidates;
        stats.prefix_pruned += expansion.stats.prefix_pruned;
        pending.extend(expansion.children);
        if !expansion.receiver_stubs.is_empty() {
            result.stubs.push(Stub {
                family: node.family,
                receiver_stubs: expansion.receiver_stubs,
            });
        }
    }
    // Deterministic breadth/order reduction even though discovery uses DFS.
    result
        .stubs
        .sort_by(|a, b| family_order(&a.family, &b.family));
    Ok(result)
}
