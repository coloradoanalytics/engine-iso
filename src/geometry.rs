//! Deterministic geometry preparation. These interpolation conventions are
//! application policies, not ISO-prescribed terrain reconstruction.
use crate::types::{Diagnostic, Point, check_cancel};
use robust::{Coord, orient2d};
use serde_json::{Value, json};
use spade::handles::FixedVertexHandle;
use spade::{
    ConstrainedDelaunayTriangulation, HasPosition, HierarchyHintGenerator, Intersection,
    LineIntersectionIterator, Point2, PositionInTriangulation, Triangulation,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::AtomicBool;

pub const TERRAIN_REVISION: &str = "iso-rust-cdt-spade-2.15.1-lexicographic-v1";
const Z_EPS: f64 = 1e-7;
const XY_EPS: f64 = 1e-9;
const MAX_POINTS: usize = 250_000;

pub fn add(a: Point, b: Point) -> Point {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub fn sub(a: Point, b: Point) -> Point {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn scale(a: Point, s: f64) -> Point {
    [a[0] * s, a[1] * s, a[2] * s]
}
pub fn dot(a: Point, b: Point) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn cross(a: Point, b: Point) -> Point {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
pub fn norm(a: Point) -> f64 {
    a[0].hypot(a[1]).hypot(a[2])
}
pub fn distance(a: Point, b: Point) -> f64 {
    norm(sub(a, b))
}
pub fn lerp(a: Point, b: Point, t: f64) -> Point {
    add(a, scale(sub(b, a), t))
}
pub fn triangle_area(t: [Point; 3]) -> f64 {
    norm(cross(sub(t[1], t[0]), sub(t[2], t[0]))) * 0.5
}
pub fn triangle_centroid(t: [Point; 3]) -> Point {
    scale(add(add(t[0], t[1]), t[2]), 1. / 3.)
}
fn orient(a: Point, b: Point, c: Point) -> f64 {
    orient2d(
        Coord { x: a[0], y: a[1] },
        Coord { x: b[0], y: b[1] },
        Coord { x: c[0], y: c[1] },
    )
}
fn cross2(a: Point, b: Point) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}
fn xy_distance(a: Point, b: Point) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}
fn xy_equal(a: Point, b: Point) -> bool {
    a[0] == b[0] && a[1] == b[1]
}
fn xy_cmp(a: &Point, b: &Point) -> std::cmp::Ordering {
    a[0].total_cmp(&b[0])
        .then(a[1].total_cmp(&b[1]))
        .then(a[2].total_cmp(&b[2]))
}
fn finite_number(v: &Value, label: &str) -> Result<f64, Diagnostic> {
    v.as_f64()
        .filter(|x| x.is_finite())
        .ok_or_else(|| Diagnostic::invalid(format!("{label} must be finite")))
}
pub fn parse_point(v: &Value) -> Result<Point, Diagnostic> {
    let a = v.as_array().filter(|a| a.len() == 3).ok_or_else(|| {
        Diagnostic::invalid("XYZ coordinates require exactly three finite numbers")
    })?;
    let mut p = [0.; 3];
    for i in 0..3 {
        p[i] = finite_number(&a[i], "coordinate")?;
        if p[i].abs() > 1e12 {
            return Err(Diagnostic::numerical(
                "Coordinates outside the supported ±1e12 metre range",
            ));
        }
        if p[i] == 0. {
            p[i] = 0.;
        }
    }
    Ok(p)
}
fn parse_xy(v: &Value) -> Result<Point, Diagnostic> {
    let a = v
        .as_array()
        .filter(|a| a.len() == 2)
        .ok_or_else(|| Diagnostic::invalid("XY coordinates require exactly two finite numbers"))?;
    parse_point(&json!([a[0], a[1], 0]))
}
fn array<'a>(v: &'a Value, key: &str) -> Result<&'a [Value], Diagnostic> {
    match v.get(key) {
        None => Ok(&[]),
        Some(Value::Array(a)) => Ok(a),
        _ => Err(Diagnostic::invalid(format!("{key} must be an array"))),
    }
}
fn fraction_xy(p: Point, a: Point, b: Point) -> f64 {
    let d = sub(b, a);
    let den = d[0] * d[0] + d[1] * d[1];
    if den == 0. {
        0.
    } else {
        ((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / den
    }
}
pub fn point_on_segment_xy(p: Point, a: Point, b: Point) -> bool {
    let t = fraction_xy(p, a, b);
    (0. ..=1.).contains(&t) && (orient(a, b, p) == 0. || xy_distance(p, lerp(a, b, t)) <= XY_EPS)
}
pub fn segment_intersection_xy(a: Point, b: Point, c: Point, d: Point) -> Option<(f64, f64)> {
    let u = sub(b, a);
    let v = sub(d, c);
    let den = cross2(u, v);
    if den == 0. {
        return None;
    }
    let w = sub(c, a);
    let t = cross2(w, v) / den;
    let s = cross2(w, u) / den;
    if (0. ..=1.).contains(&t) && (0. ..=1.).contains(&s) {
        Some((t, s))
    } else {
        None
    }
}
fn segments_touch(a: Point, b: Point, c: Point, d: Point) -> bool {
    if point_on_segment_xy(a, c, d)
        || point_on_segment_xy(b, c, d)
        || point_on_segment_xy(c, a, b)
        || point_on_segment_xy(d, a, b)
    {
        return true;
    }
    orient(a, b, c).signum() != orient(a, b, d).signum()
        && orient(c, d, a).signum() != orient(c, d, b).signum()
}
/// Returns the authored perimeter with an optional repeated closing vertex removed.
pub fn normalize_polygon(points: &[Point]) -> Result<Vec<Point>, Diagnostic> {
    let mut p = points.to_vec();
    if p.len() > 1 && xy_equal(p[0], *p.last().unwrap()) {
        if (p[0][2] - p.last().unwrap()[2]).abs() > Z_EPS {
            return Err(Diagnostic::invalid(
                "Closing polygon vertex has inconsistent elevation",
            ));
        }
        p.pop();
    }
    if p.len() < 3 {
        return Err(Diagnostic::invalid(
            "A polygon needs at least three unique XY vertices",
        ));
    }
    if p.len() > 10_000 {
        return Err(Diagnostic::new(
            "resource_limit",
            "Polygon exceeds 10000 perimeter vertices",
        ));
    }
    for i in 0..p.len() {
        if !p[i].iter().all(|x| x.is_finite()) {
            return Err(Diagnostic::invalid("Polygon has nonfinite coordinates"));
        }
        if xy_equal(p[i], p[(i + 1) % p.len()]) {
            return Err(Diagnostic::invalid("Polygon has a zero-length edge"));
        }
        for j in (i + 1)..p.len() {
            if j == i + 1 || (i == 0 && j == p.len() - 1) {
                continue;
            }
            if segments_touch(p[i], p[(i + 1) % p.len()], p[j], p[(j + 1) % p.len()]) {
                return Err(Diagnostic::invalid(
                    "Polygon is self-intersecting or self-touching",
                ));
            }
        }
        // Adjacent segments may share their endpoint but may not retrace.
        let prev = p[(i + p.len() - 1) % p.len()];
        let next = p[(i + 1) % p.len()];
        let incoming = sub(prev, p[i]);
        let outgoing = sub(next, p[i]);
        if orient(prev, p[i], next) == 0.
            && incoming[0] * outgoing[0] + incoming[1] * outgoing[1] > 0.
        {
            return Err(Diagnostic::invalid("Polygon has a retracing edge"));
        }
    }
    if polygon_signed_area(&p).abs() <= 1e-15 {
        return Err(Diagnostic::invalid("Polygon has zero XY area"));
    }
    Ok(p)
}
pub fn polygon_signed_area(p: &[Point]) -> f64 {
    if p.len() < 3 {
        return 0.;
    }
    let origin = p[0];
    (1..p.len() - 1)
        .map(|i| cross2(sub(p[i], origin), sub(p[i + 1], origin)) * 0.5)
        .sum()
}
pub fn point_in_polygon_xy(p: Point, poly: &[Point]) -> bool {
    if poly.len() < 3 {
        return false;
    }
    let mut inside = false;
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        // Cheap bounds rejection before the exact boundary predicate, which
        // can only hold within XY_EPS of the edge's bounding box.
        let near =
            |k: usize| p[k] >= a[k].min(b[k]) - 2. * XY_EPS && p[k] <= a[k].max(b[k]) + 2. * XY_EPS;
        if near(0) && near(1) && point_on_segment_xy(p, a, b) {
            return true;
        }
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
        {
            inside = !inside;
        }
    }
    inside
}
fn point_in_triangle_xy(p: Point, t: [Point; 3]) -> bool {
    let a = orient(t[0], t[1], p);
    let b = orient(t[1], t[2], p);
    let c = orient(t[2], t[0], p);
    (a >= 0. && b >= 0. && c >= 0.) || (a <= 0. && b <= 0. && c <= 0.)
}
/// First valid ear in authored order; no sorting or winding reversal of indices.
pub fn triangulate_polygon(points: &[Point]) -> Result<Vec<[usize; 3]>, Diagnostic> {
    let p = normalize_polygon(points)?;
    let sign = polygon_signed_area(&p).signum();
    let mut ids: Vec<usize> = (0..p.len()).collect();
    let mut out = Vec::with_capacity(p.len() - 2);
    while ids.len() > 3 {
        let mut ear = None;
        for i in 0..ids.len() {
            let a = ids[(i + ids.len() - 1) % ids.len()];
            let b = ids[i];
            let c = ids[(i + 1) % ids.len()];
            if orient(p[a], p[b], p[c]) * sign <= 0. {
                continue;
            }
            if ids.iter().any(|&j| {
                j != a && j != b && j != c && point_in_triangle_xy(p[j], [p[a], p[b], p[c]])
            }) {
                continue;
            }
            ear = Some((i, [a, b, c]));
            break;
        }
        if let Some((i, t)) = ear {
            out.push(t);
            ids.remove(i);
        } else {
            return Err(Diagnostic::numerical(
                "No nondegenerate polygon ear can be resolved",
            ));
        }
    }
    out.push([ids[0], ids[1], ids[2]]);
    Ok(out)
}
pub fn point_on_triangle(p: Point, t: [Point; 3]) -> bool {
    let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
    let len = norm(n);
    if len == 0. || dot(sub(p, t[0]), n).abs() > Z_EPS * len {
        return false;
    }
    let drop = (0..3)
        .max_by(|&a, &b| n[a].abs().total_cmp(&n[b].abs()))
        .unwrap();
    let project = |v: Point| match drop {
        0 => [v[1], v[2], 0.],
        1 => [v[0], v[2], 0.],
        _ => [v[0], v[1], 0.],
    };
    point_in_triangle_xy(project(p), t.map(project))
}

#[derive(Clone, Debug)]
struct Vertex {
    p: Point,
    index: usize,
}
impl HasPosition for Vertex {
    type Scalar = f64;
    fn position(&self) -> Point2<f64> {
        Point2::new(self.p[0], self.p[1])
    }
}
/// Point location descends a static hierarchy instead of spade's default
/// last-used-vertex hint. That default is one atomic shared by every worker:
/// parallel lookups in different areas overwrite it, so each walk starts far
/// from its point and all workers contend on one cache line. The hint only
/// chooses where a walk starts; the triangulation and every located face are
/// fixed by exact predicates.
type Mesh = ConstrainedDelaunayTriangulation<Vertex, (), (), (), HierarchyHintGenerator<f64>>;
#[derive(Clone, Debug)]
pub struct Terrain {
    pub vertices: Vec<Point>,
    pub triangles: Vec<[usize; 3]>,
    pub breakline_edges: Vec<[usize; 2]>,
    mesh: Mesh,
    hull: Vec<[usize; 2]>,
    collinear: Vec<usize>,
    plane: f64,
    /// Highest elevation the surface can take: every interpolant and the
    /// nearest-hull extension are convex combinations of vertex heights.
    highest: f64,
    /// Constants of the surface derived on first use.
    planar: std::sync::OnceLock<bool>,
    xy_hull: std::sync::OnceLock<Vec<Point>>,
    inferred: BTreeMap<String, Point>,
    construction_profile: String,
}
struct Work<'a> {
    cancel: &'a AtomicBool,
}
impl Work<'_> {
    fn tick(&mut self) -> Result<(), Diagnostic> {
        check_cancel(self.cancel)
    }
}

