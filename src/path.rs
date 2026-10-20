#![allow(clippy::collapsible_if, clippy::needless_range_loop)]
#![allow(clippy::type_complexity, clippy::too_many_arguments)]
//! Ordered image families, shared reference-ground context and whole-route screening.
use crate::{acoustics, geometry::*, scene::*, types::*};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
pub type Family = Vec<usize>;

#[path = "joint_contact.rs"]
pub(crate) mod joint_contact;
#[cfg(test)]
#[allow(dead_code)]
#[path = "legacy_contacts.rs"]
mod legacy_contacts;

// Engineering joint-contact enclosure tolerance, not an ISO-prescribed value.
// Finite-face and specular-angle predicates retain their own tolerances.
const CONTACT_ENCLOSURE_TOLERANCE_M: f64 = 0.01;
#[derive(Clone, Debug)]
pub struct Route {
    pub points: Vec<Point>,
    pub reflections: Vec<(usize, usize)>,
    pub diffraction: Vec<usize>,
    pub sections: Vec<usize>,
    pub kinds: Vec<u8>,
    pub vertical: Bands,
    pub lateral: Bands,
    pub terrain_support: bool,
}
pub struct Transfer {
    pub energy: Bands,
    /// Received-energy upper bounds for point bands skipped before screening.
    pub omitted: Bands,
    pub ledgers: Vec<Value>,
    /// Unit-energy bound on route alternatives an incomplete joint search may
    /// have missed: the family cap minus the capped energy of found routes.
    pub unresolved: Bands,
    pub unresolved_message: Option<String>,
}
pub struct PreparedReference {
    points: Vec<Point>,
    contacts: Vec<(usize, usize)>,
    mask: [bool; 8],
}

/// Strict absolute cutoff: equality and exact silence survive.
pub fn below_cutoff(cutoffs: &Cutoffs, band: usize, energy: f64) -> bool {
    energy > 0. && cutoffs[band].is_some_and(|c| 10. * energy.log10() < c)
}

pub fn face_accepts_endpoint(face: &Face, point: Point) -> bool {
    let d = face.signed(point);
    // Leave boundary tolerances to the exact image construction.
    match face.side.as_str() {
        "front" => d >= -1e-9,
        "back" => d <= 1e-9,
        _ => true,
    }
}

/// A plane is affine over every point/line/triangle cell. Reject only when
/// every emitting vertex is on its forbidden side, never from a centroid.
pub fn source_face_visibility(scene: &Scene, source: &Source) -> Vec<bool> {
    scene
        .faces
        .iter()
        .map(|face| {
            face.reflects
                && source.zones.iter().any(|zone| {
                    zone.cells.iter().any(|cell| {
                        cell.vertices()
                            .iter()
                            .any(|p| face_accepts_endpoint(face, *p))
                    })
                })
        })
        .collect()
}

pub fn prepare_reference(
    scene: &Scene,
    s: Point,
    r: Point,
    family: &Family,
) -> Result<Option<PreparedReference>, Diagnostic> {
    let Some(points) = reference(scene, s, r, family)? else {
        return Ok(None);
    };
    finish_reference(scene, points, family)
}
pub fn prepare_reference_images(
    scene: &Scene,
    s: Point,
    r: Point,
    family: &Family,
    images: &[Point],
) -> Result<Option<PreparedReference>, Diagnostic> {
    let Some(points) = reference_images(scene, s, r, family, images)? else {
        return Ok(None);
    };
    finish_reference(scene, points, family)
}
fn finish_reference(
    scene: &Scene,
    points: Vec<Point>,
    family: &Family,
) -> Result<Option<PreparedReference>, Diagnostic> {
    let contacts = family
        .iter()
        .enumerate()
        .map(|(i, f)| (i + 1, *f))
        .collect::<Vec<_>>();
    let mask = reflection_mask(scene, &points, &contacts)?;
    if mask.iter().all(|v| !*v) {
        return Ok(None);
    }
    Ok(Some(PreparedReference {
        points,
        contacts,
        mask,
    }))
}
pub struct Work<'a> {
    pub cancel: &'a AtomicBool,
    pub operations: &'a AtomicUsize,
}
impl Work<'_> {
    pub fn tick(&self) -> Result<(), Diagnostic> {
        check_cancel(self.cancel)?;
        self.operations.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    /// Run `f` with its own operation counter, merged once at the end, so
    /// parallel tasks never contend for one counter's cache line.
    pub fn isolated<T>(&self, f: impl FnOnce(&Work) -> T) -> T {
        let operations = AtomicUsize::new(0);
        let result = f(&Work {
            cancel: self.cancel,
            operations: &operations,
        });
        self.operations
            .fetch_add(operations.load(Ordering::Relaxed), Ordering::Relaxed);
        result
    }
    /// `tick` into a caller-owned count, for tight loops; the caller adds it
    /// to `operations` once, so the reported total is unchanged.
    pub fn tick_into(&self, pending: &std::cell::Cell<usize>) -> Result<(), Diagnostic> {
        self.ticks_into(pending, 1)
    }
    /// `count` ticks with one cancellation check.
    pub fn ticks_into(
        &self,
        pending: &std::cell::Cell<usize>,
        count: usize,
    ) -> Result<(), Diagnostic> {
        check_cancel(self.cancel)?;
        pending.set(pending.get() + count);
        Ok(())
    }
}
/// Exhaustive comparison enumerator. Production discovery uses beam::search.
pub fn families(scene: &Scene, cancel: &AtomicBool) -> Result<Vec<Family>, Diagnostic> {
    check_cancel(cancel)?;
    let faces: Vec<_> = scene
        .faces
        .iter()
        .enumerate()
        .filter(|(_, f)| f.reflects)
        .map(|(i, _)| i)
        .collect();
    let mut all: Vec<Family> = vec![vec![]];
    let (mut start, mut end) = (0, 1);
    for _ in 0..scene.settings.order {
        for parent in start..end {
            check_cancel(cancel)?;
            for i in &faces {
                check_cancel(cancel)?;
                let prefix = &all[parent];
                if prefix.last().is_some_and(|j| {
                    let a = &scene.faces[*j];
                    let b = &scene.faces[*i];
                    norm(cross(a.normal, b.normal)) < 1e-10 && a.signed(b.origin).abs() < 1e-8
                }) {
                    continue;
                }
                let mut family = prefix.clone();
                family.push(*i);
                all.push(family);
            }
        }
        start = end;
        end = all.len();
    }
    Ok(all)
}
pub fn reference(
    scene: &Scene,
    s: Point,
    r: Point,
    family: &Family,
) -> Result<Option<Vec<Point>>, Diagnostic> {
    let mut images = vec![s];
    for i in family {
        images.push(scene.faces[*i].mirror(*images.last().unwrap()))
    }
    reference_images(scene, s, r, family, &images)
}
fn reference_images(
    scene: &Scene,
    s: Point,
    r: Point,
    family: &Family,
    images: &[Point],
) -> Result<Option<Vec<Point>>, Diagnostic> {
    let mut current = r;
    let mut contacts = vec![];
    for k in (0..family.len()).rev() {
        let face = &scene.faces[family[k]];
        let image = images[k + 1];
        let a = face.signed(current);
        let b = face.signed(image);
        let den = a - b;
        if den.abs() < 1e-12 {
            return Ok(None);
        }
        let t = a / den;
        if t <= 1e-12 || t >= 1. - 1e-12 {
            return Ok(None);
        }
        let p = lerp(current, image, t);
        if p.iter().any(|x| !x.is_finite()) {
            return Err(Diagnostic::numerical("Image contact failed"));
        }
        contacts.push(p);
        current = p
    }
    contacts.reverse();
    let mut route = vec![s];
    route.extend(contacts);
    route.push(r);
    for k in 0..family.len() {
        let f = &scene.faces[family[k]];
        let a = f.signed(route[k]);
        let b = f.signed(route[k + 2]);
        if a * b <= 0. || a.abs() < 1e-9 || b.abs() < 1e-9 {
            return Ok(None);
        }
        if f.side == "front" && (a < 0. || b < 0.) || f.side == "back" && (a > 0. || b > 0.) {
            return Ok(None);
        }
    }
    Ok(Some(route))
}
fn length(p: &[Point]) -> f64 {
    p.windows(2).map(|w| distance(w[0], w[1])).sum()
}
fn horizontal(a: Point, b: Point) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}
fn cumulative(points: &[Point]) -> Vec<f64> {
    let mut s = vec![0.];
    for w in points.windows(2) {
        s.push(s.last().unwrap() + horizontal(w[0], w[1]))
    }
    s
}
fn ground(scene: &Scene, reference: &[Point]) -> Result<(Bands, f64), Diagnostic> {
    let s = reference[0];
    let r = *reference.last().unwrap();
    let hs = (s[2] - scene.terrain.elevation(s[0], s[1])?).max(0.);
    let hr = (r[2] - scene.terrain.elevation(r[0], r[1])?).max(0.);
    let distance = length(reference);
    let trace = cumulative(reference);
    let dp = *trace.last().unwrap();
    let mean_height = scene.settings.ground_method == "mean_path_height";
    let mut portions = vec![];
    let mut area = Sum::default();
    let mut base_length = Sum::default();
    for (i, w) in reference.windows(2).enumerate() {
        let l = horizontal(w[0], w[1]);
        // Only equation (14) uses the terrain integral and ground-base length.
        // The general method (11)-(13) uses endpoint heights and ground regions.
        if mean_height {
            let profile = scene.terrain.profile(w[0], w[1])?;
            let g0 = scene.terrain.elevation(w[0][0], w[0][1])?;
            let g1 = scene.terrain.elevation(w[1][0], w[1][1])?;
            base_length.add(l.hypot(g1 - g0));
            for w2 in profile.windows(2) {
                let (ta, ga) = w2[0];
                let (tb, gb) = w2[1];
                area.add(
                    l * (tb - ta)
                        * 0.5
                        * ((lerp(w[0], w[1], ta)[2] - ga) + (lerp(w[0], w[1], tb)[2] - gb)),
                );
            }
            continue;
        }
        let mut ts = vec![0., 1.];
        for ri in scene.region_index.candidates(w[0], w[1]) {
            let region = &scene.regions[ri];
            for j in 0..region.polygon.len() {
                if let Some((t, _)) = segment_cross(
                    w[0],
                    w[1],
                    region.polygon[j],
                    region.polygon[(j + 1) % region.polygon.len()],
                ) {
                    ts.push(t)
                }
            }
        }
        ts.sort_by(f64::total_cmp);
        ts.dedup_by(|a, b| (*a - *b).abs() < 1e-12);
        for t in ts.windows(2) {
            portions.push((
                trace[i] + t[0] * l,
                trace[i] + t[1] * l,
                scene.ground_at(lerp(w[0], w[1], (t[0] + t[1]) * 0.5)),
            ))
        }
    }
    if mean_height {
        if base_length.total() <= 0. {
            return Err(Diagnostic::unsupported(
                "Mean-height ground reference has zero ground-base length",
            ));
        }
        let g = acoustics::mean_height_ground(
            area.total() / base_length.total(),
            distance,
            dp,
            hs,
            hr,
        )?;
        return Ok((g.attenuation, g.reflection_gain));
    }
    let average = |lo: f64, hi: f64, endpoint: Point| {
        if hi - lo <= 1e-12 {
            return scene.ground_at(endpoint);
        }
        let mut sum = Sum::default();
        for (a, b, g) in &portions {
            sum.add((hi.min(*b) - lo.max(*a)).max(0.) * g)
        }
        sum.total() / (hi - lo)
    };
    let gs = average(0., (30. * hs).min(dp), s);
    let gr = average((dp - 30. * hr).max(0.), dp, r);
    let gm = if dp > 30. * (hs + hr) {
        average(30. * hs, dp - 30. * hr, s)
    } else {
        scene.ground_at(s)
    };
    Ok((acoustics::general_ground(hs, hr, dp, gs, gm, gr)?, 0.))
}
/// Whole-object plan width perpendicular to the horizontal leg a-b.
fn outline_width(verts: &[Point], a: Point, b: Point) -> f64 {
    let h = horizontal(a, b);
    if h == 0. {
        return 0.;
    }
    let n = [-(b[1] - a[1]) / h, (b[0] - a[0]) / h, 0.];
    let (lo, hi) = verts
        .iter()
        .map(|p| dot(*p, n))
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), x| {
            (lo.min(x), hi.max(x))
        });
    hi - lo
}
fn width(scene: &Scene, fi: usize, a: Point, b: Point) -> f64 {
    // Buildings use their footprint; a bent barrier is one physical screen.
    let verts = scene
        .face_outline
        .get(fi)
        .map_or(&scene.faces[fi].vertices, |i| &scene.outlines[*i]);
    outline_width(verts, a, b)
}
pub(crate) fn screen_eligible(scene: &Scene, fi: usize, a: Point, b: Point, band: usize) -> bool {
    exceeds_wavelength(width(scene, fi, a, b), band)
}
/// Every face of a building shares the footprint width, so one test decides
/// eligibility for the whole solid.
fn building_screen_eligible(scene: &Scene, bi: usize, a: Point, b: Point, band: usize) -> bool {
    !scene.buildings[bi].faces.is_empty()
        && exceeds_wavelength(outline_width(&scene.outlines[bi], a, b), band)
}

