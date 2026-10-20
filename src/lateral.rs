#![allow(clippy::collapsible_if, clippy::manual_contains)]
//! ISO 9613-2:2024 7.4.3 lateral planes and at most two convex corner chains.
//! This module constructs geometry; the caller excludes terrain-supported top
//! paths and calculates diffraction once over each complete returned chain.

use crate::geometry::{distance, dot, lerp, sub};
use crate::path::{Work, exceeds_wavelength};
use crate::scene::{Building, Face, Scene};
use crate::types::{Diagnostic, Point};
use robust::{Coord, orient2d};
use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

const HEIGHT_EPS: f64 = 1e-7;

#[derive(Clone, Copy)]
struct Frame {
    start: Point,
    end: Point,
    along: [f64; 2],
    normal: [f64; 2],
    span: f64,
}
impl Frame {
    fn within_leg(&self, p: Point) -> bool {
        let x = self.coordinates(p)[0];
        x >= -HEIGHT_EPS && x <= self.span + HEIGHT_EPS
    }
    fn coordinates(&self, p: Point) -> Point {
        let x = p[0] - self.start[0];
        let y = p[1] - self.start[1];
        [
            x * self.along[0] + y * self.along[1],
            x * self.normal[0] + y * self.normal[1],
            0.0,
        ]
    }
    fn on_plane(&self, mut p: Point) -> Point {
        let x = self.coordinates(p)[0];
        p[2] = self.start[2] + (self.end[2] - self.start[2]) * x / self.span;
        p
    }
    fn width(&self, vertices: &[Point]) -> f64 {
        let (lo, hi) = vertices
            .iter()
            .map(|p| self.coordinates(*p)[1])
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), y| {
                (lo.min(y), hi.max(y))
            });
        hi - lo
    }
}

/// Each result includes both endpoints, followed in source-to-receiver order.
/// `vertical_offset` is the maximum perpendicular distance of a controlling
/// top support from the 3D direct line, in the vertical propagation plane.
/// An unobstructed direct ray returns no additional lateral contributions.
pub fn candidates(
    scene: &Scene,
    start: Point,
    end: Point,
    band: usize,
    vertical_offset: f64,
    work: &Work<'_>,
) -> Result<Vec<Vec<Point>>, Diagnostic> {
    candidate_search(scene, start, end, band, vertical_offset, None, work)
}

/// Follow an already discovered physical branch during an oscillating contact
/// solve. The recut chain still passes the ordinary lateral eligibility tests.
pub fn candidates_on_supports(
    scene: &Scene,
    start: Point,
    end: Point,
    band: usize,
    vertical_offset: f64,
    previous: &[Point],
    work: &Work<'_>,
) -> Result<Vec<Vec<Point>>, Diagnostic> {
    candidate_search(
        scene,
        start,
        end,
        band,
        vertical_offset,
        Some(previous),
        work,
    )
}

fn candidate_search(
    scene: &Scene,
    start: Point,
    end: Point,
    band: usize,
    vertical_offset: f64,
    previous: Option<&[Point]>,
    work: &Work<'_>,
) -> Result<Vec<Vec<Point>>, Diagnostic> {
    work.tick()?;
    if band >= 8
        || !vertical_offset.is_finite()
        || vertical_offset < 0.0
        || start.iter().chain(end.iter()).any(|x| !x.is_finite())
    {
        return Err(Diagnostic::invalid(
            "Lateral route parameters must be finite, with a valid band and nonnegative top offset",
        ));
    }
    let dx = end[0] - start[0];
    let dy = end[1] - start[1];
    let span = dx.hypot(dy);
    if !span.is_finite() {
        return Err(Diagnostic::numerical(
            "Lateral reference distance overflowed",
        ));
    }
    if span == 0.0 {
        return Err(Diagnostic::unsupported(
            "A vertical source-receiver line has no unique ISO lateral propagation plane",
        ));
    }
    let frame = Frame {
        start,
        end,
        along: [dx / span, dy / span],
        normal: [-dy / span, dx / span],
        span,
    };
    let eligible = Eligibility::new(scene, frame, Some(band));
    let visibility = Visibility {
        scene,
        eligible: &eligible,
        work,
        pending: Cell::new(0),
    };
    if visibility.clear(start, end)? {
        return Ok(vec![]);
    }

    let corners = collect_corners(scene, &frame, &eligible, work)?;
    let limit = 8.0 * vertical_offset;
    let mut result = vec![];
    if let Some(previous) = previous {
        if let Some(chain) =
            recut_support_chain(scene, &frame, previous, &corners, limit, &visibility)?
        {
            result.push(chain);
        }
    }
    for side in [1.0, -1.0] {
        let mut nodes = vec![start, end];
        for &p in &corners {
            let y = frame.coordinates(p)[1] * side;
            if (y >= 0.0 || !frame.within_leg(p)) && y.abs() <= limit && p != start && p != end {
                nodes.push(p);
            }
        }
        if let Some(chain) = shortest_convex(&nodes, &frame, side, &visibility)? {
            // The same baseline chain cannot be counted on both sides.
            if !result.iter().any(|p| *p == chain) {
                result.push(chain);
            }
        }
    }
    Ok(result)
}