impl Terrain {
    pub fn prepare(request: &Value, cancel: &AtomicBool) -> Result<Self, Diagnostic> {
        check_cancel(cancel)?;
        let terrain = &request["terrain"];
        if !terrain.is_null() && !terrain.is_object() {
            return Err(Diagnostic::invalid("terrain must be an object"));
        }
        for k in ["raster", "raster_path", "raster_samples"] {
            if terrain.get(k).is_some() {
                return Err(Diagnostic::unsupported(
                    "Raster authoring is outside terrain preparation",
                ));
            }
        }
        let supplied_profile = terrain.get("construction_profile").and_then(Value::as_str);
        if supplied_profile.is_some_and(|s| s != "acalc_schema11") {
            return Err(Diagnostic::unsupported(
                "Unknown terrain construction_profile",
            ));
        }
        if terrain.get("construction_profile").is_some() && supplied_profile.is_none() {
            return Err(Diagnostic::invalid("construction_profile must be a string"));
        }
        let mut points = Vec::new();
        let mut segments = Vec::new();
        let mut work = Work { cancel };
        for v in array(terrain, "samples")? {
            work.tick()?;
            points.push(parse_point(v)?);
        }
        for line in array(terrain, "breaklines")? {
            let vals = line.as_array().filter(|a| a.len() >= 2).ok_or_else(|| {
                Diagnostic::invalid("Each breakline needs at least two ground vertices")
            })?;
            let p = vals
                .iter()
                .map(parse_point)
                .collect::<Result<Vec<_>, _>>()?;
            points.extend_from_slice(&p);
            for pair in p.windows(2) {
                segments.push([pair[0], pair[1]]);
            }
        }
        let mut has_ground_ref = false;
        collect_location_ground_refs(request, &mut points, &mut has_ground_ref, &mut work)?;
        for key in ["buildings", "barriers"] {
            for obj in array(request, key)? {
                if obj.get("ground_vertices").is_some() {
                    has_ground_ref = true;
                    for r in array(obj, "ground_vertices")? {
                        if let Some(p) = literal_ground_ref(r)? {
                            points.push(p);
                        }
                    }
                }
                if key == "buildings" {
                    for p in array(obj, "footprint")? {
                        points.push(parse_point(p)?);
                    }
                } else if obj.get("vertices").is_some() {
                    for p in array(obj, "vertices")? {
                        points.push(parse_point(p)?);
                    }
                } else if obj.get("start").is_some() || obj.get("end").is_some() {
                    points.push(parse_point(&obj["start"])?);
                    points.push(parse_point(&obj["end"])?);
                }
            }
        }
        let definitions = array(terrain, "breakline_definitions")?;
        let targets = array(terrain, "inference_points")?;
        if (has_ground_ref
            || terrain.get("inference_points").is_some()
            || terrain.get("breakline_definitions").is_some())
            && supplied_profile != Some("acalc_schema11")
        {
            return Err(Diagnostic::invalid(
                "Ground references and staged inference require terrain.construction_profile acalc_schema11",
            ));
        }
        let mut line_ids = BTreeSet::new();
        for line in definitions {
            let id = required_id(line)?;
            if !line_ids.insert(id) {
                return Err(Diagnostic::invalid(
                    "Duplicate terrain breakline definition ID",
                ));
            }
            let r = array(line, "vertices")?;
            if r.len() < 2 {
                return Err(Diagnostic::invalid(
                    "Referenced breakline needs at least two vertices",
                ));
            }
            let p = r
                .iter()
                .map(literal_ground_ref)
                .collect::<Result<Vec<_>, _>>()?;
            points.extend(p.iter().flatten().copied());
            for pair in p.windows(2) {
                if let (Some(a), Some(b)) = (pair[0], pair[1]) {
                    segments.push([a, b]);
                }
            }
        }
        let plane = match request.get("ground").and_then(|g| g.get("elevation_m")) {
            Some(v) => finite_number(v, "ground.elevation_m")?,
            None => 0.,
        };
        if !targets.is_empty() && points.is_empty() {
            return Err(Diagnostic::invalid(
                "Ground inference requires explicit initial ground observations",
            ));
        }
        let mut initial = Self::build(points, segments.clone(), plane, &mut work)?;
        let mut inferred = BTreeMap::new();
        for target in targets {
            work.tick()?;
            let id = required_id(target)?.to_owned();
            let mut p = parse_xy(&target["position_xy"])?;
            p[2] = initial.elevation(p[0], p[1])?;
            if inferred.insert(id, p).is_some() {
                return Err(Diagnostic::invalid("Duplicate terrain inference point ID"));
            }
        }
        initial.inferred = inferred.clone();
        let mut final_points = initial.vertices.clone();
        final_points.extend(inferred.values().copied());
        for line in definitions {
            let p = array(line, "vertices")?
                .iter()
                .map(|r| initial.resolve_ground_ref(r))
                .collect::<Result<Vec<_>, _>>()?;
            for pair in p.windows(2) {
                segments.push([pair[0], pair[1]]);
            }
        }
        let mut result = if targets.is_empty() && definitions.is_empty() {
            initial
        } else {
            Self::build(final_points, segments, plane, &mut work)?
        };
        result.inferred = inferred;
        result.construction_profile = supplied_profile.unwrap_or("explicit_samples").to_owned();
        check_cancel(cancel)?;
        Ok(result)
    }