// Frequency enters route construction only through `size > wavelength`
// eligibility tests. While one band's routes are constructed, record the
// contiguous band range for which every evaluated test has the same outcome.
// Every band in that range repeats the identical computation, so its routes
// may be shared. Recording is per thread: route construction runs
// synchronously and never yields its thread to other rayon work.
thread_local! {
    static BAND_RANGE: std::cell::Cell<Option<(usize, usize)>> =
        const { std::cell::Cell::new(None) };
}

fn wavelength(band: usize) -> f64 {
    340. / acoustics::FREQUENCIES[band]
}

/// Frequency-dependent obstacle eligibility: a screen's size must exceed the
/// nominal octave wavelength.
pub(crate) fn exceeds_wavelength(size: f64, band: usize) -> bool {
    let result = size > wavelength(band);
    BAND_RANGE.with(|range| {
        if let Some((lo, hi)) = range.get() {
            // Wavelength decreases with band: the test holds from `first` up.
            let first = (0..8).find(|b| size > wavelength(*b)).unwrap_or(8);
            range.set(Some(if result {
                (lo.max(first), hi)
            } else {
                (lo, hi.min(first - 1))
            }));
        }
    });
    result
}

/// Run `f` for `band`, returning the inclusive band range that reproduces it.
fn with_band_range<T>(band: usize, f: impl FnOnce() -> T) -> (T, (usize, usize)) {
    let outer = BAND_RANGE.replace(Some((0, 7)));
    let value = f();
    let (lo, hi) = BAND_RANGE.get().expect("band range recording is active");
    BAND_RANGE.set(outer.map(|(a, b)| (a.max(lo), b.min(hi))));
    debug_assert!(lo <= band && band <= hi);
    (value, (lo, hi))
}
fn crossing_top(face: &Face, a: Point, b: Point) -> Option<(f64, f64)> {
    if !face.vertical() {
        return None;
    }
    let da = face.signed(a);
    let db = face.signed(b);
    let den = da - db;
    if den.abs() < 1e-12 {
        return None;
    }
    let t = da / den;
    if !(0.0..=1.).contains(&t) {
        return None;
    }
    let p = lerp(a, b, t);
    vertical_interval(face, p).map(|(_, top)| (t, top))
}

/// Finite vertical extent at this plan position, including sloping upper and
/// lower edges. Global Z bounds are not the height at a reflection contact.
pub(crate) fn vertical_interval(face: &Face, p: Point) -> Option<(f64, f64)> {
    let mut zs = vec![];
    for i in 0..face.vertices.len() {
        let q = face.vertices[i];
        let r = face.vertices[(i + 1) % face.vertices.len()];
        let dx = r[0] - q[0];
        let dy = r[1] - q[1];
        let len = dx * dx + dy * dy;
        if len <= 1e-20 {
            if horizontal(p, q) < 1e-7 {
                zs.extend([q[2], r[2]])
            }
        } else {
            let u = ((p[0] - q[0]) * dx + (p[1] - q[1]) * dy) / len;
            if (-1e-10..=1. + 1e-10).contains(&u) && horizontal(p, lerp(q, r, u)) < 1e-7 {
                zs.push(q[2] + u * (r[2] - q[2]))
            }
        }
    }
    if zs.is_empty() {
        None
    } else {
        Some(
            zs.into_iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), z| {
                    (lo.min(z), hi.max(z))
                }),
        )
    }
}
pub fn ray_blocked(
    scene: &Scene,
    a: Point,
    b: Point,
    skip: &[usize],
    band: usize,
) -> Result<bool, Diagnostic> {
    ray_blocked_candidates(
        scene,
        a,
        b,
        skip,
        band,
        scene.building_index.candidates(a, b),
    )
}