/// New plane intersections must belong to the same finite physical roof edge,
/// or the same authored vertical corner. Proximity alone cannot match branches.
pub(crate) fn shares_support(
    scene: &Scene,
    old: Point,
    new: Point,
    work: &Work,
) -> Result<bool, Diagnostic> {
    if (old[0] - new[0]).hypot(old[1] - new[1]) <= 1e-8 {
        return Ok(true);
    }
    for bi in scene.building_index.candidates(old, new) {
        work.tick()?;
        let b = &scene.buildings[bi];
        for t in &b.roof_triangles {
            for i in 0..3 {
                let a = b.roof[t[i]];
                let c = b.roof[t[(i + 1) % 3]];
                if crate::scene::point_segment_distance(old, a, c) <= 1e-8
                    && crate::scene::point_segment_distance(new, a, c) <= 1e-8
                {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn recut_support_chain(
    scene: &Scene,
    frame: &Frame,
    previous: &[Point],
    corners: &[Point],
    limit: f64,
    visibility: &Visibility,
) -> Result<Option<Vec<Point>>, Diagnostic> {
    if previous.len() < 3 {
        return Ok(None);
    }
    let mut chain = vec![frame.start];
    for old in &previous[1..previous.len() - 1] {
        let mut nearest: Option<Point> = None;
        for &new in corners {
            visibility.work.tick()?;
            if shares_support(scene, *old, new, visibility.work)?
                && nearest.is_none_or(|p| distance(*old, new) < distance(*old, p))
            {
                nearest = Some(new);
            }
        }
        let Some(new) = nearest else {
            return Ok(None);
        };
        chain.push(new);
    }
    chain.push(frame.end);
    let side = previous[1..previous.len() - 1]
        .iter()
        .map(|p| frame.coordinates(*p)[1])
        .max_by(|a, b| a.abs().total_cmp(&b.abs()))
        .unwrap()
        .signum();
    if side == 0. {
        return Ok(None);
    }
    let local: Vec<_> = chain
        .iter()
        .map(|p| {
            let mut q = frame.coordinates(*p);
            q[1] *= side;
            q
        })
        .collect();
    for i in 1..chain.len() - 1 {
        if (local[i][1] < 0. && frame.within_leg(chain[i])) || local[i][1].abs() > limit {
            return Ok(None);
        }
        let turn = orientation(local[i - 1], local[i], local[i + 1]);
        let terminal_escape = !frame.within_leg(chain[i]);
        let old = [local[i][0] - local[i - 1][0], local[i][1] - local[i - 1][1]];
        let new = [local[i + 1][0] - local[i][0], local[i + 1][1] - local[i][1]];
        if (turn > 0. && !terminal_escape)
            || (turn == 0. && old[0] * new[0] + old[1] * new[1] <= 0.)
            || (turn != 0. && !terminal_escape && new[1].atan2(new[0]) > old[1].atan2(old[0]))
        {
            return Ok(None);
        }
    }
    for leg in chain.windows(2) {
        if !visibility.clear(leg[0], leg[1])? {
            return Ok(None);
        }
    }
    Ok(Some(chain))
}

fn collect_corners(
    scene: &Scene,
    frame: &Frame,
    eligible: &Eligibility,
    work: &Work,
) -> Result<Vec<Point>, Diagnostic> {
    let mut corners = vec![];
    // A courtyard can require leaving behind the source (T18, Table 60).
    // Exclude wholly out-of-span buildings, rather than clipping individual
    // corners of a relevant solid. Its terminal escape can use a reflex edge.
    for (index, building) in scene.buildings.iter().enumerate() {
        let xs: Vec<_> = building
            .base
            .iter()
            .map(|p| frame.coordinates(*p)[0])
            .collect();
        if xs.iter().all(|x| *x < -HEIGHT_EPS) || xs.iter().all(|x| *x > frame.span + HEIGHT_EPS) {
            continue;
        }
        if !eligible.building(index) {
            continue;
        }
        for i in 0..building.base.len() {
            work.tick()?;
            let vertex = building.base[i];
            let p = frame.on_plane(vertex);
            if p[2] >= scene.terrain.elevation(p[0], p[1])? - HEIGHT_EPS
                && p[2] <= building.roof[i][2] + HEIGHT_EPS
            {
                corners.push(p);
            }
        }
        // A steep lateral plane can leave a solid through its roof before it
        // reaches the next vertical corner (ISO/TR 17534-3 Figures 20/26).
        // Those finite cross-section vertices belong to the rubber band too;
        // discarding them removes both side paths for elevated receivers.
        // Include facet edges so the same construction covers nonplanar roofs.
        for triangle in &building.roof_triangles {
            for i in 0..3 {
                work.tick()?;
                let a = building.roof[triangle[i]];
                let b = building.roof[triangle[(i + 1) % 3]];
                let da = a[2] - frame.on_plane(a)[2];
                let db = b[2] - frame.on_plane(b)[2];
                if da.abs() <= HEIGHT_EPS && db.abs() <= HEIGHT_EPS {
                    // The plane contains this complete edge within the existing
                    // boundary predicate. Its endpoint corners were added above;
                    // a quotient of tiny roundoff distances is not a new vertex.
                    continue;
                }
                if (da < 0.0 && db > 0.0) || (da > 0.0 && db < 0.0) {
                    let p = frame.on_plane(lerp(a, b, da / (da - db)));
                    if frame.within_leg(p)
                        && p[2] >= scene.terrain.elevation(p[0], p[1])? - HEIGHT_EPS
                    {
                        corners.push(p);
                    }
                }
            }
        }
    }
    // Every non-solid vertical edge can be a barrier end or polyline joint.
    // A nonvertical opaque face may obstruct a lateral ray, but does not acquire
    // artificial vertical diffraction edges in this construction.
    for (i, face) in scene.faces.iter().enumerate() {
        if face.solid || !face.vertical() || !eligible.face(i) {
            continue;
        }
        for j in 0..face.vertices.len() {
            work.tick()?;
            let a = face.vertices[j];
            let b = face.vertices[(j + 1) % face.vertices.len()];
            if a[0] != b[0] || a[1] != b[1] || a[2] == b[2] {
                continue;
            }
            let p = frame.on_plane(a);
            if !frame.within_leg(p) {
                continue;
            }
            let bottom = if face.anchored {
                scene.terrain.elevation(p[0], p[1])?
            } else {
                a[2].min(b[2])
            };
            if p[2] >= bottom - HEIGHT_EPS && p[2] <= a[2].max(b[2]) + HEIGHT_EPS {
                corners.push(p);
            }
        }
    }
    corners.sort_by(|a, b| {
        a[0].total_cmp(&b[0])
            .then(a[1].total_cmp(&b[1]))
            .then(a[2].total_cmp(&b[2]))
    });
    corners.dedup();
    Ok(corners)
}

fn orientation(a: Point, b: Point, c: Point) -> f64 {
    orient2d(
        Coord { x: a[0], y: a[1] },
        Coord { x: b[0], y: b[1] },
        Coord { x: c[0], y: c[1] },
    )
}

/// `orientation(a, b, c)` for the eight nodes from `start` where the
/// installed kernels already decide it; returns those lanes (none without
/// installed kernels). Others need `orientation`.
fn filtered_turns(
    a: Point,
    b: Point,
    xs: &[f64],
    ys: &[f64],
    start: usize,
    out: &mut [f64; 8],
) -> u8 {
    match crate::kernels::installed() {
        Some(k) => k.filtered_turns(
            a,
            b,
            xs[start..start + 8].try_into().unwrap(),
            ys[start..start + 8].try_into().unwrap(),
            out,
        ),
        None => 0,
    }
}

fn on_edge(p: Point, a: Point, b: Point) -> bool {
    orientation(a, b, p) == 0.0
        && p[0] >= a[0].min(b[0])
        && p[0] <= a[0].max(b[0])
        && p[1] >= a[1].min(b[1])
        && p[1] <= a[1].max(b[1])
}

fn interior_xy(p: Point, polygon: &[Point]) -> bool {
    let mut winding = 0_i32;
    for i in 0..polygon.len() {
        let a = polygon[i];
        let b = polygon[(i + 1) % polygon.len()];
        if on_edge(p, a, b) {
            return false;
        }
        let side = orientation(a, b, p);
        if a[1] <= p[1] && b[1] > p[1] && side > 0.0 {
            winding += 1;
        }
        if a[1] > p[1] && b[1] <= p[1] && side < 0.0 {
            winding -= 1;
        }
    }
    winding != 0
}

/// Crossing parameters along one leg; inline for ordinary footprints.
type Splits = smallvec::SmallVec<[f64; 32]>;
fn split_at_segment(a: Point, b: Point, c: Point, d: Point, ts: &mut Splits) {
    // A shared authored XY endpoint has exact parameter 0 or 1. Recomputing
    // its quotient can create a spurious tiny interior interval after rotation.
    if [c, d]
        .iter()
        .any(|p| (p[0] == a[0] && p[1] == a[1]) || (p[0] == b[0] && p[1] == b[1]))
    {
        if orientation(a, b, c) != 0.0 || orientation(a, b, d) != 0.0 {
            return;
        }
    }
    let v = sub(b, a);
    let e = sub(d, c);
    let q = sub(c, a);
    let denominator = v[0] * e[1] - v[1] * e[0];
    let push = |ts: &mut Splits, t: f64| {
        if t > 0.0 && t < 1.0 && t.is_finite() {
            ts.push(t)
        }
    };
    if denominator != 0.0 {
        let t = (q[0] * e[1] - q[1] * e[0]) / denominator;
        let u = (q[0] * v[1] - q[1] * v[0]) / denominator;
        if (0.0..=1.0).contains(&u) {
            push(ts, t)
        }
    } else if orientation(a, b, c) == 0.0 {
        let norm2 = v[0] * v[0] + v[1] * v[1];
        if norm2 > 0.0 {
            for p in [c, d] {
                push(ts, ((p[0] - a[0]) * v[0] + (p[1] - a[1]) * v[1]) / norm2)
            }
        }
    }
}

/// Whole-object wavelength eligibility in one lateral plane, evaluated on
/// first use: only objects the search reaches are tested.
struct Eligibility<'a> {
    scene: &'a Scene,
    frame: Frame,
    /// `None` treats every object as eligible.
    band: Option<usize>,
    outlines: Vec<Cell<Option<bool>>>,
}
impl<'a> Eligibility<'a> {
    fn new(scene: &'a Scene, frame: Frame, band: Option<usize>) -> Self {
        Self {
            scene,
            frame,
            band,
            outlines: vec![Cell::new(None); scene.outlines.len()],
        }
    }
    fn test(&self, vertices: &[Point]) -> bool {
        self.band
            .is_none_or(|band| exceeds_wavelength(self.frame.width(vertices), band))
    }
    fn outline(&self, index: usize) -> bool {
        if let Some(value) = self.outlines[index].get() {
            return value;
        }
        let value = self.test(&self.scene.outlines[index]);
        self.outlines[index].set(Some(value));
        value
    }
    fn building(&self, index: usize) -> bool {
        self.outline(index)
    }
    fn face(&self, index: usize) -> bool {
        match self.scene.face_outline.get(index) {
            Some(outline) => self.outline(*outline),
            None => self.test(&self.scene.faces[index].vertices),
        }
    }
}

struct Visibility<'a, 'w> {
    scene: &'a Scene,
    eligible: &'a Eligibility<'a>,
    work: &'a Work<'w>,
    /// Operations counted during this search, added to `work` on drop: one
    /// shared atomic update per search instead of one per step.
    pending: Cell<usize>,
}
impl Drop for Visibility<'_, '_> {
    fn drop(&mut self) {
        self.work
            .operations
            .fetch_add(self.pending.get(), std::sync::atomic::Ordering::Relaxed);
    }
}
impl Visibility<'_, '_> {
    fn tick(&self) -> Result<(), Diagnostic> {
        self.work.tick_into(&self.pending)
    }
    fn ticks(&self, count: usize) -> Result<(), Diagnostic> {
        self.work.ticks_into(&self.pending, count)
    }
    fn clear(&self, a: Point, b: Point) -> Result<bool, Diagnostic> {
        // Solid faces would be skipped below; their buildings are tested whole.
        let open = &self.scene.open_faces;
        let mut faces = crate::spatial::Candidates::new();
        let mut buildings = crate::spatial::Candidates::new();
        self.scene.open_face_index.candidates_into(
            a,
            b,
            crate::spatial::SEGMENT_MARGIN,
            &mut faces,
        );
        self.scene.building_index.candidates_into(
            a,
            b,
            crate::spatial::SEGMENT_MARGIN,
            &mut buildings,
        );
        self.clear_candidates(
            a,
            b,
            faces.iter().map(|i| open[*i]),
            buildings.iter().copied(),
        )
    }

    fn clear_candidates(
        &self,
        a: Point,
        b: Point,
        faces: impl IntoIterator<Item = usize>,
        buildings: impl IntoIterator<Item = usize>,
    ) -> Result<bool, Diagnostic> {
        self.tick()?;
        if a == b {
            return Ok(false);
        }
        // XY broad phase only: retain the same exact finite-face/solid tests
        // for every possible contact, including endpoint and grazing contacts.
        for i in faces {
            let face = &self.scene.faces[i];
            if face.solid || !self.eligible.face(i) {
                continue;
            }
            self.tick()?;
            // Closed solids are checked by complete footprint/facet intervals,
            // which also distinguish exterior tangency from interior launch.
            if face.vertical() && (vertical_boundary(face, a) || vertical_boundary(face, b)) {
                continue;
            }
            let da = face.signed(a);
            let db = face.signed(b);
            if (da < 0.0 && db > 0.0) || (da > 0.0 && db < 0.0) {
                let p = lerp(a, b, da / (da - db));
                let projected = [
                    dot(sub(p, face.origin), face.u),
                    dot(sub(p, face.origin), face.v),
                    0.0,
                ];
                if interior_xy(projected, &face.projected) {
                    return Ok(false);
                }
                // Anchored walls extend to the local ground, including terrain
                // below the straight chord between their supplied base points.
                if face.anchored && face.vertical() {
                    if let Some(top) = vertical_top(face, p) {
                        if p[2] < top - HEIGHT_EPS
                            && p[2] >= self.scene.terrain.elevation(p[0], p[1])? - HEIGHT_EPS
                            && !vertical_boundary(face, p)
                        {
                            return Ok(false);
                        }
                    }
                }
            }
        }
        for i in buildings {
            if self.eligible.building(i) && self.enters_building(a, b, &self.scene.buildings[i])? {
                return Ok(false);
            }
        }
        if self.scene.terrain.below_segment(a, b) {
            return Ok(true);
        }
        for (t, ground) in self.scene.terrain.profile(a, b)? {
            self.tick()?;
            if lerp(a, b, t)[2] < ground - HEIGHT_EPS {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn enters_building(&self, a: Point, b: Point, building: &Building) -> Result<bool, Diagnostic> {
        self.tick()?;
        enters_building(a, b, building)
    }
}

/// Whether the open segment enters the solid interior of a building. Grazing
/// along a wall, touching a corner or launching from the boundary is exterior
/// contact, not entry.
pub(crate) fn enters_building(a: Point, b: Point, building: &Building) -> Result<bool, Diagnostic> {
    // Built in place: an inline buffer is too large to move cheaply.
    let mut ts = Splits::new();
    ts.push(0.0);
    ts.push(1.0);
    for i in 0..building.base.len() {
        split_at_segment(
            a,
            b,
            building.base[i],
            building.base[(i + 1) % building.base.len()],
            &mut ts,
        )
    }
    for triangle in &building.roof_triangles {
        for i in 0..3 {
            split_at_segment(
                a,
                b,
                building.roof[triangle[i]],
                building.roof[triangle[(i + 1) % 3]],
                &mut ts,
            )
        }
    }
    ts.sort_by(f64::total_cmp);
    ts.dedup();
    let leg_length = distance(a, b);
    // Whether both leg ends lie on base edge i's line: independent of the
    // interval, so evaluated once per edge, on first use.
    let mut aligned: smallvec::SmallVec<[Option<bool>; 16]> =
        smallvec::smallvec![None; building.base.len()];
    let mut on_line = |i: usize, c: Point, d: Point| {
        *aligned[i].get_or_insert_with(|| {
            let edge_length = (d[0] - c[0]).hypot(d[1] - c[1]);
            // Analytically generated roof exits can be a few ulps off
            // their wall. Resolve boundary contact in metres, not an
            // exact orientation equality or a distance-scaled epsilon.
            orientation(c, d, a).abs() <= 1e-8 * edge_length
                && orientation(c, d, b).abs() <= 1e-8 * edge_length
        })
    };
    for range in ts.windows(2) {
        // Generated mirror/roof endpoints may lie a few ulps inside the
        // solid. Apply the same metre boundary-contact predicate as the
        // production finite-face ray test to endpoint intervals only.
        if range[1] * leg_length <= 1e-8 || (1. - range[0]) * leg_length <= 1e-8 {
            continue;
        }
        let middle = lerp(a, b, (range[0] + range[1]) / 2.0);
        let on_authored_boundary = (0..building.base.len()).any(|i| {
            let c = building.base[i];
            let d = building.base[(i + 1) % building.base.len()];
            on_line(i, c, d)
                && middle[0] >= c[0].min(d[0]) - 1e-8
                && middle[0] <= c[0].max(d[0]) + 1e-8
                && middle[1] >= c[1].min(d[1]) - 1e-8
                && middle[1] <= c[1].max(d[1]) + 1e-8
        });
        if on_authored_boundary {
            continue;
        }
        if !interior_xy(middle, &building.base) {
            continue;
        }
        let triangle = building
            .roof_triangles
            .iter()
            .map(|idx| idx.map(|i| building.roof[i]))
            .find(|t| inside_triangle(middle, *t))
            .ok_or_else(|| {
                Diagnostic::numerical("Building roof has no facet over a lateral interior interval")
            })?;
        // All interval endpoints lie in one roof-plane domain. Checking both
        // ends detects even an arbitrarily narrow interior crossing.
        for t in range {
            let p = lerp(a, b, *t);
            let roof = triangle_height(p, triangle)?;
            if p[2] < roof - HEIGHT_EPS {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn inside_triangle(p: Point, t: [Point; 3]) -> bool {
    let a = orientation(t[0], t[1], p);
    let b = orientation(t[1], t[2], p);
    let c = orientation(t[2], t[0], p);
    (a >= 0.0 && b >= 0.0 && c >= 0.0) || (a <= 0.0 && b <= 0.0 && c <= 0.0)
}
fn triangle_height(p: Point, t: [Point; 3]) -> Result<f64, Diagnostic> {
    let denominator = orientation(t[0], t[1], t[2]);
    if denominator == 0.0 {
        return Err(Diagnostic::numerical(
            "Degenerate roof projection in lateral clearance",
        ));
    }
    let first = orientation(p, t[1], t[2]) / denominator;
    let second = orientation(t[0], p, t[2]) / denominator;
    let z = first * t[0][2] + second * t[1][2] + (1.0 - first - second) * t[2][2];
    if z.is_finite() {
        Ok(z)
    } else {
        Err(Diagnostic::numerical("Nonfinite lateral roof clearance"))
    }
}
fn vertical_boundary(face: &Face, p: Point) -> bool {
    (0..face.vertices.len()).any(|i| {
        let a = face.vertices[i];
        let b = face.vertices[(i + 1) % face.vertices.len()];
        a[0] == b[0] && a[1] == b[1] && p[0] == a[0] && p[1] == a[1]
    })
}
fn vertical_top(face: &Face, p: Point) -> Option<f64> {
    let mut top = None;
    for i in 0..face.vertices.len() {
        let a = face.vertices[i];
        let b = face.vertices[(i + 1) % face.vertices.len()];
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        let den = dx * dx + dy * dy;
        if den == 0.0 {
            continue;
        }
        let t = ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / den;
        if (0.0..=1.0).contains(&t) {
            let q = lerp(a, b, t);
            if (p[0] - q[0]).hypot(p[1] - q[1]) < 1e-8 {
                let z = q[2];
                top = Some(top.map_or(z, |old: f64| old.max(z)))
            }
        }
    }
    top
}

#[derive(Clone, Copy)]
struct Queue {
    cost: f64,
    previous: usize,
    current: usize,
}
impl PartialEq for Queue {
    fn eq(&self, other: &Self) -> bool {
        self.cost == other.cost && self.previous == other.previous && self.current == other.current
    }
}
impl Eq for Queue {}
impl PartialOrd for Queue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Queue {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .total_cmp(&self.cost)
            .then(other.previous.cmp(&self.previous))
            .then(other.current.cmp(&self.current))
    }
}
/// Node-pair table of one search. Entries carry the search's stamp, so a new
/// search starts empty without clearing; storage is reused per thread.
/// Lookups only, never iterated, so storage cannot affect results.
struct PairTable<T: Copy> {
    n: usize,
    stamp: u32,
    cells: Vec<(u32, Option<T>)>,
}
impl<T: Copy> Default for PairTable<T> {
    fn default() -> Self {
        Self {
            n: 0,
            stamp: 0,
            cells: vec![],
        }
    }
}
impl<T: Copy> PairTable<T> {
    fn reset(&mut self, n: usize) {
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.cells.fill((0, None));
            self.stamp = 1;
        }
        if self.cells.len() < n * n {
            self.cells.resize(n * n, (0, None));
        }
        self.n = n;
    }
    fn get(&self, i: usize, j: usize) -> Option<T> {
        let (stamp, value) = self.cells[i * self.n + j];
        if stamp == self.stamp { value } else { None }
    }
    fn set(&mut self, i: usize, j: usize, value: T) {
        self.cells[i * self.n + j] = (self.stamp, Some(value));
    }
}
#[derive(Clone, Copy)]
struct State {
    cost: f64,
    parent: Option<(usize, usize)>,
}
/// Reusable working storage of `shortest_convex`.
#[derive(Default)]
struct Scratch {
    local: Vec<Point>,
    /// `local` X and Y as lanes, padded to a multiple of eight.
    xs: Vec<f64>,
    ys: Vec<f64>,
    /// Unordered connection length, and whether it is clear once tested.
    lengths: PairTable<f64>,
    clear: PairTable<bool>,
    /// Plan direction angle of the directed step i -> j.
    angles: PairTable<f64>,
    states: PairTable<State>,
    queue: BinaryHeap<Queue>,
}
/// Node tables above this size would hold too much memory per thread.
const DENSE_NODES: usize = 256;
thread_local! {
    static SCRATCH: std::cell::RefCell<Scratch> = std::cell::RefCell::default();
}

fn shortest_convex(
    nodes: &[Point],
    frame: &Frame,
    side: f64,
    visibility: &Visibility<'_, '_>,
) -> Result<Option<Vec<Point>>, Diagnostic> {
    #[cfg(test)]
    if tests::REFERENCE_SEARCH.get() {
        return tests::shortest_convex_reference(nodes, frame, side, visibility);
    }
    if nodes.len() > DENSE_NODES {
        return shortest_convex_in(&mut Scratch::default(), nodes, frame, side, visibility);
    }
    SCRATCH.with(|scratch| match scratch.try_borrow_mut() {
        Ok(mut scratch) => shortest_convex_in(&mut scratch, nodes, frame, side, visibility),
        Err(_) => shortest_convex_in(&mut Scratch::default(), nodes, frame, side, visibility),
    })
}

fn shortest_convex_in(
    scratch: &mut Scratch,
    nodes: &[Point],
    frame: &Frame,
    side: f64,
    visibility: &Visibility<'_, '_>,
) -> Result<Option<Vec<Point>>, Diagnostic> {
    let n = nodes.len();
    let Scratch {
        local,
        xs,
        ys,
        lengths,
        clear,
        angles,
        states,
        queue,
    } = scratch;
    local.clear();
    local.extend(nodes.iter().map(|p| {
        let mut q = frame.coordinates(*p);
        q[1] *= side;
        q
    }));
    xs.clear();
    ys.clear();
    xs.extend(local.iter().map(|p| p[0]));
    ys.extend(local.iter().map(|p| p[1]));
    xs.resize(n.div_ceil(8) * 8, 0.);
    ys.resize(n.div_ceil(8) * 8, 0.);
    lengths.reset(n);
    clear.reset(n);
    angles.reset(n);
    states.reset(n);
    queue.clear();
    // Atan2 of the step i -> j, shared by its use as old and new direction.
    let angle = |angles: &mut PairTable<f64>, i: usize, j: usize| {
        angles.get(i, j).unwrap_or_else(|| {
            let value = (local[j][1] - local[i][1]).atan2(local[j][0] - local[i][0]);
            angles.set(i, j, value);
            value
        })
    };
    // Distance is symmetric bit for bit: hypot ignores signs.
    let length = |lengths: &mut PairTable<f64>, i: usize, j: usize| {
        let (lo, hi) = (i.min(j), i.max(j));
        lengths.get(lo, hi).unwrap_or_else(|| {
            let value = distance(nodes[i], nodes[j]);
            lengths.set(lo, hi, value);
            value
        })
    };
    // A step's visibility is the same for every relaxation that uses it, so
    // it is tested when a state over the step is first popped, not when
    // relaxed: live states keep the same costs, parents and pop order, and
    // steps never reached are never tested.
    //
    // `bound` is the cost of a complete verified chain. A popped state whose
    // cost plus straight distance to the end exceeds it, with margin for
    // summation roundoff, lies on no chain as short as the result: it cannot
    // be on the returned chain or parent any state there, so skipping it
    // leaves the result unchanged.
    let mut bound = f64::INFINITY;
    for current in 2..n {
        let cost = distance(nodes[0], nodes[current]);
        states.set(0, current, State { cost, parent: None });
        queue.push(Queue {
            cost,
            previous: 0,
            current,
        });
    }
    while let Some(Queue {
        cost,
        previous,
        current,
    }) = queue.pop()
    {
        visibility.tick()?;
        let key = (previous, current);
        if cost > states.get(previous, current).expect("queued state").cost {
            continue;
        }
        if bound.is_finite()
            && cost + length(lengths, current, 1) * (1. - 1e-9) > bound * (1. + 1e-9) + 1e-9
        {
            continue;
        }
        let (lo, hi) = (previous.min(current), previous.max(current));
        let open = match clear.get(lo, hi) {
            Some(open) => open,
            None => {
                let open = visibility.clear(nodes[previous], nodes[current])?;
                clear.set(lo, hi, open);
                open
            }
        };
        if !open {
            continue;
        }
        if current == 1 {
            let mut ids = vec![current];
            let mut cursor = key;
            loop {
                ids.push(cursor.0);
                if let Some(parent) = states.get(cursor.0, cursor.1).expect("state").parent {
                    cursor = parent
                } else {
                    break;
                }
            }
            ids.reverse();
            if ids.len() < 3 {
                return Ok(None);
            }
            return Ok(Some(ids.into_iter().map(|i| nodes[i]).collect()));
        }
        let old = [
            local[current][0] - local[previous][0],
            local[current][1] - local[previous][1],
        ];
        let terminal_escape = !frame.within_leg(nodes[current]);
        let old_angle = if terminal_escape {
            0.0
        } else {
            angle(angles, previous, current)
        };
        visibility.ticks(n - 1)?;
        let mut turns = [0.; 8];
        let mut certain = 0u8;
        for next in 1..n {
            if next % 8 == 0 || next == 1 {
                certain = filtered_turns(
                    local[previous],
                    local[current],
                    xs,
                    ys,
                    next & !7,
                    &mut turns,
                );
            }
            if next == current || next == previous {
                continue;
            }
            let lane = next % 8;
            let turn = if certain >> lane & 1 == 1 {
                turns[lane]
            } else {
                orientation(local[previous], local[current], local[next])
            };
            if turn > 0.0 && !terminal_escape {
                continue;
            }
            if turn == 0.0 {
                let new = [
                    local[next][0] - local[current][0],
                    local[next][1] - local[current][1],
                ];
                if old[0] * new[0] + old[1] * new[1] <= 0.0 {
                    continue;
                }
            } else if !terminal_escape && angle(angles, current, next) > old_angle {
                continue;
            }
            let (lo, hi) = (current.min(next), current.max(next));
            if clear.get(lo, hi) == Some(false) {
                continue;
            }
            let total = cost + length(lengths, current, next);
            if !total.is_finite() {
                return Err(Diagnostic::numerical("Lateral path length overflowed"));
            }
            // A step to the end is tested now: if clear, it completes a chain
            // that bounds the result.
            if next == 1 {
                let open = match clear.get(lo, hi) {
                    Some(open) => open,
                    None => {
                        let open = visibility.clear(nodes[current], nodes[next])?;
                        clear.set(lo, hi, open);
                        open
                    }
                };
                if !open {
                    continue;
                }
                bound = bound.min(total);
            }
            if states.get(current, next).is_none_or(|old| total < old.cost) {
                states.set(
                    current,
                    next,
                    State {
                        cost: total,
                        parent: Some(key),
                    },
                );
                queue.push(Queue {
                    cost: total,
                    previous: current,
                    current: next,
                })
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    fn run(
        mut request: Value,
        start: Point,
        end: Point,
        offset: f64,
    ) -> Result<Vec<Vec<Point>>, Diagnostic> {
        request["sources"] = json!([{"id":"s","kind":"point","position":start,"spectrum":[90,90,90,90,90,90,90,90]}]);
        request["receivers"] = json!([{"id":"r","position":end}]);
        let cancel = AtomicBool::new(false);
        let count = AtomicUsize::new(0);
        let scene = Scene::prepare(&request, &cancel)?;
        candidates(
            &scene,
            start,
            end,
            4,
            offset,
            &Work {
                cancel: &cancel,
                operations: &count,
            },
        )
    }
    fn screen(id: &str, x: f64, left: f64, right: f64) -> Value {
        json!({"id":id,"start":[x,left,0],"end":[x,right,0],"height_m":10,"closed":true,"surface_density_kg_m2":10})
    }
    fn box_building() -> Value {
        json!({"id":"box","footprint":[[4,-2,0],[6,-2,0],[6,2,0],[4,2,0]],"height_m":10})
    }

    #[test]
    fn indexed_clearance_matches_exhaustive_scene_checks() {
        // Exercise every ordered endpoint/support chord, not just the selected
        // shortest paths: includes courtyard, high receiver, terrain, roof exits
        // and tangent/blocked connections. Eligibility is deliberately broad.
        let requests = [
            include_str!("../tests/fixtures/iso17534/T10.json"),
            include_str!("../tests/fixtures/iso17534/T12.json"),
            include_str!("../tests/fixtures/iso17534/T14.json"),
            include_str!("../tests/fixtures/iso17534/T15.json"),
            include_str!("../tests/fixtures/iso17534/T16.json"),
            include_str!("../tests/fixtures/iso17534/T18.json"),
            include_str!("../tests/fixtures/iso17534/T19.json"),
        ];
        for request in requests {
            let cancel = AtomicBool::new(false);
            let operations = AtomicUsize::new(0);
            let scene = Scene::prepare(&serde_json::from_str(request).unwrap(), &cancel).unwrap();
            let start = scene.sources[0].centre;
            let end = scene.receivers[0].point;
            let dx = end[0] - start[0];
            let dy = end[1] - start[1];
            let span = dx.hypot(dy);
            let frame = Frame {
                start,
                end,
                span,
                along: [dx / span, dy / span],
                normal: [-dy / span, dx / span],
            };
            let eligible = Eligibility::new(&scene, frame, None);
            let work = Work {
                cancel: &cancel,
                operations: &operations,
            };
            let visibility = Visibility {
                scene: &scene,
                eligible: &eligible,
                work: &work,
                pending: Cell::new(0),
            };
            let mut points = collect_corners(&scene, &frame, &eligible, &work).unwrap();
            points.extend([start, end]);
            for &a in &points {
                for &b in &points {
                    let indexed = visibility.clear(a, b).unwrap();
                    let exhaustive = visibility
                        .clear_candidates(a, b, 0..scene.faces.len(), 0..scene.buildings.len())
                        .unwrap();
                    assert_eq!(indexed, exhaustive, "{a:?} -> {b:?}");
                }
            }
        }
    }

    #[test]
    fn following_supports_rejects_blocked_shortcuts_limits_and_cancellation() {
        let cancel = AtomicBool::new(false);
        let scene=Scene::prepare(&json!({"buildings":[{"id":"b","footprint":[[4.,-2.,0.],[6.,-2.,0.],[6.,2.,0.],[4.,2.,0.]],"height_m":2.}]}),&cancel).unwrap();
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        let s = [0., 0., 1.];
        let r = [10., 0., 1.];
        let chain = vec![s, [4., 2., 1.], [6., 2., 1.], r];
        let actual = candidates_on_supports(&scene, s, r, 4, 1., &chain, &work).unwrap();
        assert_eq!(actual[0], chain);
        let shortcut = vec![s, [4., 2., 1.], r];
        let actual = candidates_on_supports(&scene, s, r, 4, 1., &shortcut, &work).unwrap();
        assert!(!actual.is_empty());
        assert!(actual.iter().all(|p| p != &shortcut));
        assert!(
            candidates_on_supports(&scene, s, r, 4, 0.1, &chain, &work)
                .unwrap()
                .is_empty()
        );
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            candidates_on_supports(&scene, s, r, 4, 1., &chain, &work)
                .unwrap_err()
                .code,
            "cancelled"
        );
    }

    #[test]
    fn diffraction_candidates_stay_between_each_legs_endpoints() {
        let cancel = AtomicBool::new(false);
        let mut buildings = vec![];
        for (id, dx) in [("before", -10.), ("between", 0.), ("after", 10.)] {
            let mut b = box_building();
            b["id"] = json!(id);
            for p in b["footprint"].as_array_mut().unwrap() {
                p[0] = json!(p[0].as_f64().unwrap() + dx);
            }
            buildings.push(b);
        }
        let scene = Scene::prepare(&json!({"buildings":buildings,
            "barriers":[screen("before-screen", -2., -3., 3.), screen("after-screen", 12., -3., 3.)]}), &cancel).unwrap();
        let operations = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &operations,
        };
        for (start, end, along) in [
            ([0., 0., 1.], [10., 0., 1.], [1., 0.]),
            ([10., 0., 1.], [0., 0., 1.], [-1., 0.]),
        ] {
            let frame = Frame {
                start,
                end,
                along,
                normal: [0., along[0]],
                span: 10.,
            };
            let actual = collect_corners(
                &scene,
                &frame,
                &Eligibility::new(&scene, frame, None),
                &work,
            )
            .unwrap();
            assert_eq!(
                actual,
                vec![[4., -2., 1.], [4., 2., 1.], [6., -2., 1.], [6., 2., 1.]]
            );
        }
    }

    #[test]
    fn single_screen_has_two_prescribed_plane_contacts() {
        let paths = run(
            json!({"barriers":[screen("wall",4.0,-2.0,3.0)]}),
            [0.0, 0.0, 1.0],
            [10.0, 0.0, 3.0],
            2.0,
        )
        .unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(
            paths[0],
            vec![[0.0, 0.0, 1.0], [4.0, 3.0, 1.8], [10.0, 0.0, 3.0]]
        );
        assert_eq!(paths[1][1], [4.0, -2.0, 1.8]);
    }
    #[test]
    fn two_screens_form_one_convex_diffraction_chain() {
        let paths = run(
            json!({"barriers":[screen("a",3.0,-2.0,2.0),screen("b",7.0,-3.0,3.0)]}),
            [0.0, 0.0, 1.0],
            [10.0, 0.0, 1.0],
            2.0,
        )
        .unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(
            paths[0],
            vec![
                [0.0, 0.0, 1.0],
                [3.0, 2.0, 1.0],
                [7.0, 3.0, 1.0],
                [10.0, 0.0, 1.0]
            ]
        );
        assert_eq!(paths[1].len(), 4);
    }
    #[test]
    fn building_routes_follow_exterior_and_never_cut_interior() {
        let paths = run(
            json!({"buildings":[box_building()]}),
            [0.0, 0.0, 1.0],
            [10.0, 0.0, 1.0],
            2.0,
        )
        .unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(
            paths[0],
            vec![
                [0.0, 0.0, 1.0],
                [4.0, 2.0, 1.0],
                [6.0, 2.0, 1.0],
                [10.0, 0.0, 1.0]
            ]
        );
    }
    #[test]
    fn elevated_receiver_plane_exits_through_roof_edges() {
        // Published T12 inputs, with the two EL/roof intersections derived
        // from z=1+0.7(x-50): x=440/7 at z=10, not the absent rear corners.
        let paths = run(
            json!({"buildings":[{"id":"H","footprint":[[55,5,0],[65,5,0],[65,15,0],[55,15,0]],"height_m":10}]}),
            [50., 10., 1.], [70., 10., 15.], 5.,
        ).unwrap();
        assert_eq!(paths.len(), 2);
        for (path, y) in paths.iter().zip([15., 5.]) {
            let expected = [
                [50., 10., 1.],
                [55., y, 4.5],
                [440. / 7., y, 10.],
                [70., 10., 15.],
            ];
            assert_eq!(path.len(), expected.len());
            for (actual, expected) in path.iter().zip(expected) {
                assert!(distance(*actual, expected) < 1e-10, "{path:?}");
            }
        }
    }
    #[test]
    fn oblique_elevated_receiver_retains_both_roof_exit_paths() {
        let paths = run(
            json!({"buildings":[{"id":"H","footprint":[[10.96,15.5,0],[12,13,0],[14.5,11.96,0],[17,13,0],[18.04,15.5,0],[17,18,0],[14.5,19.04,0],[12,18,0]],"height_m":10}]}),
            [8.,10.,1.], [25.,20.,23.], 2.,
        ).unwrap();
        assert_eq!(paths.len(), 2, "{paths:?}");
        for path in &paths {
            assert_eq!(path.len(), 4, "{path:?}");
            assert!((path[2][2] - 10.).abs() < 1e-10);
        }
    }
    #[test]
    fn courtyard_escape_can_pass_behind_source() {
        // T18 Tables 58–60: both routes leave the backyard via corners behind
        // the source before passing the outside of the same screening solid.
        let paths = run(
            json!({"buildings":[{"id":"H","footprint":[[36.83,7.19,0],[54.15,17.19,0],[29.15,60.49,0],[-5.49,40.49,0],[9.51,14.51,0],[18.17,19.51,0],[8.17,36.83,0],[25.49,46.83,0],[40.49,20.85,0],[31.83,15.85,0]],"height_m":10}]}),
            [15.,35.,2.], [44.64,63.66,4.], 8.,
        ).unwrap();
        assert_eq!(paths.len(), 2, "{paths:?}");
        let actual: Vec<Vec<_>> = paths
            .iter()
            .map(|p| p[1..p.len() - 1].iter().map(|p| [p[0], p[1]]).collect())
            .collect();
        assert!(
            actual.contains(&vec![
                [18.17, 19.51],
                [9.51, 14.51],
                [-5.49, 40.49],
                [29.15, 60.49]
            ]),
            "{actual:?}"
        );
        assert!(
            actual.contains(&vec![[36.83, 7.19], [54.15, 17.19]]),
            "{actual:?}"
        );
    }
    #[test]
    fn concave_building_uses_convex_envelope_supports() {
        let request = json!({"buildings":[{"id":"notch","footprint":[[3,-2,0],[7,-2,0],[7,2,0],[6,2,0],[6,1,0],[4,1,0],[4,2,0],[3,2,0]],"height_m":10}]});
        let paths = run(request, [0.0, 0.0, 1.0], [10.0, 0.0, 1.0], 2.0).unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(
            paths[0],
            vec![
                [0.0, 0.0, 1.0],
                [3.0, 2.0, 1.0],
                [7.0, 2.0, 1.0],
                [10.0, 0.0, 1.0]
            ]
        );
    }
    #[test]
    fn side_offset_and_finite_edge_height_are_eligibility_rules() {
        let paths = run(
            json!({"barriers":[screen("wall",5.0,-2.0,9.0)]}),
            [0.0, 0.0, 1.0],
            [10.0, 0.0, 1.0],
            1.0,
        )
        .unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0][1][1], -2.0);
        assert!(
            run(
                json!({"barriers":[screen("wall",5.0,-2.0,2.0)]}),
                [0.0, 0.0, 20.0],
                [10.0, 0.0, 20.0],
                1.0
            )
            .unwrap()
            .is_empty()
        );
    }
    #[test]
    fn no_obstacle_is_no_extra_lateral_path_and_cancellation_stops_work() {
        assert!(
            run(json!({}), [0.0, 0.0, 1.0], [10.0, 0.0, 1.0], 1.0)
                .unwrap()
                .is_empty()
        );
        let cancel = AtomicBool::new(false);
        let count = AtomicUsize::new(0);
        let scene = Scene::prepare(&json!({"sources":[],"receivers":[]}), &cancel).unwrap();
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(
            candidates(
                &scene,
                [0.0, 0.0, 1.0],
                [10.0, 0.0, 1.0],
                4,
                1.0,
                &Work {
                    cancel: &cancel,
                    operations: &count,
                }
            )
            .is_err()
        );
    }

    #[test]
    fn rotation_and_translation_preserve_building_routes() {
        let angle = std::f64::consts::PI / 7.0;
        let transform = |p: Point| {
            [
                1e6 + p[0] * angle.cos() - p[1] * angle.sin(),
                -2e6 + p[0] * angle.sin() + p[1] * angle.cos(),
                p[2],
            ]
        };
        let base = [
            [4.0, -2.0, 0.0],
            [6.0, -2.0, 0.0],
            [6.0, 2.0, 0.0],
            [4.0, 2.0, 0.0],
        ]
        .map(transform);
        let paths = run(
            json!({"buildings":[{"id":"rotated","footprint":base,"height_m":10}]}),
            transform([0.0, 0.0, 1.0]),
            transform([10.0, 0.0, 1.0]),
            2.0,
        )
        .unwrap();
        assert_eq!(paths.len(), 2);
        for (actual, expected) in paths[0].iter().zip(
            [
                [0.0, 0.0, 1.0],
                [4.0, 2.0, 1.0],
                [6.0, 2.0, 1.0],
                [10.0, 0.0, 1.0],
            ]
            .map(transform),
        ) {
            assert!(distance(*actual, expected) < 1e-6);
        }
    }

    thread_local! {
        /// Route lateral searches through the reference implementation.
        pub(super) static REFERENCE_SEARCH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// The path search as of 4f683d1: eager visibility tests, every popped
    /// state expanded. Kept verbatim as the reference for the optimized search.
    pub(super) fn shortest_convex_reference(
        nodes: &[Point],
        frame: &Frame,
        side: f64,
        visibility: &Visibility<'_, '_>,
    ) -> Result<Option<Vec<Point>>, Diagnostic> {
        let local: Vec<Point> = nodes
            .iter()
            .map(|p| {
                let mut q = frame.coordinates(*p);
                q[1] *= side;
                q
            })
            .collect();
        let mut visible = std::collections::HashMap::new();
        let mut angles = std::collections::HashMap::new();
        let mut states: std::collections::HashMap<(usize, usize), State> =
            std::collections::HashMap::new();
        let mut queue = BinaryHeap::new();
        for current in 2..nodes.len() {
            if visibility.clear(nodes[0], nodes[current])? {
                let cost = distance(nodes[0], nodes[current]);
                states.insert((0, current), State { cost, parent: None });
                queue.push(Queue {
                    cost,
                    previous: 0,
                    current,
                });
            }
        }
        while let Some(Queue {
            cost,
            previous,
            current,
        }) = queue.pop()
        {
            visibility.tick()?;
            let key = (previous, current);
            if cost > states[&key].cost {
                continue;
            }
            if current == 1 {
                let mut ids = vec![current];
                let mut cursor = key;
                loop {
                    ids.push(cursor.0);
                    if let Some(parent) = states[&cursor].parent {
                        cursor = parent
                    } else {
                        break;
                    }
                }
                ids.reverse();
                if ids.len() < 3 {
                    return Ok(None);
                }
                return Ok(Some(ids.into_iter().map(|i| nodes[i]).collect()));
            }
            let old = [
                local[current][0] - local[previous][0],
                local[current][1] - local[previous][1],
            ];
            let old_angle = old[1].atan2(old[0]);
            let terminal_escape = !frame.within_leg(nodes[current]);
            for next in 1..nodes.len() {
                visibility.tick()?;
                if next == current || next == previous {
                    continue;
                }
                let turn = orientation(local[previous], local[current], local[next]);
                if turn > 0.0 && !terminal_escape {
                    continue;
                }
                let new = [
                    local[next][0] - local[current][0],
                    local[next][1] - local[current][1],
                ];
                if turn == 0.0 {
                    if old[0] * new[0] + old[1] * new[1] <= 0.0 {
                        continue;
                    }
                } else if !terminal_escape
                    && *angles
                        .entry((current, next))
                        .or_insert_with(|| new[1].atan2(new[0]))
                        > old_angle
                {
                    continue;
                }
                let connection = if current < next {
                    (current, next)
                } else {
                    (next, current)
                };
                let clear = if let Some(clear) = visible.get(&connection) {
                    *clear
                } else {
                    let clear = visibility.clear(nodes[current], nodes[next])?;
                    visible.insert(connection, clear);
                    clear
                };
                if !clear {
                    continue;
                }
                let total = cost + distance(nodes[current], nodes[next]);
                if !total.is_finite() {
                    return Err(Diagnostic::numerical("Lateral path length overflowed"));
                }
                let next_key = (current, next);
                if states.get(&next_key).is_none_or(|old| total < old.cost) {
                    states.insert(
                        next_key,
                        State {
                            cost: total,
                            parent: Some(key),
                        },
                    );
                    queue.push(Queue {
                        cost: total,
                        previous: current,
                        current: next,
                    })
                }
            }
        }
        Ok(None)
    }

    /// Random rotated-building scenes, half on an exact round grid to provoke
    /// equal-length chains: the optimized search must return exactly the
    /// reference chains.
    #[test]
    fn optimized_search_returns_reference_chains() {
        let mut state = 11u64;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 11) as f64 / (1u64 << 53) as f64
        };
        let cancel = AtomicBool::new(false);
        let count = AtomicUsize::new(0);
        let work = Work {
            cancel: &cancel,
            operations: &count,
        };
        let (mut searches, mut chains) = (0, 0);
        for scene_index in 0..200 {
            let round = scene_index % 2 == 0;
            let mut buildings = vec![];
            for i in 0..4 {
                for j in 0..4 {
                    if next() < 0.25 {
                        continue;
                    }
                    let (cx, cy) = (i as f64 * 30. + 15., j as f64 * 30. + 15.);
                    let (w, h, angle) = if round {
                        (
                            (4. + (next() * 6.).floor()) * 2.,
                            (4. + (next() * 6.).floor()) * 2.,
                            0.,
                        )
                    } else {
                        (
                            6. + next() * 12.,
                            6. + next() * 12.,
                            next() * std::f64::consts::PI,
                        )
                    };
                    let (c, s) = (angle.cos(), angle.sin());
                    let footprint: Vec<Point> = [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
                        .iter()
                        .map(|(u, v)| {
                            let (x, y) = (u * w / 2., v * h / 2.);
                            [cx + c * x - s * y, cy + s * x + c * y, 0.0]
                        })
                        .collect();
                    let height = if round {
                        3. + (next() * 10.).floor()
                    } else {
                        3. + next() * 12.
                    };
                    buildings.push(
                        json!({"id":format!("b{i}{j}"),"footprint":footprint,"height_m":height}),
                    );
                }
            }
            let request = json!({"buildings":buildings,"ground":{"factor":0.5,"elevation_m":0.0}});
            let scene = Scene::prepare(&request, &cancel).unwrap();
            for _ in 0..60 {
                // Cell corners lie outside every building.
                let mut point = || {
                    let (i, j) = ((next() * 5.).floor(), (next() * 5.).floor());
                    let z = if round {
                        1. + (next() * 12.).floor()
                    } else {
                        0.5 + next() * 14.
                    };
                    if round {
                        [i * 30., j * 30., z]
                    } else {
                        [i * 30. + next() * 4. - 2., j * 30. + next() * 4. - 2., z]
                    }
                };
                let (start, end) = (point(), point());
                if start[0] == end[0] && start[1] == end[1] {
                    continue;
                }
                let band = (next() * 8.).floor() as usize;
                let offset = if round {
                    (next() * 20.).floor()
                } else {
                    next() * 20.
                };
                REFERENCE_SEARCH.set(true);
                let reference = candidates(&scene, start, end, band, offset, &work).unwrap();
                REFERENCE_SEARCH.set(false);
                let optimized = candidates(&scene, start, end, band, offset, &work).unwrap();
                assert_eq!(
                    optimized, reference,
                    "{start:?} {end:?} band {band} offset {offset}"
                );
                searches += 1;
                chains += reference.len();
            }
        }
        assert!(
            searches > 10_000 && chains > 5_000,
            "{searches} searches, {chains} chains"
        );
    }

    #[test]
    fn coincident_building_source_can_launch_tangentially() {
        let paths = run(
            json!({"buildings":[box_building()]}),
            [6.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
            2.0,
        )
        .unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(
            paths[0],
            vec![
                [6.0, 0.0, 1.0],
                [6.0, -2.0, 1.0],
                [4.0, -2.0, 1.0],
                [0.0, 0.0, 1.0]
            ]
        );
    }
}