    fn build(
        mut points: Vec<Point>,
        mut segments: Vec<[Point; 2]>,
        plane: f64,
        work: &mut Work<'_>,
    ) -> Result<Self, Diagnostic> {
        if points.len() > MAX_POINTS {
            return Err(Diagnostic::new(
                "resource_limit",
                "Terrain exceeds 250000 ground observations",
            ));
        }
        for s in &mut segments {
            if xy_equal(s[0], s[1]) {
                return Err(Diagnostic::invalid(
                    "Breakline has a zero-length XY segment",
                ));
            }
            if xy_cmp(&s[0], &s[1]).is_gt() {
                s.swap(0, 1);
            }
        }
        segments.sort_by(|a, b| xy_cmp(&a[0], &b[0]).then(xy_cmp(&a[1], &b[1])));
        segments.dedup();
        let mut crossing_index: BTreeMap<u64, Vec<Point>> = BTreeMap::new();
        for &p in &points {
            crossing_index
                .entry(ordered_float(p[0]))
                .or_default()
                .push(p);
        }
        // Compute consistent crossings before inserting constraints. A single
        // shared node is used by both segment chains; original samples are kept.
        for i in 0..segments.len() {
            for j in i + 1..segments.len() {
                work.tick()?;
                let [a, b] = segments[i];
                let [c, d] = segments[j];
                if b[0].max(a[0]) < c[0].min(d[0]) {
                    break;
                }
                if let Some((t, u)) = segment_intersection_xy(a, b, c, d) {
                    let p = lerp(a, b, t);
                    let q = lerp(c, d, u);
                    if (p[2] - q[2]).abs() > Z_EPS {
                        return Err(Diagnostic::invalid(
                            "Crossing breaklines have inconsistent elevations",
                        ));
                    }
                    // Independent floating-point intersections of three or more
                    // lines need one canonical topological node. Snap calculated
                    // crossings only; never merge distinct authored observations.
                    let mut existing = None;
                    for candidates in crossing_index
                        .range(ordered_float(p[0] - XY_EPS)..=ordered_float(p[0] + XY_EPS))
                        .map(|(_, p)| p)
                    {
                        for &candidate in candidates {
                            work.tick()?;
                            if xy_distance(p, candidate) <= XY_EPS
                                && existing.is_none_or(|other| xy_cmp(&candidate, &other).is_lt())
                            {
                                existing = Some(candidate);
                            }
                        }
                    }
                    if let Some(other) = existing {
                        if (p[2] - other[2]).abs() > Z_EPS {
                            return Err(Diagnostic::invalid(
                                "Ground sample at breakline crossing has inconsistent elevation",
                            ));
                        }
                    } else {
                        crossing_index
                            .entry(ordered_float(p[0]))
                            .or_default()
                            .push(p);
                        points.push(p);
                    }
                }
            }
        }
        points.sort_by(xy_cmp);
        let mut unique: Vec<Point> = Vec::with_capacity(points.len());
        for p in points {
            work.tick()?;
            if let Some(last) = unique.last()
                && xy_equal(*last, p)
            {
                if (last[2] - p[2]).abs() > Z_EPS {
                    return Err(Diagnostic::invalid(
                        "Coincident ground samples have inconsistent elevations",
                    ));
                }
                continue;
            }
            unique.push(p);
        }
        if unique.len() > MAX_POINTS {
            return Err(Diagnostic::new(
                "resource_limit",
                "Split terrain exceeds 250000 ground vertices",
            ));
        }
        let points = unique;
        let mut constraints = BTreeSet::new();
        for [a, b] in segments {
            let left = points.partition_point(|p| p[0] < a[0].min(b[0]) - XY_EPS);
            let right = points.partition_point(|p| p[0] <= a[0].max(b[0]) + XY_EPS);
            let mut chain = Vec::new();
            for (index, &p) in points.iter().enumerate().take(right).skip(left) {
                work.tick()?;
                if point_on_segment_xy(p, a, b) {
                    let t = fraction_xy(p, a, b);
                    if (p[2] - lerp(a, b, t)[2]).abs() > Z_EPS {
                        return Err(Diagnostic::invalid(
                            "Ground sample on breakline has inconsistent elevation",
                        ));
                    }
                    chain.push((t, index));
                }
            }
            chain.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            for pair in chain.windows(2) {
                let mut edge = [pair[0].1, pair[1].1];
                edge.sort_unstable();
                if edge[0] != edge[1] {
                    constraints.insert(edge);
                }
            }
        }
        let mut mesh = Mesh::new();
        let mut handles = Vec::new();
        for (index, &p) in points.iter().enumerate() {
            work.tick()?;
            handles.push(mesh.insert(Vertex { p, index }).map_err(|e| {
                Diagnostic::numerical(format!(
                    "Terrain triangulation could not insert vertex: {e}"
                ))
            })?);
        }
        for edge in &constraints {
            work.tick()?;
            if mesh
                .try_add_constraint(handles[edge[0]], handles[edge[1]])
                .is_empty()
            {
                return Err(Diagnostic::numerical(
                    "Constrained terrain edges could not be represented without crossing",
                ));
            }
        }
        let mut triangles = Vec::new();
        for face in mesh.inner_faces() {
            let mut t = face.vertices().map(|v| v.data().index);
            let smallest = (0..3).min_by_key(|&i| t[i]).unwrap();
            t.rotate_left(smallest);
            triangles.push(t);
        }
        triangles.sort_unstable();
        let mut hull = mesh
            .convex_hull()
            .map(|e| e.vertices().map(|v| v.data().index))
            .collect::<Vec<_>>();
        hull.sort_unstable();
        let mut collinear = Vec::new();
        if points.len() > 1 && triangles.is_empty() {
            let a = points[0];
            let b = *points.last().unwrap();
            collinear = (0..points.len()).collect();
            collinear.sort_by(|&i, &j| {
                fraction_xy(points[i], a, b).total_cmp(&fraction_xy(points[j], a, b))
            });
        }
        let highest = points
            .iter()
            .map(|p| p[2])
            .reduce(f64::max)
            .unwrap_or(plane);
        Ok(Self {
            vertices: points,
            highest,
            planar: Default::default(),
            xy_hull: Default::default(),
            triangles,
            breakline_edges: constraints.into_iter().collect(),
            mesh,
            hull,
            collinear,
            plane,
            inferred: BTreeMap::new(),
            construction_profile: "explicit_samples".into(),
        })
    }