fn ray_blocked_candidates(
    scene: &Scene,
    a: Point,
    b: Point,
    skip: &[usize],
    band: usize,
    buildings: impl IntoIterator<Item = usize>,
) -> Result<bool, Diagnostic> {
    for i in scene.face_index.candidates(a, b) {
        let f = &scene.faces[i];
        if skip.contains(&i) || !screen_eligible(scene, i, a, b, band) {
            continue;
        }
        let da = f.signed(a);
        let db = f.signed(b);
        if da * db < -1e-14 {
            let t = da / (da - db);
            // Diffraction/reflection contacts are finite-face boundaries, not
            // crossings of the open leg. Plane-dot roundoff can put a contact
            // infinitesimally on the far side. Use the same metre endpoint
            // tolerance as route/support construction, independently of length.
            let leg = distance(a, b);
            if t * leg <= 1e-8 || (1. - t) * leg <= 1e-8 {
                continue;
            }
            let p = lerp(a, b, t);
            if f.contains(p) {
                return Ok(true);
            }
            if f.anchored {
                if let Some((_, top)) = crossing_top(f, a, b) {
                    if p[2] < top - 1e-8 && p[2] >= scene.terrain.elevation(p[0], p[1])? - 1e-8 {
                        return Ok(true);
                    }
                }
            }
        }
    }
    for bi in buildings {
        let bld = &scene.buildings[bi];
        if !building_screen_eligible(scene, bi, a, b, band) {
            continue;
        }
        for t in [0.25, 0.5, 0.75] {
            let p = lerp(a, b, t);
            if point_in_polygon_xy(p, &bld.base)
                && bld.top(p).is_some_and(|z| p[2] < z - 1e-7)
                && !bld.faces.iter().any(|i| scene.faces[*i].contains(p))
            {
                return Ok(true);
            }
        }
    }
    if scene.terrain.below_segment(a, b) {
        return Ok(false);
    }
    for (t, g) in scene.terrain.profile(a, b)? {
        if lerp(a, b, t)[2] < g - 1e-7 {
            return Ok(true);
        }
    }
    Ok(false)
}
#[derive(Clone, Copy)]
struct Support {
    s: f64,
    z: f64,
    terrain: bool,
}
fn upper_hull(mut p: Vec<Support>) -> Vec<Support> {
    p.sort_by(|a, b| a.s.total_cmp(&b.s).then(b.z.total_cmp(&a.z)));
    // Coincident supports are one profile station at the highest of them.
    // Ties are not always exact: keeping the first could let a terrain sample
    // a picometre earlier hide a screen top at the same station.
    p.dedup_by(|a, b| {
        let same = (a.s - b.s).abs() < 1e-9;
        if same && a.z > b.z {
            *b = *a;
        }
        same
    });
    let mut hull: Vec<Support> = vec![];
    for q in p {
        while hull.len() >= 2 {
            let a = hull[hull.len() - 2];
            let b = hull[hull.len() - 1];
            let cross = (b.s - a.s) * (q.z - b.z) - (b.z - a.z) * (q.s - b.s);
            // Compare the middle-point excess height in metres. A fixed
            // cross-product tolerance scales with neither leg length nor the
            // roundoff of a projected terrain profile and can retain a point
            // inside a planar facet as a false diffraction edge.
            if cross >= -(q.s - a.s) * 1e-8 {
                hull.pop();
            } else {
                break;
            }
        }
        hull.push(q)
    }
    hull
}
#[cfg(test)]
mod profile_hull_tests {
    use super::*;

    #[test]
    fn planar_profile_roundoff_is_not_a_diffraction_vertex() {
        // Affine facet heights: the middle sample has a 3 pm excess from
        // interpolation at projected coordinates. It is not a physical edge.
        let samples = vec![
            Support {
                s: 0.,
                z: 214.56531707240129,
                terrain: true,
            },
            Support {
                s: 117.15257397885551,
                z: 215.442571724132,
                terrain: true,
            },
            Support {
                s: 126.42936688379827,
                z: 215.5120376304473,
                terrain: true,
            },
        ];
        for offset in [0., 600_000.] {
            let mut profile = samples.clone();
            for p in &mut profile {
                p.s += offset;
            }
            assert_eq!(upper_hull(profile.clone()).len(), 2);
            profile[1].z += 1e-6;
            assert_eq!(upper_hull(profile).len(), 3);
        }
    }

    #[test]
    fn band_range_records_every_eligibility_outcome() {
        // 3 m exceeds the 125 Hz wavelength (2.72 m) but not 63 Hz (5.40 m).
        let ((), range) = with_band_range(0, || assert!(!exceeds_wavelength(3., 0)));
        assert_eq!(range, (0, 0));
        let ((), range) = with_band_range(4, || {
            assert!(exceeds_wavelength(3., 4));
            assert!(exceeds_wavelength(10., 4));
            // 0.5 m exceeds only the 1000 Hz and higher wavelengths.
            assert!(exceeds_wavelength(0.5, 4));
        });
        assert_eq!(range, (4, 7));
        // 0.1 m exceeds only the 4000 and 8000 Hz wavelengths.
        let ((), range) = with_band_range(5, || assert!(!exceeds_wavelength(0.1, 5)));
        assert_eq!(range, (0, 5));
        // Recording is scoped: outside it the test only answers.
        assert!(exceeds_wavelength(3., 1));
        assert_eq!(BAND_RANGE.get(), None);
    }

    #[test]
    fn nearly_coincident_supports_keep_the_highest() {
        // A flat terrain sample a picometre before a screen-top crossing must
        // not evict the screen support from the profile.
        let support = |s, z, terrain| Support { s, z, terrain };
        let hull = upper_hull(vec![
            support(0., 1., false),
            support(50. - 1e-12, 0., true),
            support(50., 6., false),
            support(100., 1., false),
        ]);
        assert_eq!(hull.len(), 3);
        assert_eq!(hull[1].z, 6.);
        assert!(!hull[1].terrain);
    }
}

fn at_trace(points: &[Point], trace: &[f64], s: f64) -> Point {
    let i = trace
        .partition_point(|x| *x <= s)
        .saturating_sub(1)
        .min(points.len() - 2);
    let d = trace[i + 1] - trace[i];
    lerp(
        points[i],
        points[i + 1],
        if d == 0. {
            0.
        } else {
            ((s - trace[i]) / d).clamp(0., 1.)
        },
    )
}
fn height_at(hull: &[Support], s: f64) -> f64 {
    let i = hull
        .partition_point(|p| p.s <= s)
        .saturating_sub(1)
        .min(hull.len() - 2);
    let a = hull[i];
    let b = hull[i + 1];
    a.z + (b.z - a.z) * (s - a.s) / (b.s - a.s)
}
fn build_route(
    scene: &Scene,
    skeleton: &[Point],
    bounce_indices: &[(usize, usize)],
    sections: &[usize],
    kinds: &[u8],
    band: usize,
) -> Result<Option<Route>, Diagnostic> {
    profile_route(scene, skeleton, bounce_indices, sections, kinds, band, true)
}

/// Discover finite screening supports even when the provisional contacts do not
/// yet form a clear ray. This proposal is never accepted as a propagation route.
fn profile_route(
    scene: &Scene,
    skeleton: &[Point],
    bounce_indices: &[(usize, usize)],
    sections: &[usize],
    kinds: &[u8],
    band: usize,
    validate: bool,
) -> Result<Option<Route>, Diagnostic> {
    let trace = cumulative(skeleton);
    let total = *trace.last().unwrap();
    if total <= 1e-10 {
        if skeleton.len() != 2 {
            return Err(Diagnostic::unsupported(
                "Vertically aligned reflected reference",
            ));
        }
        if ray_blocked(scene, skeleton[0], skeleton[1], &[], band)? {
            return Err(Diagnostic::unsupported(
                "Vertical obstructed propagation has no defined lateral/profile geometry",
            ));
        }
        return Ok(Some(Route {
            points: skeleton.to_vec(),
            reflections: vec![],
            diffraction: vec![],
            sections: vec![0, 1],
            kinds: vec![0],
            vertical: [0.; 8],
            lateral: [0.; 8],
            terrain_support: false,
        }));
    }
    let mut supports = vec![];
    let mut near = vec![];
    for (i, w) in skeleton.windows(2).enumerate() {
        let section = sections
            .partition_point(|n| *n <= i)
            .saturating_sub(1)
            .min(kinds.len() - 1);
        if kinds[section] != 0 {
            continue;
        }
        let l = trace[i + 1] - trace[i];
        for (t, z) in scene.terrain.profile(w[0], w[1])? {
            supports.push(Support {
                s: trace[i] + l * t,
                z,
                terrain: true,
            })
        }
        for fi in scene.face_index.candidates(w[0], w[1]) {
            let f = &scene.faces[fi];
            if !screen_eligible(scene, fi, w[0], w[1], band) {
                continue;
            }
            if bounce_indices
                .iter()
                .any(|(idx, face)| *face == fi && (*idx == i || *idx == i + 1))
            {
                continue;
            }
            if f.solid
                && (((f.contains(w[0])
                    || (!validate
                        && f.vertical()
                        && f.signed(w[0]).abs() <= 1e-7
                        && vertical_interval(f, w[0]).is_some()))
                    && dot(f.normal, sub(w[1], w[0])) >= -1e-10)
                    || ((f.contains(w[1])
                        || (!validate
                            && f.vertical()
                            && f.signed(w[1]).abs() <= 1e-7
                            && vertical_interval(f, w[1]).is_some()))
                        && dot(f.normal, sub(w[0], w[1])) >= -1e-10))
            {
                continue;
            }
            if let Some((t, z)) = crossing_top(f, w[0], w[1]) {
                let p = lerp(w[0], w[1], t);
                let bottom = f
                    .vertices
                    .iter()
                    .map(|q| q[2])
                    .fold(f64::INFINITY, f64::min);
                if !f.anchored && p[2] < bottom - 1e-8 {
                    continue;
                }
                let q = Support {
                    s: trace[i] + l * t,
                    z,
                    terrain: false,
                };
                supports.push(q);
                near.push(q)
            }
        }
        // Roof facets can create controlling ridges between footprint entry and exit.
        // XY broad phase: a roof edge crossing needs footprint overlap.
        for bi in scene.building_index.candidates(w[0], w[1]) {
            let b = &scene.buildings[bi];
            if !building_screen_eligible(scene, bi, w[0], w[1], band) {
                continue;
            }
            for tri in &b.roof_triangles {
                for k in 0..3 {
                    let a = b.roof[tri[k]];
                    let c = b.roof[tri[(k + 1) % 3]];
                    if let Some((t, u)) = segment_cross(w[0], w[1], a, c) {
                        // Test endpoint proximity in metres, not as a fraction
                        // of leg length. A short ray at projected GIS coordinates
                        // can round its wall contact just inside the roof edge.
                        if (t * l <= 1e-8
                            && b.faces.iter().any(|fi| {
                                let f = &scene.faces[*fi];
                                (f.contains(w[0])
                                    || (!validate
                                        && f.vertical()
                                        && f.signed(w[0]).abs() <= 1e-7
                                        && vertical_interval(f, w[0]).is_some()))
                                    && dot(f.normal, sub(w[1], w[0])) >= -1e-10
                            }))
                            || ((1. - t) * l <= 1e-8
                                && b.faces.iter().any(|fi| {
                                    let f = &scene.faces[*fi];
                                    (f.contains(w[1])
                                        || (!validate
                                            && f.vertical()
                                            && f.signed(w[1]).abs() <= 1e-7
                                            && vertical_interval(f, w[1]).is_some()))
                                        && dot(f.normal, sub(w[0], w[1])) >= -1e-10
                                }))
                        {
                            continue;
                        }
                        let q = Support {
                            s: trace[i] + l * t,
                            z: a[2] + u * (c[2] - a[2]),
                            terrain: false,
                        };
                        supports.push(q);
                        near.push(q)
                    }
                }
            }
        }
    }
    let start = skeleton[0];
    let end = *skeleton.last().unwrap();
    let z0 = supports
        .iter()
        .filter(|p| p.s <= 1e-9)
        .map(|p| p.z)
        .fold(start[2], f64::max);
    let z1 = supports
        .iter()
        .filter(|p| p.s >= total - 1e-9)
        .map(|p| p.z)
        .fold(end[2], f64::max);
    supports.retain(|p| p.s > 1e-9 && p.s < total - 1e-9);
    supports.push(Support {
        s: 0.,
        z: z0,
        terrain: false,
    });
    supports.push(Support {
        s: total,
        z: z1,
        terrain: false,
    });
    let mut anchors = vec![Support {
        s: 0.,
        z: z0,
        terrain: false,
    }];
    for (i, p) in skeleton.iter().enumerate().skip(1).take(skeleton.len() - 2) {
        if !bounce_indices.iter().any(|(j, _)| *j == i) {
            anchors.push(Support {
                s: trace[i],
                z: p[2],
                terrain: false,
            })
        }
    }
    anchors.push(Support {
        s: total,
        z: z1,
        terrain: false,
    });
    let mut hull = vec![];
    for pair in anchors.windows(2) {
        let mut local: Vec<_> = supports
            .iter()
            .filter(|p| p.s > pair[0].s + 1e-9 && p.s < pair[1].s - 1e-9)
            .copied()
            .collect();
        local.extend([pair[0], pair[1]]);
        let mut h = upper_hull(local);
        if !hull.is_empty() {
            h.remove(0);
        }
        hull.extend(h)
    }
    let terrain_support = hull
        .iter()
        .skip(1)
        .take(hull.len().saturating_sub(2))
        .any(|p| p.terrain && p.z > start[2] + (end[2] - start[2]) * p.s / total + 1e-8);
    let mut events: Vec<(f64, Option<usize>, bool)> = trace
        .iter()
        .enumerate()
        .map(|(i, s)| (*s, Some(i), false))
        .collect();
    for p in &hull {
        if p.s > 1e-9 && p.s < total - 1e-9 {
            events.push((p.s, None, true))
        }
    }
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut points = vec![];
    let mut map = vec![0; skeleton.len()];
    let mut diffraction = vec![];
    if z0 > start[2] + 1e-8 {
        points.push(start);
        diffraction.push(1)
    }
    for (s, original, diff) in events {
        let mut p = at_trace(skeleton, &trace, s);
        p[2] = height_at(&hull, s);
        let index = if points.last().is_some_and(|q| distance(*q, p) < 1e-8) {
            points.len() - 1
        } else {
            points.push(p);
            points.len() - 1
        };
        if let Some(i) = original {
            map[i] = index;
            let is_ref = bounce_indices.iter().any(|(j, _)| *j == i);
            if i > 0 && i + 1 < skeleton.len() && !is_ref {
                diffraction.push(index)
            }
        }
        if diff {
            diffraction.push(index)
        }
    }
    if z1 > end[2] + 1e-8 {
        diffraction.push(points.len() - 1);
        points.push(end)
    }
    let reflections: Vec<_> = bounce_indices
        .iter()
        .map(|(i, fi)| (map[*i], *fi))
        .collect();
    if validate {
        for (idx, fi) in &reflections {
            if !scene.faces[*fi].contains(points[*idx]) {
                return Ok(None);
            }
        }
        for (i, w) in points.windows(2).enumerate() {
            let skip: Vec<_> = reflections
                .iter()
                .filter(|(j, _)| *j == i || *j == i + 1)
                .map(|(_, f)| *f)
                .collect();
            if ray_blocked(scene, w[0], w[1], &skip, band)? {
                return Ok(None);
            }
        }
    }
    diffraction.sort_unstable();
    diffraction.dedup();
    diffraction.retain(|i| *i > 0 && *i + 1 < points.len());
    let section_points: Vec<_> = sections.iter().map(|i| map[*i]).collect();
    let mut route = Route {
        points,
        reflections,
        diffraction,
        sections: section_points,
        kinds: kinds.to_vec(),
        vertical: [0.; 8],
        lateral: [0.; 8],
        terrain_support,
    };
    if !validate {
        return Ok(Some(route));
    }
    if kinds.iter().all(|k| *k == 0) {
        let (v, _) = route_loss(
            &route.points,
            &route.diffraction,
            false,
            length(skeleton),
            &near,
            0.,
            total,
        )?;
        route.vertical = v
    } else {
        for i in 0..kinds.len() {
            let begin = route.sections[i];
            let finish = route.sections[i + 1];
            if finish <= begin {
                continue;
            }
            let p = &route.points[begin..=finish];
            let d: Vec<_> = route
                .diffraction
                .iter()
                .filter(|j| **j > begin && **j < finish)
                .map(|j| *j - begin)
                .collect();
            let refd = distance(p[0], *p.last().unwrap());
            let (loss, _) = route_loss(
                p,
                &d,
                kinds[i] != 0,
                refd,
                &near,
                trace[sections[i]],
                trace[sections[i + 1]],
            )?;
            for b in 0..8 {
                if kinds[i] == 0 {
                    route.vertical[b] += loss[b]
                } else {
                    route.lateral[b] += loss[b]
                }
            }
        }
    }
    Ok(Some(route))
}
fn route_loss(
    points: &[Point],
    edges: &[usize],
    lateral: bool,
    reference: f64,
    near: &[Support],
    lo: f64,
    hi: f64,
) -> Result<(Bands, f64), Diagnostic> {
    let path = length(points);
    if !edges.is_empty() {
        let first = edges[0];
        let last = *edges.last().unwrap();
        let ds = length(&points[..=first]);
        let dr = length(&points[last..]);
        let e = if first == last {
            0.
        } else {
            length(&points[first..=last])
        };
        let delta = (path - reference).max(0.);
        return Ok((
            acoustics::diffraction(delta, ds, e, dr, reference, lateral)?,
            delta,
        ));
    }
    if lateral {
        return Ok(([0.; 8], 0.));
    }
    let mut best: Option<(f64, f64, f64)> = None;
    let span = hi - lo;
    for p in near.iter().filter(|p| p.s > lo + 1e-9 && p.s < hi - 1e-9) {
        let t = (p.s - lo) / span;
        let zline = points[0][2] + (points.last().unwrap()[2] - points[0][2]) * t;
        if p.z > zline + 1e-7 {
            continue;
        }
        let ds = ((p.s - lo).powi(2) + (p.z - points[0][2]).powi(2)).sqrt();
        let dr = ((hi - p.s).powi(2) + (points.last().unwrap()[2] - p.z).powi(2)).sqrt();
        let delta = -(ds + dr - reference).max(0.);
        if best.is_none_or(|(d, _, _)| delta > d) {
            best = Some((delta, ds, dr))
        }
    }
    if let Some((delta, ds, dr)) = best {
        Ok((
            acoustics::diffraction(delta, ds, 0., dr, reference, false)?,
            delta,
        ))
    } else {
        Ok(([0.; 8], 0.))
    }
}
fn specular_direction_matches(face: &Face, before: Point, contact: Point, after: Point) -> bool {
    let incoming = sub(contact, before);
    let outgoing = sub(after, contact);
    let reflected = sub(
        incoming,
        scale(face.normal, 2. * dot(incoming, face.normal)),
    );
    norm(cross(
        scale(reflected, 1. / norm(reflected)),
        scale(outgoing, 1. / norm(outgoing)),
    )) <= 1e-6
        && dot(reflected, outgoing) > 0.
}