    /// Barycentric interpolation inside the mesh; None outside its hull.
    /// Each thread starts its walk at the vertex its previous lookup found:
    /// a worker evaluates one path at a time, so successive lookups are
    /// close. The located face, edge or vertex is fixed by exact predicates;
    /// edge endpoints are taken in vertex order so the walk direction cannot
    /// change the result. The arithmetic is spade's barycentric interpolation.
    fn mesh_elevation(&self, p: Point2<f64>) -> Option<f64> {
        thread_local!(static HINT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });
        let count = self.mesh.num_vertices();
        let hint = HINT.with(|h| h.get()).min(count.saturating_sub(1));
        let z = |v: FixedVertexHandle| self.mesh.vertex(v).data().p[2];
        let (value, near) = match self
            .mesh
            .locate_with_hint(p, FixedVertexHandle::from_index(hint))
        {
            PositionInTriangulation::OnVertex(v) => (0. + z(v) * 1., v),
            PositionInTriangulation::OnEdge(edge) => {
                let [a, b] = self.mesh.directed_edge(edge).vertices().map(|v| v.fix());
                let (a, b) = if a.index() <= b.index() {
                    (a, b)
                } else {
                    (b, a)
                };
                let (pa, pb) = (
                    self.mesh.vertex(a).position(),
                    self.mesh.vertex(b).position(),
                );
                let (dx, dy) = (pb.x - pa.x, pb.y - pa.y);
                let rel = ((p.x - pa.x) * dx + (p.y - pa.y) * dy) / (dx * dx + dy * dy);
                (0. + z(a) * (1. - rel) + z(b) * rel, a)
            }
            PositionInTriangulation::OnFace(face) => {
                let face = self.mesh.face(face);
                let [c0, c1, c2] = face.barycentric_interpolation(p);
                let [v0, v1, v2] = face.vertices().map(|v| v.fix());
                (0. + z(v0) * c0 + z(v1) * c1 + z(v2) * c2, v0)
            }
            _ => return None,
        };
        HINT.with(|h| h.set(near.index()));
        Some(value)
    }

    pub fn elevation(&self, x: f64, y: f64) -> Result<f64, Diagnostic> {
        if !x.is_finite() || !y.is_finite() || x.abs() > 1e12 || y.abs() > 1e12 {
            return Err(Diagnostic::numerical(
                "Terrain sampling coordinate outside supported finite range",
            ));
        }
        let p = [x, y, 0.];
        let z = if self.vertices.is_empty() {
            self.plane
        } else if self.vertices.len() == 1 {
            self.vertices[0][2]
        } else if !self.collinear.is_empty() {
            let a = self.vertices[self.collinear[0]];
            let b = self.vertices[*self.collinear.last().unwrap()];
            let t = fraction_xy(p, a, b);
            let upper = self
                .collinear
                .partition_point(|&i| fraction_xy(self.vertices[i], a, b) < t);
            if upper == 0 {
                a[2]
            } else if upper == self.collinear.len() {
                b[2]
            } else {
                let l = self.vertices[self.collinear[upper - 1]];
                let r = self.vertices[self.collinear[upper]];
                let lt = fraction_xy(l, a, b);
                let rt = fraction_xy(r, a, b);
                l[2] + (r[2] - l[2]) * (t - lt) / (rt - lt)
            }
        } else if let Some(z) = self.mesh_elevation(Point2::new(x, y)) {
            z
        } else {
            let mut nearest = (f64::INFINITY, 0.);
            for edge in &self.hull {
                let a = self.vertices[edge[0]];
                let b = self.vertices[edge[1]];
                let q = lerp(a, b, fraction_xy(p, a, b).clamp(0., 1.));
                let d = xy_distance(p, q);
                if d < nearest.0 {
                    nearest = (d, q[2]);
                }
            }
            nearest.1
        };
        if z.is_finite() {
            Ok(z)
        } else {
            Err(Diagnostic::numerical("Terrain interpolation is nonfinite"))
        }
    }

    /// Whether segment a-b lies wholly at or above the highest terrain point,
    /// so no profile breakpoint can be above it. Callers use this to skip the
    /// profile walk; their below-ground tests keep a positive height tolerance.
    pub fn below_segment(&self, a: Point, b: Point) -> bool {
        a[2].min(b[2]) >= self.highest
    }

    /// Breakpoints of exact piecewise-linear terrain elevation along an XY leg.
    /// t is normalized along the original leg, not terrain surface arc length.
    pub fn profile(&self, a: Point, b: Point) -> Result<Vec<(f64, f64)>, Diagnostic> {
        if a.iter()
            .chain(b.iter())
            .any(|x| !x.is_finite() || x.abs() > 1e12)
        {
            return Err(Diagnostic::numerical(
                "Profile endpoints outside supported finite coordinate range",
            ));
        }
        let mut ts = vec![0., 1.];
        let d = sub(b, a);
        let den = d[0] * d[0] + d[1] * d[1];
        if den == 0. {
            return Ok(vec![
                (0., self.elevation(a[0], a[1])?),
                (1., self.elevation(b[0], b[1])?),
            ]);
        }
        if !self.collinear.is_empty() {
            let left = self.vertices[self.collinear[0]];
            let right = self.vertices[*self.collinear.last().unwrap()];
            let axis = sub(right, left);
            let slope = d[0] * axis[0] + d[1] * axis[1];
            if slope != 0. {
                for &i in &self.collinear {
                    let p = sub(self.vertices[i], a);
                    push_t(&mut ts, (p[0] * axis[0] + p[1] * axis[1]) / slope);
                }
            }
        } else if !self.triangles.is_empty() {
            for item in LineIntersectionIterator::new(
                &self.mesh,
                Point2::new(a[0], a[1]),
                Point2::new(b[0], b[1]),
            ) {
                match item {
                    Intersection::VertexIntersection(v) => {
                        push_t(&mut ts, fraction_xy(v.data().p, a, b))
                    }
                    Intersection::EdgeIntersection(e) => {
                        let [c, d] = e.vertices().map(|v| v.data().p);
                        if let Some((t, _)) = segment_intersection_xy(a, b, c, d) {
                            push_t(&mut ts, t);
                        }
                    }
                    Intersection::EdgeOverlap(e) => {
                        for v in e.vertices() {
                            push_t(&mut ts, fraction_xy(v.data().p, a, b));
                        }
                    }
                }
            }
            // Outside a convex hull, nearest-boundary projection switches at
            // normals through hull vertices. Including all such roots also
            // handles legs that never enter the triangulation. A leg with both
            // endpoints in the convex hull lies inside it and needs none.
            let inside = |p: Point| {
                !matches!(
                    self.mesh.locate(Point2::new(p[0], p[1])),
                    PositionInTriangulation::OutsideOfConvexHull(_)
                        | PositionInTriangulation::NoTriangulation
                )
            };
            let hull = if inside(a) && inside(b) {
                &[][..]
            } else {
                &self.hull[..]
            };
            for edge in hull {
                let c = self.vertices[edge[0]];
                let e = sub(self.vertices[edge[1]], c);
                let slope = d[0] * e[0] + d[1] * e[1];
                if slope != 0. {
                    for &i in edge {
                        let v = sub(self.vertices[i], a);
                        push_t(&mut ts, (v[0] * e[0] + v[1] * e[1]) / slope);
                    }
                }
            }
        }
        ts.sort_by(f64::total_cmp);
        ts.dedup_by(|a, b| *a == *b);
        ts.into_iter()
            .map(|t| {
                let p = lerp(a, b, t);
                Ok((t, self.elevation(p[0], p[1])?))
            })
            .collect()
    }

    pub fn resolve_ground_ref(&self, v: &Value) -> Result<Point, Diagnostic> {
        if let Some(p) = literal_ground_ref(v)? {
            return Ok(p);
        }
        let id = v["inference_id"].as_str().unwrap();
        self.inferred
            .get(id)
            .copied()
            .ok_or_else(|| Diagnostic::invalid(format!("Unknown ground inference ID {id}")))
    }
    pub fn resolve_location(&self, v: &Value) -> Result<Point, Diagnostic> {
        let count = ["position", "position_xy", "ground_ref"]
            .iter()
            .filter(|k| v.get(**k).is_some())
            .count();
        if count != 1 {
            return Err(Diagnostic::invalid(
                "Location requires exactly one of position, position_xy or ground_ref",
            ));
        }
        let p = if let Some(p) = v.get("position") {
            if v.get("height_m").is_some() || v.get("elevation_m").is_some() {
                return Err(Diagnostic::invalid(
                    "Absolute position excludes height_m and elevation_m",
                ));
            }
            parse_point(p)?
        } else if let Some(xy) = v.get("position_xy") {
            if v.get("elevation_m").is_some() {
                return Err(Diagnostic::invalid(
                    "position_xy requires height_m, not elevation_m",
                ));
            }
            let mut p = parse_xy(xy)?;
            let h = finite_number(&v["height_m"], "height_m")?;
            if h < 0. {
                return Err(Diagnostic::invalid("height_m must be nonnegative"));
            }
            p[2] = self.elevation(p[0], p[1])? + h;
            p
        } else {
            let mut p = self.resolve_ground_ref(&v["ground_ref"])?;
            match (v.get("height_m"), v.get("elevation_m")) {
                (Some(h), None) => {
                    let h = finite_number(h, "height_m")?;
                    if h < 0. {
                        return Err(Diagnostic::invalid("height_m must be nonnegative"));
                    }
                    p[2] += h;
                }
                (None, Some(z)) => p[2] = finite_number(z, "elevation_m")?,
                _ => {
                    return Err(Diagnostic::invalid(
                        "ground_ref requires exactly one of height_m and elevation_m",
                    ));
                }
            };
            p
        };
        if !p[2].is_finite() {
            return Err(Diagnostic::numerical(
                "Resolved acoustic elevation overflow",
            ));
        }
        if p[2] < self.elevation(p[0], p[1])? - Z_EPS {
            return Err(Diagnostic::invalid(format!(
                "Acoustic position is below prepared terrain at {p:?} m"
            )));
        }
        Ok(p)
    }
    /// Highest vertex elevation, or the plane elevation without vertices.
    pub fn highest(&self) -> f64 {
        self.highest
    }
    /// XY convex hull of the vertices as built by `build` on first use.
    pub fn xy_hull(&self, build: impl FnOnce(&[Point]) -> Vec<Point>) -> &[Point] {
        self.xy_hull.get_or_init(|| build(&self.vertices))
    }
    pub fn is_planar(&self) -> bool {
        *self.planar.get_or_init(|| self.planarity())
    }
    fn planarity(&self) -> bool {
        if self.vertices.len() < 3 {
            return true;
        }
        if self.triangles.is_empty() {
            let a = self.vertices[0];
            let b = *self.vertices.last().unwrap();
            return self
                .vertices
                .iter()
                .all(|&p| (p[2] - lerp(a, b, fraction_xy(p, a, b))[2]).abs() <= Z_EPS);
        }
        let t = self.triangles[0].map(|i| self.vertices[i]);
        let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
        self.vertices
            .iter()
            .all(|&p| dot(sub(p, t[0]), n).abs() <= Z_EPS * n[2].abs())
    }
    pub fn prepared_json(&self) -> Value {
        let mode = if self.vertices.is_empty() {
            "plane"
        } else if self.vertices.len() == 1 {
            "single_point"
        } else if !self.collinear.is_empty() {
            "collinear"
        } else {
            "triangulated"
        };
        let mut v = json!({"construction_profile":self.construction_profile,"revision":TERRAIN_REVISION,"vertices":self.vertices,"triangles":self.triangles,"breakline_edges":self.breakline_edges,"inferred_points":self.inferred.iter().map(|(id,p)|json!({"id":id,"position":p})).collect::<Vec<_>>(),"coverage_mode":mode});
        if self.vertices.is_empty() {
            v["elevation_m"] = json!(self.plane);
        }
        if !self.collinear.is_empty() {
            v["collinear_support_indices"] = json!(self.collinear);
        }
        v
    }
}
fn push_t(ts: &mut Vec<f64>, t: f64) {
    if t > 0. && t < 1. && t.is_finite() {
        ts.push(t);
    }
}
fn ordered_float(value: f64) -> u64 {
    let bits = value.to_bits();
    if bits >> 63 != 0 {
        !bits
    } else {
        bits ^ (1_u64 << 63)
    }
}
fn required_id(v: &Value) -> Result<&str, Diagnostic> {
    v["id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Diagnostic::invalid("Ground object ID must be a nonempty string"))
}
fn literal_ground_ref(v: &Value) -> Result<Option<Point>, Diagnostic> {
    let obj = v
        .as_object()
        .ok_or_else(|| Diagnostic::invalid("GroundRef must be an object"))?;
    if obj.len() != 1 {
        return Err(Diagnostic::invalid(
            "GroundRef has exactly one position or inference_id field",
        ));
    }
    if let Some(p) = obj.get("position") {
        Ok(Some(parse_point(p)?))
    } else if obj
        .get("inference_id")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
    {
        Ok(None)
    } else {
        Err(Diagnostic::invalid(
            "GroundRef needs explicit position or nonempty inference_id",
        ))
    }
}
fn collect_location_ground_refs(
    v: &Value,
    points: &mut Vec<Point>,
    present: &mut bool,
    work: &mut Work<'_>,
) -> Result<(), Diagnostic> {
    work.tick()?;
    match v {
        Value::Object(o) => {
            for (k, value) in o {
                if k == "ground_ref" {
                    *present = true;
                    if let Some(p) = literal_ground_ref(value)? {
                        points.push(p);
                    }
                } else {
                    collect_location_ground_refs(value, points, present, work)?;
                }
            }
        }
        Value::Array(a) => {
            for value in a {
                collect_location_ground_refs(value, points, present, work)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn terrain(v: Value) -> Terrain {
        Terrain::prepare(&v, &AtomicBool::new(false)).unwrap()
    }
    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }
    #[test]
    fn analytic_plane_and_nearest_hull() {
        let t = terrain(json!({"terrain":{"samples":[[0,0,10],[10,0,20],[0,10,30],[10,10,40]]}}));
        close(t.elevation(2., 3.).unwrap(), 18.);
        close(t.elevation(20., 3.).unwrap(), 26.);
        close(t.elevation(-5., -5.).unwrap(), 10.);
        assert!(t.is_planar());
    }
    #[test]
    fn singleton_collinear_and_empty() {
        close(
            terrain(json!({"ground":{"elevation_m":-3}}))
                .elevation(2., 9.)
                .unwrap(),
            -3.,
        );
        close(
            terrain(json!({"terrain":{"samples":[[1,2,7]]}}))
                .elevation(20., 90.)
                .unwrap(),
            7.,
        );
        let t = terrain(json!({"terrain":{"samples":[[0,0,0],[5,0,10],[10,0,0]]}}));
        close(t.elevation(2., 99.).unwrap(), 4.);
        close(t.elevation(7., 99.).unwrap(), 6.);
        close(t.elevation(12., 99.).unwrap(), 0.);
        assert!(!t.is_planar());
    }
    #[test]
    fn immutable_inference_uses_initial_surface() {
        let t = terrain(
            json!({"terrain":{"construction_profile":"acalc_schema11","samples":[[0,0,0],[10,0,10]],"inference_points":[{"id":"a","position_xy":[5,10]},{"id":"b","position_xy":[10,10]}]},"receivers":[{"ground_ref":{"inference_id":"a"},"height_m":2}]}),
        );
        assert_eq!(
            t.resolve_ground_ref(&json!({"inference_id":"a"})).unwrap(),
            [5., 10., 5.]
        );
        assert_eq!(
            t.resolve_ground_ref(&json!({"inference_id":"b"})).unwrap(),
            [10., 10., 10.]
        );
    }
    #[test]
    fn acoustic_positions_do_not_seed_terrain() {
        let t = terrain(
            json!({"ground":{"elevation_m":3},"sources":[{"position":[0,0,100]}],"reflectors":[{"start":[0,0,30],"end":[1,0,30]}]}),
        );
        assert!(t.vertices.is_empty());
        assert_eq!(
            t.resolve_location(&json!({"position_xy":[0,0],"height_m":2}))
                .unwrap(),
            [0., 0., 5.]
        );
    }
    #[test]
    fn crossing_breaklines_split_and_inconsistent_rejected() {
        let t =
            terrain(json!({"terrain":{"breaklines":[[[0,0,0],[10,10,10]],[[0,10,5],[10,0,5]]]}}));
        assert!(t.vertices.contains(&[5., 5., 5.]));
        assert_eq!(t.breakline_edges.len(), 4);
        assert!(
            Terrain::prepare(
                &json!({"terrain":{"breaklines":[[[0,0,0],[10,10,10]],[[0,10,6],[10,0,6]]]}}),
                &AtomicBool::new(false)
            )
            .is_err()
        );
    }
    #[test]
    fn overlapping_constraints_are_split_once() {
        let t = terrain(
            json!({"terrain":{"breaklines":[[[0,0,0],[10,0,10]],[[2,0,2],[8,0,8]],[[8,0,8],[2,0,2]]]}}),
        );
        assert_eq!(t.breakline_edges.len(), 3);
        close(t.elevation(5., 4.).unwrap(), 5.);
    }
    #[test]
    fn three_consistent_crossings_share_one_node() {
        let t = terrain(
            json!({"terrain":{"breaklines":[[[0,0,0],[1,1,0]],[[0,1,0],[0.5,0,0]],[[0,-1,0],[0.5,1,0]]]}}),
        );
        close(t.elevation(1. / 3., 1. / 3.).unwrap(), 0.);
        assert_eq!(t.breakline_edges.len(), 6);
    }
    #[test]
    fn inferred_mixed_breakline_and_literal_sources_seed_stages() {
        let t = terrain(
            json!({"terrain":{"construction_profile":"acalc_schema11","inference_points":[{"id":"a","position_xy":[2,2]}],"breakline_definitions":[{"id":"line","vertices":[{"position":[0,0,1]},{"inference_id":"a"}]}]},"sources":[{"geometry":{"vertices":[{"ground_ref":{"position":[4,0,5]},"height_m":1}]}}]}),
        );
        assert_eq!(
            t.resolve_ground_ref(&json!({"inference_id":"a"})).unwrap(),
            [2., 2., 3.]
        );
        assert_eq!(t.breakline_edges.len(), 1);
    }
    #[test]
    fn missing_observations_cannot_infer_from_legacy_plane() {
        assert!(Terrain::prepare(&json!({"ground":{"elevation_m":50},"terrain":{"construction_profile":"acalc_schema11","inference_points":[{"id":"a","position_xy":[0,0]}]}}),&AtomicBool::new(false)).is_err());
    }
    #[test]
    fn coincident_and_on_line_conflicts_rejected() {
        for v in [
            json!({"terrain":{"samples":[[0,0,0],[0,0,1]]}}),
            json!({"terrain":{"samples":[[5,0,1]],"breaklines":[[[0,0,0],[10,0,0]]]}}),
        ] {
            assert!(Terrain::prepare(&v, &AtomicBool::new(false)).is_err());
        }
    }
    #[test]
    fn input_order_is_irrelevant() {
        let a = terrain(json!({"terrain":{"samples":[[0,0,0],[10,0,3],[0,10,1],[10,10,4]]}}));
        let b = terrain(json!({"terrain":{"samples":[[10,10,4],[0,10,1],[10,0,3],[0,0,0]]}}));
        assert_eq!(a.prepared_json(), b.prepared_json());
    }
    #[test]
    fn profile_integrates_ridge_exactly() {
        let t = terrain(json!({"terrain":{"samples":[[0,0,0],[5,0,10],[10,0,0]]}}));
        let p = t.profile([-5., 2., 15.], [15., 2., 15.]).unwrap();
        let integral: f64 = p
            .windows(2)
            .map(|p| (p[1].0 - p[0].0) * 20. * (p[0].1 + p[1].1) * 0.5)
            .sum();
        close(integral, 50.);
    }
    #[test]
    fn exterior_profile_clamping_breakpoints() {
        let t = terrain(json!({"terrain":{"samples":[[0,0,0],[10,0,10],[0,10,0],[10,10,10]]}}));
        let p = t.profile([-5., -2., 20.], [15., -2., 20.]).unwrap();
        let integral: f64 = p
            .windows(2)
            .map(|p| (p[1].0 - p[0].0) * 20. * (p[0].1 + p[1].1) * 0.5)
            .sum();
        close(integral, 100.);
    }
    #[test]
    fn segments_above_highest_vertex_skip_no_profile_breakpoint() {
        let t =
            terrain(json!({"terrain":{"samples":[[0,0,0],[10,0,2],[0,10,1],[10,10,0],[5,5,7.5]]}}));
        for (a, b) in [
            ([-20., 3., 7.5], [30., 6., 9.]),
            ([5., 5., 7.5], [5., 5., 7.5 + 1e-9]),
            ([40., -40., 8.], [-40., 40., 7.5]),
        ] {
            assert!(t.below_segment(a, b));
            for (s, ground) in t.profile(a, b).unwrap() {
                assert!(lerp(a, b, s)[2] >= ground, "{a:?}-{b:?} at {s}");
            }
        }
        // Just below the peak, or with one end low, the profile decides.
        assert!(!t.below_segment([0., 5., 7.4], [10., 5., 9.]));
        assert!(!t.below_segment([-20., 5., 20.], [30., 5., 0.]));
        assert!(
            terrain(json!({"ground":{"elevation_m":-3}}))
                .below_segment([0., 0., -3.], [9., 9., 0.])
        );
    }
    #[test]
    fn concave_ears_preserve_area_and_indices() {
        let p = [
            [0., 0., 0.],
            [3., 0., 0.],
            [3., 3., 0.],
            [1., 1., 0.],
            [0., 3., 0.],
        ];
        let ts = triangulate_polygon(&p).unwrap();
        close(ts.iter().map(|t| triangle_area(t.map(|i| p[i]))).sum(), 6.);
        assert!(point_in_polygon_xy([0., 1., 0.], &p));
        assert!(!point_in_polygon_xy([1.5, 2.5, 0.], &p));
    }
    #[test]
    fn invalid_polygon_and_cancel() {
        assert!(
            normalize_polygon(&[[0., 0., 0.], [2., 2., 0.], [0., 2., 0.], [2., 0., 0.]]).is_err()
        );
        assert!(Terrain::prepare(&json!({}), &AtomicBool::new(true)).is_err());
    }
}