/// A small contact update alone cannot certify reflection. An incompatible
/// coincident diffraction support is left to the established rejection rule.
#[cfg(test)]
fn reflection_directions_settled(scene: &Scene, route: &Route) -> bool {
    route.reflections.iter().all(|(at, fi)| {
        (route.diffraction.contains(at) && scene.faces[*fi].vertical())
            || specular_direction_matches(
                &scene.faces[*fi],
                route.points[at - 1],
                route.points[*at],
                route.points[at + 1],
            )
    })
}

fn completed_reflection_mask(scene: &Scene, route: &Route) -> Result<[bool; 8], Diagnostic> {
    // A top hull may pin a mandatory wall reflection to a roof/terrain edge.
    // If that edge requires a kink inconsistent with reflection, this particular
    // completed route cannot satisfy both constraints. Keep other alternatives;
    // ordinary angle failures without a coincident support remain diagnostics.
    for (at, fi) in &route.reflections {
        if route.diffraction.contains(at)
            && scene.faces[*fi].vertical()
            && !specular_direction_matches(
                &scene.faces[*fi],
                route.points[at - 1],
                route.points[*at],
                route.points[at + 1],
            )
        {
            return Ok([false; 8]);
        }
    }
    reflection_mask(scene, &route.points, &route.reflections)
}

fn reflection_mask(
    scene: &Scene,
    points: &[Point],
    contacts: &[(usize, usize)],
) -> Result<[bool; 8], Diagnostic> {
    let mut mask = [true; 8];
    let total = length(points);
    for (index, fi) in contacts {
        let f = &scene.faces[*fi];
        let p = points[*index];
        if !f.contains(p) {
            return Ok([false; 8]);
        }
        let incoming = sub(p, points[index - 1]);
        if !specular_direction_matches(f, points[index - 1], p, points[index + 1]) {
            return Err(Diagnostic::numerical(
                "Completed reflection does not satisfy specular geometry",
            ));
        }
        let ds = length(&points[..=*index]);
        let dr = total - ds;
        let a = f
            .projected
            .iter()
            .map(|q| q[0])
            .fold(f64::NEG_INFINITY, f64::max)
            - f.projected
                .iter()
                .map(|q| q[0])
                .fold(f64::INFINITY, f64::min);
        let h = f
            .projected
            .iter()
            .map(|q| q[1])
            .fold(f64::NEG_INFINITY, f64::max)
            - f.projected
                .iter()
                .map(|q| q[1])
                .fold(f64::INFINITY, f64::min);
        let vn = dot(incoming, f.normal).abs();
        let effective = if f.vertical() {
            let w = f
                .vertices
                .iter()
                .map(|q| dot(*q, [f.normal[1], -f.normal[0], 0.]))
                .fold(f64::NEG_INFINITY, f64::max)
                - f.vertices
                    .iter()
                    .map(|q| dot(*q, [f.normal[1], -f.normal[0], 0.]))
                    .fold(f64::INFINITY, f64::min);
            let (bottom, top) = vertical_interval(f, p).ok_or_else(|| {
                Diagnostic::numerical("Reflector has no vertical extent at its contact")
            })?;
            // ISO 9613-2:2024 7.5.2 (27): h is the extension at the contact.
            // Buried portions of ground-anchored walls cannot reflect sound.
            let bottom = if f.anchored {
                scene.terrain.elevation(p[0], p[1])?
            } else {
                bottom
            };
            let vertical = (top - bottom).max(0.);
            (w * vn / incoming[0].hypot(incoming[1])).min(vertical * vn / vn.hypot(incoming[2]))
        } else {
            let u = dot(incoming, f.u);
            let v = dot(incoming, f.v);
            (a * vn / vn.hypot(u)).min(h * vn / vn.hypot(v))
        };
        for (b, m) in mask.iter_mut().enumerate() {
            *m &= f.absorption[b] < 1. && acoustics::reflector_eligible(b, effective, ds, dr)?
        }
    }
    Ok(mask)
}
fn host_allows(scene: &Scene, zone: &Zone, points: &[Point]) -> Result<bool, Diagnostic> {
    let s = points[0];
    let d = points
        .iter()
        .skip(1)
        .find(|p| distance(**p, s) > 1e-10)
        .map(|p| sub(*p, s))
        .ok_or_else(|| Diagnostic::numerical("Route has no physical launch segment"))?;
    let faces: Vec<_> = if let Some(i) = zone.host {
        vec![&scene.faces[i]]
    } else {
        scene
            .face_index
            // Face::contains admits 1e-7 m off-plane; keep a wider margin.
            .candidates_within(s, s, 1e-6)
            .into_iter()
            .map(|i| &scene.faces[i])
            .filter(|f| f.solid && f.contains(s))
            .collect()
    };
    if faces.len() > 1
        && faces
            .iter()
            .any(|f| norm(cross(f.normal, faces[0].normal)) > 1e-8)
    {
        return Err(Diagnostic::invalid(
            "Ambiguous source contact between building face planes",
        ));
    }
    Ok(faces.iter().all(|f| dot(f.normal, d) >= -1e-10))
}
fn raw_route(points: Vec<Point>, family: &Family) -> Route {
    let sections = (0..points.len()).collect();
    Route {
        reflections: family
            .iter()
            .enumerate()
            .map(|(i, f)| (i + 1, *f))
            .collect(),
        points,
        diffraction: vec![],
        sections,
        kinds: vec![0; family.len() + 1],
        vertical: [0.; 8],
        lateral: [0.; 8],
        terrain_support: false,
    }
}
fn resolve_choices(
    scene: &Scene,
    s: Point,
    r: Point,
    family: &Family,
    choices: &[Vec<Point>],
) -> Result<Option<(Vec<Point>, Vec<(usize, usize)>, Vec<usize>)>, Diagnostic> {
    // Fixed diffraction anchors separate runs of mandatory reflection planes.
    let mut anchors = vec![(s, 0usize)];
    for (section, p) in choices.iter().enumerate() {
        for q in p.iter().skip(1).take(p.len().saturating_sub(2)) {
            anchors.push((*q, section))
        }
    }
    anchors.push((r, family.len()));
    let mut points = vec![s];
    let mut contacts = vec![];
    let mut section_indices = vec![0; family.len() + 2];
    let mut next_face = 0;
    for w in anchors.windows(2) {
        let stop = w[1].1;
        let faces = family[next_face..stop].to_vec();
        let Some(path) = reference(scene, w[0].0, w[1].0, &faces)? else {
            return Ok(None);
        };
        for (j, p) in path.iter().enumerate().skip(1) {
            points.push(*p);
            if j <= faces.len() {
                contacts.push((points.len() - 1, faces[j - 1]));
                section_indices[next_face + j] = points.len() - 1
            }
        }
        next_face = stop;
    }
    section_indices[family.len() + 1] = points.len() - 1;
    Ok(Some((points, contacts, section_indices)))
}
/// Joint mixed routes, or the incomplete-search diagnostic message when the
/// search could not finish. Routes found before that point are not retained.
fn mixed_routes(
    scene: &Scene,
    s: Point,
    r: Point,
    family: &Family,
    reference: &[Point],
    band: usize,
    top: &Option<Route>,
    cache: &mut joint_contact::RootCache,
    work: &Work,
) -> Result<(Vec<Route>, Option<String>), Diagnostic> {
    // Test-only triggers for pathway handling; never compiled into builds.
    #[cfg(test)]
    for f in family {
        match scene.faces[*f].object.as_str() {
            "test_incomplete_contact_search" => {
                return Ok((vec![], Some("Test-only incomplete joint search".into())));
            }
            "test_failed_pathway" => {
                return Err(Diagnostic::new(
                    "calculation_failed",
                    "Test-only failed pathway",
                ));
            }
            "test_unsupported_pathway" => {
                return Err(Diagnostic::unsupported("Test-only unsupported pathway"));
            }
            _ => {}
        }
    }
    match joint_contact::routes(scene, s, r, family, reference, band, top, cache, work) {
        Ok(routes) => Ok((routes, None)),
        Err(error) if error.code == joint_contact::INCOMPLETE => Ok((vec![], Some(error.message))),
        Err(error) => Err(error),
    }
}

/// Identify a side where the route passes the source-receiver segment. A
/// courtyard escape behind an endpoint can begin on the other side of the
/// infinite line; using its first corner merges the two distinct T18 routes.
fn lateral_side(points: &[Point]) -> f64 {
    let a = points[0];
    let b = *points.last().unwrap();
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let span2 = dx * dx + dy * dy;
    let side = |p: &Point| orient(a, b, *p);
    let corners = &points[1..points.len() - 1];
    corners
        .iter()
        .filter(|p| {
            let x = (p[0] - a[0]) * dx + (p[1] - a[1]) * dy;
            (0. ..=span2).contains(&x)
        })
        .max_by(|p, q| side(p).abs().total_cmp(&side(q).abs()))
        .or_else(|| {
            corners
                .iter()
                .max_by(|p, q| side(p).abs().total_cmp(&side(q).abs()))
        })
        .map_or(1., |p| side(p).signum())
}
/// Band-independent terms of one completed route, evaluated on first use.
#[derive(Clone, Copy)]
struct Terms {
    abar: Bands,
    dc: Bands,
    misc: Bands,
}
/// A distinct completed route admitted by the source host, with its octave
/// reflection eligibility. Shared by every band of one construction range.
struct Candidate {
    route: Route,
    mask: [bool; 8],
    terms: Option<Terms>,
}
impl Candidate {
    fn terms(
        &mut self,
        scene: &Scene,
        zone: &Zone,
        s: Point,
        agr: Bands,
    ) -> Result<Terms, Diagnostic> {
        if let Some(terms) = self.terms {
            return Ok(terms);
        }
        let route = &self.route;
        let terms = Terms {
            abar: acoustics::screening_loss(route.vertical, route.lateral, agr)?,
            dc: zone
                .directivity
                .as_ref()
                .map(|dir| dir.correction(sub(route.points[1], s)))
                .unwrap_or([0.; 8]),
            misc: crate::foliage::attenuation(scene, &route.points, &route.diffraction)?,
        };
        self.terms = Some(terms);
        Ok(terms)
    }
}

/// Two constructions of one physical route can differ only by floating-point
/// roundoff in their contacts. Count such a route once.
const SAME_ROUTE_TOLERANCE_M: f64 = 1e-6;
fn same_route(a: &Route, b: &Route) -> bool {
    a.points.len() == b.points.len()
        && a.reflections.len() == b.reflections.len()
        && a.points
            .iter()
            .zip(&b.points)
            .all(|(p, q)| distance(*p, *q) <= SAME_ROUTE_TOLERANCE_M)
}

/// Every top/side route alternative of one reflection family for `band`.
/// Frequency enters only through recorded wavelength eligibility tests.
fn candidate_routes(
    scene: &Scene,
    zone: &Zone,
    s: Point,
    r: Point,
    family: &Family,
    reference: &[Point],
    reference_contacts: &[(usize, usize)],
    nonvertical: bool,
    band: usize,
    contact_roots: &mut joint_contact::RootCache,
    work: &Work,
) -> Result<(Vec<Candidate>, Option<String>), Diagnostic> {
    let top = if nonvertical {
        let mut blocked = false;
        for (i, w) in reference.windows(2).enumerate() {
            let skip: Vec<_> = reference_contacts
                .iter()
                .filter(|(n, _)| *n == i || *n == i + 1)
                .map(|(_, f)| *f)
                .collect();
            blocked |= ray_blocked(scene, w[0], w[1], &skip, band)?
        }
        if blocked {
            return Err(Diagnostic::unsupported(
                "Screening combined with a nonvertical reflection requires a separate geometry profile",
            ));
        }
        Some(raw_route(reference.to_vec(), family))
    } else {
        build_route(
            scene,
            reference,
            reference_contacts,
            &(0..reference.len()).collect::<Vec<_>>(),
            &vec![0; reference.len() - 1],
            band,
        )?
    };
    let (mut alternatives, incomplete) = if nonvertical {
        (vec![], None)
    } else {
        mixed_routes(
            scene,
            s,
            r,
            family,
            reference,
            band,
            &top,
            contact_roots,
            work,
        )?
    };
    if let Some(top) = top {
        alternatives.insert(0, top)
    }
    let mut seen = HashSet::new();
    let mut candidates: Vec<Candidate> = vec![];
    for route in alternatives {
        work.tick()?;
        let key: Vec<_> = route.points.iter().map(|p| p.map(f64::to_bits)).collect();
        if !seen.insert(key) || candidates.iter().any(|c| same_route(&c.route, &route)) {
            continue;
        }
        if !host_allows(scene, zone, &route.points)? {
            continue;
        }
        let mask = completed_reflection_mask(scene, &route)?;
        candidates.push(Candidate {
            route,
            mask,
            terms: None,
        });
    }
    Ok((candidates, incomplete))
}
pub fn transfer(
    scene: &Scene,
    source: &Source,
    zone: &Zone,
    s: Point,
    receiver: &Receiver,
    family: &Family,
    work: &Work,
) -> Result<Transfer, Diagnostic> {
    work.tick()?;
    let Some(prepared) = prepare_reference(scene, s, receiver.point, family)? else {
        return Ok(Transfer {
            energy: [0.; 8],
            omitted: [0.; 8],
            ledgers: vec![],
            unresolved: [0.; 8],
            unresolved_message: None,
        });
    };
    transfer_prepared(
        scene,
        source,
        zone,
        s,
        receiver,
        family,
        prepared,
        false,
        &scene.settings.cutoffs,
        work,
    )
}

/// `whole_point_source` permits absolute cuts on complete point families only.
/// Integration samples are not complete physical-source families.
pub fn transfer_prepared(
    scene: &Scene,
    source: &Source,
    zone: &Zone,
    s: Point,
    receiver: &Receiver,
    family: &Family,
    prepared: PreparedReference,
    whole_point_source: bool,
    cutoffs: &Cutoffs,
    work: &Work,
) -> Result<Transfer, Diagnostic> {
    let r = receiver.point;
    let PreparedReference {
        points: reference,
        contacts: reference_contacts,
        mask: refmask,
    } = prepared;
    let (agr, domega) = ground(scene, &reference)?;
    let d = length(&reference);
    let adiv = acoustics::divergence(d)?;
    let mut imagecentre = source.centre;
    for fi in family {
        if scene.settings.long_term && !scene.faces[*fi].vertical() {
            return Err(Diagnostic::unsupported(
                "Nonvertical reflected long-term meteorology is not in the supported profile",
            ));
        }
        imagecentre = scene.faces[*fi].mirror(imagecentre)
    }
    let cmet = if scene.settings.long_term {
        acoustics::meteorological(
            source.height.unwrap_or(0.),
            (r[2] - receiver.ground).max(0.),
            horizontal(imagecentre, r),
            scene.settings.c0,
        )?
    } else {
        0.
    };
    let mut material = [1.; 8];
    for fi in family {
        for b in 0..8 {
            material[b] *= 1. - scene.faces[*fi].absorption[b]
        }
    }
    let refdir = zone
        .directivity
        .as_ref()
        .map(|dir| dir.correction(sub(reference[1], s)))
        .unwrap_or([0.; 8]);
    let refmisc = crate::foliage::attenuation(scene, &reference, &[])?;
    let mut ceiling = [0.; 8];
    for b in 0..8 {
        if refmask[b] && material[b] > 0. {
            ceiling[b] = positive(
                10f64.powf(
                    (refdir[b] + domega - adiv - scene.alpha[b] * d - agr[b] - refmisc[b] - cmet)
                        / 10.,
                ) * material[b],
                "Unscreened transfer",
            )?
        }
    }
    let full = string(&scene.output, "path_diagnostics", "none") == "full";
    let summary = string(&scene.output, "path_diagnostics", "none") != "none";
    let mut energy = [0.; 8];
    let mut omitted = [0.; 8];
    let mut unresolved = [0.; 8];
    let mut unresolved_message = None;
    let mut ledgers = vec![];
    let nonvertical = family.iter().any(|i| !scene.faces[*i].vertical());
    let mut contact_roots = joint_contact::RootCache::default();
    // Route alternatives for the most recently constructed band range.
    let mut shared: Option<((usize, usize), (Vec<Candidate>, Option<String>))> = None;
    for band in 0..8 {
        check_cancel(work.cancel)?;
        if ceiling[band] == 0. || zone.power[band] == 0. {
            continue;
        }
        if whole_point_source && !nonvertical {
            // The existing complete-family cap bounds the sum of all top/side
            // alternatives, including changed diffraction launch directions.
            let upper = positive(
                ceiling[band] * zone.power[band],
                "Point family upper energy",
            )?;
            if below_cutoff(cutoffs, band, upper) {
                omitted[band] = upper;
                continue;
            }
        }
        if !shared
            .as_ref()
            .is_some_and(|((lo, hi), _)| (*lo..=*hi).contains(&band))
        {
            let (candidates, range) = with_band_range(band, || {
                candidate_routes(
                    scene,
                    zone,
                    s,
                    r,
                    family,
                    &reference,
                    &reference_contacts,
                    nonvertical,
                    band,
                    &mut contact_roots,
                    work,
                )
            });
            shared = Some((range, candidates?));
        }
        let (candidates, incomplete) = &mut shared.as_mut().expect("band routes are prepared").1;
        if let Some(message) = incomplete.as_ref() {
            // Only a complete point family has the cap that bounds missing
            // alternatives; an integration sample is part of a quadrature.
            if !whole_point_source {
                return Err(Diagnostic::new("calculation_failed", message.clone()));
            }
        }
        let mut sum = Sum::default();
        let mut route_count = 0;
        for candidate in candidates.iter_mut() {
            work.tick()?;
            if !candidate.mask[band] {
                continue;
            }
            let Terms { abar, dc, misc } = candidate.terms(scene, zone, s, agr)?;
            let route = &candidate.route;
            let level = dc[band] + domega
                - adiv
                - scene.alpha[band] * d
                - agr[band]
                - abar[band]
                - misc[band]
                - cmet;
            let e = positive(
                10f64.powf(level / 10.) * material[band],
                "Completed route transfer",
            )?;
            sum.add(e);
            route_count += 1;
            if full {
                ledgers.push(json!({"source_id":source.id,"zone_id":zone.id,"receiver_id":receiver.id,"band":band,"reflection_faces":family.iter().map(|i|scene.faces[*i].key()).collect::<Vec<_>>(),"source_position":s,"physical_route":route.points,"diffraction_vertices":route.diffraction,"ground_reference":reference,"reference_distance_m":d,"physical_route_distance_m":length(&route.points),"L_W":0.,"D_c":dc[band],"D_omega":domega,"A_div":adiv,"A_atm":scene.alpha[band]*d,"A_gr":agr[band],"A_bar":abar[band],"A_refl":-10.*material[band].log10(),"A_misc":misc[band],"C_met":cmet,"unit_route_energy":e,"unscreened_unit_energy":ceiling[band]}))
            }
        }
        let before = sum.total();
        energy[band] = before.min(ceiling[band]);
        if !energy[band].is_finite() {
            return Err(Diagnostic::numerical("Family energy overflow"));
        }
        if let Some(message) = incomplete.as_ref() {
            // The family total is capped at the unscreened reference, so
            // alternatives the search missed add at most the remainder.
            unresolved[band] = ceiling[band] - energy[band];
            unresolved_message.get_or_insert_with(|| message.clone());
        }
        if summary {
            ledgers.push(json!({"source_id":source.id,"zone_id":zone.id,"receiver_id":receiver.id,"source_position":s,"band":band,"reflection_faces":family.iter().map(|i|scene.faces[*i].key()).collect::<Vec<_>>(),"eligible_routes":route_count,"pre_cap_unit_energy":before,"post_cap_unit_energy":energy[band],"unscreened_unit_energy":ceiling[band]}))
        }
    }
    Ok(Transfer {
        energy,
        omitted,
        ledgers,
        unresolved,
        unresolved_message,
    })
}

#[cfg(test)]
mod screen_width_tests {
    use super::*;

    #[test]
    fn indexed_solid_rays_match_exhaustive_building_checks() {
        let mut buildings = vec![];
        let mut points = vec![];
        for i in 0..12 {
            let x = 500_000. + i as f64 * 20.;
            let y = 4_500_000.;
            buildings.push(json!({"id":format!("b{i}"), "footprint":[
                [x,y,0.],[x+10.,y,0.],[x+10.,y+10.,0.],[x,y+10.,0.]], "height_m":10.}));
            // Exterior, interior, roof boundary and just above the roof.
            points.extend([
                [x - 1., y + 5., 5.],
                [x + 5., y + 5., 5.],
                [x + 5., y + 5., 10.],
                [x + 5., y + 5., 10.000001],
            ]);
        }
        let scene =
            Scene::prepare(&json!({"buildings":buildings}), &AtomicBool::new(false)).unwrap();
        let all_faces: Vec<_> = (0..scene.faces.len()).collect();
        for skip in [&[][..], all_faces.as_slice()] {
            for &a in &points {
                for &b in &points {
                    for band in [0, 7] {
                        let indexed = ray_blocked(&scene, a, b, skip, band).unwrap();
                        let exhaustive = ray_blocked_candidates(
                            &scene,
                            a,
                            b,
                            skip,
                            band,
                            0..scene.buildings.len(),
                        )
                        .unwrap();
                        assert_eq!(indexed, exhaustive, "{a:?} -> {b:?}, band {band}");
                    }
                }
            }
        }
    }

    #[test]
    fn prepared_widths_keep_whole_objects_separate_at_survey_coordinates() {
        let offset = [500_000., 4_500_000., 0.];
        let point = |x, y, z| add(offset, [x, y, z]);
        let request = json!({
            "buildings":[{"id":"building", "footprint":[
                point(4.,-2.,0.), point(8.,-2.,0.),
                point(8.,2.,0.), point(4.,2.,0.)], "height_m":6.}],
            "barriers":[{"id":"bent", "vertices":[
                point(10.,-4.,0.), point(12.,1.,0.), point(10.,7.,0.)],
                "height_m":4.}],
            "reflectors":[{"id":"panel", "start":point(20.,1.,0.),
                "end":point(20.,3.,0.), "height_m":3.}]
        });
        let scene = Scene::prepare(&request, &AtomicBool::new(false)).unwrap();
        for (fi, face) in scene.faces.iter().enumerate() {
            let expected = match face.object.as_str() {
                "building" => [4., 4.],
                "bent" => [11., 2.],
                "panel" => [2., 0.],
                _ => unreachable!(),
            };
            for (axis, endpoint) in [point(30., 0., 2.), point(0., 30., 2.)]
                .into_iter()
                .enumerate()
            {
                let start = point(0., 0., 2.);
                assert_eq!(width(&scene, fi, start, endpoint), expected[axis]);
                assert_eq!(width(&scene, fi, endpoint, start), expected[axis]);
            }
        }
    }
}

#[cfg(test)]
mod ground_selection_tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn irregular_terrain_keeps_general_method_and_endpoint_ground() {
        let request = json!({
            "terrain":{"samples":[[0,0,0],[50,0,20],[100,0,10]]},
            "settings":{"ground_method":"general"}
        });
        let scene = Scene::prepare(&request, &AtomicBool::new(false)).unwrap();
        let (attenuation, gain) = ground(&scene, &[[0., 0., 2.], [100., 0., 12.]]).unwrap();
        // Hard endpoint regions: -1.5 dB each; dp=100 < 30*(hs+hr)=120.
        // Equations (11)-(13): k=100^2/(100^2+4^2), applied in energy.
        let expected = -10. * (1. + (10000. / 10016.) * (10_f64.powf(0.3) - 1.)).log10();
        for value in attenuation {
            assert!((value - expected).abs() < 1e-12);
        }
        assert_eq!(gain, 0.);
        assert_eq!(scene.terrain.elevation(50., 0.).unwrap(), 20.);
    }

    #[test]
    fn irregular_terrain_does_not_gate_long_term_or_mean_height_preparation() {
        for method in ["general", "mean_path_height"] {
            let request = json!({
                "terrain":{"samples":[[0,0,0],[50,0,10],[100,0,0]]},
                "settings":{"ground_method":method,"meteorological_mode":"long_term","c0":3}
            });
            let scene = Scene::prepare(&request, &AtomicBool::new(false)).unwrap();
            assert_eq!(scene.settings.ground_method, method);
            assert!(scene.settings.long_term);
            let (attenuation, gain) = ground(&scene, &[[0., 0., 20.], [100., 0., 20.]]).unwrap();
            assert!(attenuation.iter().all(|v| v.is_finite()));
            assert!(gain.is_finite());
            // Signed mean reference height: (100*20 - 100*10/2)/100 = 15 m.
            if method == "mean_path_height" {
                // Equation 14 yields max(0, 4.8 - .3*20) = 0 dB.
                assert_eq!(attenuation, [0.; 8]);
                assert!(gain > 0.);
            }
        }
    }
}

#[cfg(test)]
mod work_accounting_tests {
    use super::*;

    #[test]
    fn passes_former_operation_ceiling_and_still_cancels() {
        let cancel = AtomicBool::new(false);
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        for _ in 0..1_000_001 {
            work.tick().unwrap();
        }
        assert_eq!(operations.load(Ordering::Relaxed), 1_000_001);
        cancel.store(true, Ordering::Relaxed);
        assert!(work.tick().is_err());
        assert_eq!(operations.load(Ordering::Relaxed), 1_000_001);
    }
}

#[cfg(test)]
mod roof_contact_tests {
    use super::*;

    #[test]
    fn reflection_eligibility_uses_height_at_contact() {
        let scene = Scene::prepare(
            &json!({"reflectors":[{
                "id":"sloping-bottom", "vertices":[[0,0,0],[10,0,10],[10,0,12],[0,0,12]]
            }]}),
            &AtomicBool::new(false),
        )
        .unwrap();
        let points = [[0., -10., 10.], [5., 0., 10.], [10., -10., 10.]];
        // Local height is 12-5=7 m; ds=dr=sqrt(125) m. The 63 Hz
        // Fresnel threshold sqrt((340/63)*sqrt(125)) exceeds 7 m,
        // while the 125 Hz threshold is smaller. Overall height 12 is wrong.
        assert_eq!(
            vertical_interval(&scene.faces[0], points[1]),
            Some((5., 12.))
        );
        assert_eq!(
            reflection_mask(&scene, &points, &[(1, 0)]).unwrap(),
            [false, true, true, true, true, true, true, true]
        );
    }

    #[test]
    fn courtyard_lateral_escape_is_visible_in_final_route() {
        let scene = Scene::prepare(&json!({"buildings":[{"id":"H","footprint":[[36.83,7.19,0],[54.15,17.19,0],[29.15,60.49,0],[-5.49,40.49,0],[9.51,14.51,0],[18.17,19.51,0],[8.17,36.83,0],[25.49,46.83,0],[40.49,20.85,0],[31.83,15.85,0]],"height_m":10}]}), &AtomicBool::new(false)).unwrap();
        let s = [15., 35., 2.];
        let r = [44.64, 63.66, 4.];
        let plane = |x: f64, y: f64| {
            [
                x,
                y,
                2. + 2. * ((x - 15.) * 29.64 + (y - 35.) * 28.66)
                    / (29.64_f64.powi(2) + 28.66_f64.powi(2)),
            ]
        };
        let path = [s, plane(36.83, 7.19), plane(54.15, 17.19), r];
        for w in path.windows(2) {
            assert!(
                !ray_blocked(&scene, w[0], w[1], &[], 3).unwrap(),
                "blocked {w:?}"
            );
        }
        assert!(
            build_route(&scene, &path, &[], &[0, 3], &[1], 3)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn building_entry_distinguishes_exterior_contact() {
        // Supplementary geometry regression, not an ISO reference case, for
        // the closed-solid test shared by lateral visibility. `ray_blocked`
        // still uses per-face containment plus three samples and reports the
        // first two legs below as blocked (a known limitation).
        let scene = Scene::prepare(
            &json!({"buildings":[{"id":"B","footprint":[[0,0,0],[4,0,0],[4,4,0],[0,4,0]],"height_m":9}]}),
            &AtomicBool::new(false),
        )
        .unwrap();
        let building = &scene.buildings[0];
        let enters = |a: Point, b: Point| crate::lateral::enters_building(a, b, building).unwrap();
        // Along the roof at roof height, leaving across the top of a wall.
        assert!(!enters([4., 1., 9.], [-10., 3., 9.]));
        assert!(ray_blocked(&scene, [4., 1., 9.], [-10., 3., 9.], &[], 3).unwrap());
        // Along a wall face, touching the far corner; through a corner only.
        assert!(!enters([-5., 0., 2.], [10., 0., 1.]));
        assert!(!enters([-1., 1., 2.], [1., -1., 2.]));
        // Just below the roof across the footprint; an ordinary wall crossing.
        assert!(enters([-100., 2., 9.05], [100., 2., 8.95]));
        assert!(enters([-5., 2., 2.], [10., 2., 1.]));
    }

    #[test]
    fn oblique_barrier_top_contact_is_not_an_interior_obstruction() {
        // ISO/TR 17534-3:2015 T08, Tables 1 and 19. The intersection of
        // the source-receiver vertical plane with the top edge is a valid
        // diffraction support; neither open leg crosses the screen interior.
        let scene = Scene::prepare(
            &json!({"barriers":[{
                "id":"B", "start":[100,240,0], "end":[265,-180,0],
                "height_m":6, "closed":true, "surface_density_kg_m2":10
            }]}),
            &AtomicBool::new(false),
        )
        .unwrap();
        let s = [10., 10., 1.];
        let r = [200., 50., 4.];
        let face = &scene.faces[0];
        let (t, z) = crossing_top(face, s, r).unwrap();
        let mut support = lerp(s, r, t);
        support[2] = z;
        assert!(
            !ray_blocked(&scene, s, support, &[], 3).unwrap(),
            "incoming contact {support:?}: signed distances {}, {}",
            face.signed(s),
            face.signed(support)
        );
        assert!(
            !ray_blocked(&scene, support, r, &[], 3).unwrap(),
            "outgoing contact {support:?}: signed distances {}, {}",
            face.signed(support),
            face.signed(r)
        );
        assert!(
            build_route(&scene, &[s, r], &[], &[0, 1], &[0], 3)
                .unwrap()
                .is_some()
        );
        assert!(ray_blocked(&scene, s, r, &[], 3).unwrap());
    }

    #[test]
    fn exterior_wall_reflection_does_not_screen_itself_at_gis_coordinates() {
        // Supplementary surveyed-coordinate regression, not an ISO reference case.
        // The source and receiver are outside the same planar wall. Its roof
        // cannot obstruct either leg of their single specular reflection.
        for offset in [[0., 0., 0.], [600686., 300366., 0.]] {
            let shift = |p: Point| sub(p, offset);
            let footprint = [
                [600706.4479501825, 300366.6291063794, 211.836],
                [600686.0275930744, 300366.858083026, 211.836],
                [600686.1728394546, 300379.81129462447, 211.836],
                [600706.5931965628, 300379.58231797785, 211.836],
            ];
            let request = json!({"terrain":{"construction_profile":"acalc_schema11"},"buildings":[{
                "id":"host", "ground_vertices":footprint.map(|p| json!({"position":shift(p)})),
                "roof":{"mode":"average","height_m":4.2672},
                "roof_triangles":[[1,0,3],[3,2,1]], "absorption":([0.1;8])
            }]});
            let scene = Scene::prepare(&request, &AtomicBool::new(false)).unwrap();
            let fi = scene.faces.iter().position(|f| f.id == "wall:1:0").unwrap();
            let s = shift([600691.0842310847, 300366.55752633914, 213.05519390398783]);
            let r = shift([600499.5317156321, 299907.698424595, 223.1878885790606]);
            assert!(scene.faces[fi].signed(s) > 0.24);
            assert!(scene.faces[fi].signed(r) > 400.);
            let reference = reference(&scene, s, r, &vec![fi]).unwrap().unwrap();
            let contacts = [(1, fi)];
            assert_eq!(
                reflection_mask(&scene, &reference, &contacts).unwrap(),
                [true; 8]
            );
            for band in 0..8 {
                let route = build_route(&scene, &reference, &contacts, &[0, 1, 2], &[0, 0], band)
                    .unwrap()
                    .unwrap();
                assert!(
                    route.diffraction.is_empty(),
                    "roof introduced a false diffraction: {:?}",
                    route.points
                );
                assert_eq!(route.points.len(), 3);
                for (actual, expected) in route.points.iter().zip(&reference) {
                    assert!(distance(*actual, *expected) < 1e-8);
                }
                assert_eq!(
                    reflection_mask(&scene, &route.points, &route.reflections).unwrap(),
                    [true; 8]
                );
            }
        }
    }

    #[test]
    fn roof_ridge_inside_a_crossed_building_still_screens() {
        let request = json!({"terrain":{"construction_profile":"acalc_schema11"},"buildings":[{
            "id":"ridge", "ground_vertices":[
                {"position":[0,0,0]}, {"position":[10,0,0]},
                {"position":[10,10,0]}, {"position":[0,10,0]}
            ], "roof":{"mode":"individual","heights_m":[5,5,8,5]},
            "roof_triangles":[[0,1,2],[0,2,3]]
        }]});
        let scene = Scene::prepare(&request, &AtomicBool::new(false)).unwrap();
        let route = build_route(
            &scene,
            &[[-1., 5., 2.], [11., 5., 2.]],
            &[],
            &[0, 1],
            &[0],
            0,
        )
        .unwrap()
        .unwrap();
        assert!(!route.diffraction.is_empty());
        assert!(
            route
                .points
                .iter()
                .any(|p| (p[0] - 5.).abs() < 1e-8 && (p[2] - 6.5).abs() < 1e-8)
        );
    }
}

#[cfg(test)]
mod reflection_family_count_tests {
    use super::*;

    #[test]
    fn second_order_exceeds_former_family_limit_without_truncation() {
        let cancel = AtomicBool::new(false);
        let mut scene =
            Scene::prepare(&json!({"settings":{"max_reflect_order":2}}), &cancel).unwrap();
        for i in 0..101 {
            let x = i as f64;
            scene.faces.push(
                Face::new(
                    &format!("wall-{i}"),
                    "wall",
                    vec![[x, 0., 0.], [x, 10., 0.], [x, 10., 10.], [x, 0., 10.]],
                    [0.1; 8],
                    true,
                    "both",
                    false,
                    false,
                )
                .unwrap(),
            );
        }
        let all = families(&scene, &cancel).unwrap();
        // Direct + 101 single reflections + every ordered distinct pair.
        assert_eq!(all.len(), 1 + 101 + 101 * 100);
        assert_eq!(all.iter().filter(|f| f.len() == 2).count(), 10100);
        let unique: HashSet<_> = all.iter().collect();
        assert_eq!(unique.len(), all.len());
        assert!(all.iter().all(|f| f.len() < 2 || f[0] != f[1]));
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(families(&scene, &cancel).unwrap_err().code, "cancelled");
    }
}
