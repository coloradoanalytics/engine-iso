//! Immutable preparation. Acoustic positions never become terrain samples.
use crate::{geometry::*, types::*};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::atomic::AtomicBool,
};

/// Minimum clearance between a point source and a building face it does not
/// lie on. Acalc engineering policy, not an ISO 9613-2 value: a source either
/// emits from the surface (host emission rule, no host reflection) or stands
/// clear of it as an ordinary point whose host image is a real reflection.
pub const MIN_SOURCE_FACE_CLEARANCE_M: f64 = 0.05;
/// Off-plane tolerance of `Face::contains`; a source this close is on the face.
const FACE_CONTACT_M: f64 = 1e-7;

pub fn array(v: &Value, key: &str) -> Vec<Value> {
    v.get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}
pub fn number(v: &Value, key: &str, default: f64) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(default)
}
pub fn string<'a>(v: &'a Value, key: &str, default: &'a str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or(default)
}
fn points(v: &Value) -> Result<Vec<Point>, Diagnostic> {
    v.as_array()
        .ok_or_else(|| Diagnostic::invalid("Expected coordinate array"))?
        .iter()
        .map(parse_point)
        .collect()
}
fn xy_polygon(v: &Value) -> Result<Vec<Point>, Diagnostic> {
    let p = v
        .as_array()
        .ok_or_else(|| Diagnostic::invalid("Expected polygon"))?
        .iter()
        .map(|p| {
            Ok([
                p[0].as_f64()
                    .ok_or_else(|| Diagnostic::invalid("Invalid X"))?,
                p[1].as_f64()
                    .ok_or_else(|| Diagnostic::invalid("Invalid Y"))?,
                0.,
            ])
        })
        .collect::<Result<Vec<_>, Diagnostic>>()?;
    normalize_polygon(&p)
}
pub fn absorption(v: &Value) -> Result<Bands, Diagnostic> {
    if v.is_null() {
        return Ok([0.1; 8]);
    }
    let a = v
        .as_array()
        .ok_or_else(|| Diagnostic::invalid("Invalid absorption"))?;
    if a.len() != 8 {
        return Err(Diagnostic::invalid(
            "Absorption requires eight coefficients",
        ));
    }
    let mut b = [0.; 8];
    for i in 0..8 {
        b[i] = a[i]
            .as_f64()
            .ok_or_else(|| Diagnostic::invalid("Invalid absorption coefficient"))?;
        if !(0.0..=1.).contains(&b[i]) {
            return Err(Diagnostic::invalid("Absorption outside [0,1]"));
        }
    }
    Ok(b)
}
pub fn power(v: &Value) -> Result<Bands, Diagnostic> {
    let a = v
        .as_array()
        .ok_or_else(|| Diagnostic::invalid("Spectrum required"))?;
    if a.len() != 8 {
        return Err(Diagnostic::invalid("Eight octave values required"));
    }
    let mut b = [0.; 8];
    for i in 0..8 {
        if !a[i].is_null() {
            let db = a[i]
                .as_f64()
                .ok_or_else(|| Diagnostic::invalid("Invalid sound power"))?;
            if !(-200.0..=300.).contains(&db) {
                return Err(Diagnostic::invalid(
                    "Sound power outside the API numerical profile [-200,300] dB",
                ));
            }
            b[i] = positive(10f64.powf(db / 10.), "Emitted power")?
        }
    }
    Ok(b)
}
/// Per-band contribution cutoffs in dB; `None` disables a band's cutoff.
pub type Cutoffs = [Option<f64>; 8];
pub fn pruning(cutoffs: &Cutoffs) -> bool {
    cutoffs.iter().any(Option::is_some)
}
#[derive(Clone, Debug)]
pub struct Settings {
    pub ground_method: String,
    pub order: usize,
    pub cutoff: Option<f64>,
    /// Fixed per-band cutoffs: `cutoff` in every band. A pruning strategy
    /// instead supplies live cutoffs to each calculation task.
    pub cutoffs: Cutoffs,
    /// `"dba"` or `"dbc"`: automatic staged pruning on that weighted level.
    pub strategy: Option<String>,
    pub factor: f64,
    pub max_extent: Option<f64>,
    pub relative: f64,
    pub absolute: f64,
    pub max_depth: usize,
    pub long_term: bool,
    pub c0: f64,
}
impl Settings {
    pub fn from(v: &Value) -> Self {
        Self {
            ground_method: string(v, "ground_method", "general").into(),
            order: number(v, "max_reflect_order", 0.) as usize,
            // Absolute pruning is opt-in: absent and null both disable it.
            cutoff: v["minimum_contribution_db"].as_f64(),
            cutoffs: [v["minimum_contribution_db"].as_f64(); 8],
            strategy: v["pruning_strategy"].as_str().map(str::to_string),
            factor: number(v, "area_raster_factor", 0.5),
            max_extent: v["area_max_extent_m"].as_f64(),
            relative: number(v, "area_relative_tolerance", 0.001),
            absolute: number(v, "area_absolute_tolerance", 1e-12),
            max_depth: number(v, "area_max_depth", 16.) as usize,
            long_term: string(v, "meteorological_mode", "downwind") == "long_term",
            c0: number(v, "c0", 0.),
        }
    }
    pub fn pruning(&self) -> bool {
        pruning(&self.cutoffs)
    }
    pub fn json(&self) -> Value {
        json!({"ground_method":self.ground_method,"max_reflect_order":self.order,"minimum_contribution_db":self.cutoff,"pruning_strategy":self.strategy,"area_raster_factor":self.factor,"area_max_extent_m":self.max_extent,"area_relative_tolerance":self.relative,"area_absolute_tolerance":self.absolute,"area_max_depth":self.max_depth,"meteorological_mode":if self.long_term{"long_term"}else{"downwind"},"c0":self.c0})
    }
}
#[derive(Clone, Debug)]
pub struct Face {
    pub object: String,
    pub id: String,
    pub vertices: Vec<Point>,
    pub normal: Point,
    pub origin: Point,
    pub u: Point,
    pub v: Point,
    pub projected: Vec<Point>,
    pub absorption: Bands,
    pub reflects: bool,
    pub side: String,
    pub solid: bool,
    pub anchored: bool,
}
impl Face {
    #[allow(clippy::too_many_arguments)] // One immutable face constructor shared by preparation and image geometry.
    pub fn new(
        object: &str,
        id: &str,
        mut vertices: Vec<Point>,
        material: Bands,
        reflects: bool,
        side: &str,
        solid: bool,
        anchored: bool,
    ) -> Result<Self, Diagnostic> {
        if vertices.len() > 1 && vertices.first() == vertices.last() {
            vertices.pop();
        }
        if vertices.len() < 3 {
            return Err(Diagnostic::invalid("Face needs three vertices"));
        }
        let origin = vertices[0];
        if vertices.iter().flatten().any(|x| !x.is_finite()) || distance(vertices[1], origin) == 0.
        {
            return Err(Diagnostic::invalid(
                "Face has nonfinite coordinates or a zero-length edge",
            ));
        }
        let mut n = [0.; 3];
        for i in 1..vertices.len() - 1 {
            n = add(
                n,
                cross(sub(vertices[i], origin), sub(vertices[i + 1], origin)),
            )
        }
        let len = norm(n);
        if len <= 1e-12 {
            return Err(Diagnostic::invalid("Degenerate face"));
        }
        n = scale(n, 1. / len);
        let u = scale(sub(vertices[1], origin), 1. / distance(vertices[1], origin));
        let v = cross(n, u);
        let projected: Vec<Point> = vertices
            .iter()
            .map(|p| {
                let q = sub(*p, origin);
                [dot(q, u), dot(q, v), 0.]
            })
            .collect();
        for p in &vertices {
            if dot(sub(*p, origin), n).abs() > 1e-7 {
                return Err(Diagnostic::invalid("Surface vertices are not coplanar"));
            }
        }
        normalize_polygon(&projected)?;
        Ok(Self {
            object: object.into(),
            id: id.into(),
            vertices,
            normal: n,
            origin,
            u,
            v,
            projected,
            absorption: material,
            reflects,
            side: side.into(),
            solid,
            anchored,
        })
    }
    pub fn contains(&self, p: Point) -> bool {
        dot(sub(p, self.origin), self.normal).abs() <= FACE_CONTACT_M
            && point_in_polygon_xy(
                [
                    dot(sub(p, self.origin), self.u),
                    dot(sub(p, self.origin), self.v),
                    0.,
                ],
                &self.projected,
            )
    }
    pub fn signed(&self, p: Point) -> f64 {
        dot(sub(p, self.origin), self.normal)
    }
    pub fn mirror(&self, p: Point) -> Point {
        sub(p, scale(self.normal, 2. * self.signed(p)))
    }
    pub fn vertical(&self) -> bool {
        self.normal[2].abs() < 1e-9
    }
    pub fn key(&self) -> String {
        format!("{}/{}", self.object, self.id)
    }
    pub fn json(&self) -> Value {
        json!({"object_id":self.object,"face_id":self.id,"vertices":self.vertices,"normal":self.normal,"absorption":self.absorption,"reflecting":self.reflects,"side":self.side})
    }
}
#[derive(Clone, Debug)]
pub struct Building {
    pub id: String,
    pub base: Vec<Point>,
    pub roof: Vec<Point>,
    pub roof_triangles: Vec<[usize; 3]>,
    pub faces: Vec<usize>,
}
impl Building {
    pub fn top(&self, p: Point) -> Option<f64> {
        for t in &self.roof_triangles {
            let a = self.roof[t[0]];
            let b = self.roof[t[1]];
            let c = self.roof[t[2]];
            let den = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
            let w = ((b[1] - c[1]) * (p[0] - c[0]) + (c[0] - b[0]) * (p[1] - c[1])) / den;
            let q = ((c[1] - a[1]) * (p[0] - c[0]) + (a[0] - c[0]) * (p[1] - c[1])) / den;
            if w >= -1e-10 && q >= -1e-10 && w + q <= 1. + 1e-10 {
                return Some(w * a[2] + q * b[2] + (1. - w - q) * c[2]);
            }
        }
        None
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Cell {
    Point(Point),
    Line([Point; 2]),
    Triangle([Point; 3]),
}
impl Cell {
    pub fn measure(&self) -> f64 {
        match self {
            Self::Point(_) => 1.,
            Self::Line(p) => distance(p[0], p[1]),
            Self::Triangle(t) => triangle_area(*t),
        }
    }
    pub fn centroid(&self) -> Point {
        match self {
            Self::Point(p) => *p,
            Self::Line(p) => lerp(p[0], p[1], 0.5),
            Self::Triangle(t) => triangle_centroid(*t),
        }
    }
    pub fn vertices(&self) -> Vec<Point> {
        match self {
            Self::Point(p) => vec![*p],
            Self::Line(p) => p.to_vec(),
            Self::Triangle(p) => p.to_vec(),
        }
    }
    pub fn extent(&self) -> f64 {
        let p = self.vertices();
        let mut d: f64 = 0.;
        for a in &p {
            for b in &p {
                d = d.max(distance(*a, *b))
            }
        }
        d
    }
    pub fn split(&self) -> Result<Vec<Self>, Diagnostic> {
        let out = match self {
            Self::Point(_) => return Err(Diagnostic::numerical("Cannot refine point cell")),
            Self::Line([a, b]) => {
                let m = lerp(*a, *b, 0.5);
                vec![Self::Line([*a, m]), Self::Line([m, *b])]
            }
            Self::Triangle([a, b, c]) => {
                let ab = lerp(*a, *b, 0.5);
                let bc = lerp(*b, *c, 0.5);
                let ca = lerp(*c, *a, 0.5);
                vec![
                    Self::Triangle([*a, ab, ca]),
                    Self::Triangle([ab, *b, bc]),
                    Self::Triangle([ca, bc, *c]),
                    Self::Triangle([ab, bc, ca]),
                ]
            }
        };
        if out
            .iter()
            .any(|c| c.measure() <= 0. || c.extent() >= self.extent())
        {
            return Err(Diagnostic::numerical(
                "Integration subdivision cannot advance at float64 precision",
            ));
        }
        Ok(out)
    }
}
#[derive(Clone, Debug)]
pub struct Directivity {
    az: Vec<f64>,
    el: Vec<f64>,
    values: Vec<Vec<Bands>>,
    rotation: [[f64; 3]; 3],
}
impl Directivity {
    pub fn maximum_correction(&self) -> Bands {
        let mut result = [f64::NEG_INFINITY; 8];
        for row in &self.values {
            for value in row {
                for band in 0..8 {
                    result[band] = result[band].max(value[band]);
                }
            }
        }
        result
    }
    /// A conservative angular cone test for integration cells. No table knot
    /// may cross the cone unless that angular coordinate is constant.
    pub fn smooth_directions(&self, directions: &[Point]) -> bool {
        if directions.is_empty() {
            return false;
        }
        let local: Vec<_> = directions
            .iter()
            .map(|&d| {
                [
                    dot(
                        d,
                        [
                            self.rotation[0][0],
                            self.rotation[1][0],
                            self.rotation[2][0],
                        ],
                    ),
                    dot(
                        d,
                        [
                            self.rotation[0][1],
                            self.rotation[1][1],
                            self.rotation[2][1],
                        ],
                    ),
                    dot(
                        d,
                        [
                            self.rotation[0][2],
                            self.rotation[1][2],
                            self.rotation[2][2],
                        ],
                    ),
                ]
            })
            .collect();
        let centre = scale(
            local.iter().copied().fold([0.; 3], add),
            1. / local.len() as f64,
        );
        let radius = local
            .iter()
            .map(|&p| distance(p, centre))
            .fold(0., f64::max);
        if norm(centre) <= radius {
            return false;
        }
        let angular = (radius / norm(centre)).asin();
        let elevation = centre[2].atan2(centre[0].hypot(centre[1]));
        let same_elevation = local.iter().all(|p| p[2].abs() < 1e-12)
            || local
                .iter()
                .all(|p| norm(cross(*p, local[0])) <= 1e-12 * norm(*p) * norm(local[0]));
        if !same_elevation
            && self
                .el
                .iter()
                .skip(1)
                .take(self.el.len().saturating_sub(2))
                .any(|k| (k.to_radians() - elevation).abs() < angular + 1e-12)
        {
            return false;
        }
        let same_azimuth = local.iter().all(|p| {
            (p[0] * local[0][1] - p[1] * local[0][0]).abs()
                <= 1e-12 * p[0].hypot(p[1]) * local[0][0].hypot(local[0][1])
                && p[0] * local[0][0] + p[1] * local[0][1] > 0.
        });
        if same_azimuth || self.az.len() == 1 {
            return true;
        }
        if elevation.abs() + angular >= std::f64::consts::FRAC_PI_2 {
            return false;
        }
        let span = (angular.sin() / elevation.cos()).clamp(-1., 1.).asin();
        let azimuth = centre[1].atan2(centre[0]);
        !self.az.iter().any(|a| {
            let d = (a.to_radians() - azimuth + std::f64::consts::PI)
                .rem_euclid(std::f64::consts::TAU)
                - std::f64::consts::PI;
            d.abs() < span + 1e-12
        })
    }
    pub fn from(v: &Value) -> Result<Option<Self>, Diagnostic> {
        if v.is_null() {
            return Ok(None);
        }
        let az = array(v, "azimuth_deg")
            .iter()
            .map(|x| x.as_f64().unwrap())
            .collect();
        let el = array(v, "elevation_deg")
            .iter()
            .map(|x| x.as_f64().unwrap())
            .collect();
        let values = array(v, "correction_db")
            .iter()
            .map(|row| {
                row.as_array()
                    .unwrap()
                    .iter()
                    .map(|b| {
                        let mut out = [0.; 8];
                        for i in 0..8 {
                            out[i] = b[i].as_f64().unwrap()
                        }
                        out
                    })
                    .collect()
            })
            .collect();
        let mut rotation = [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]];
        if let Some(m) = v.get("orientation_matrix") {
            for i in 0..3 {
                for j in 0..3 {
                    rotation[i][j] = m[i][j].as_f64().unwrap()
                }
            }
        } else if let Some(o) = v.get("orientation_deg") {
            let y = o[0].as_f64().unwrap().to_radians();
            let p = o[1].as_f64().unwrap().to_radians();
            let r = o[2].as_f64().unwrap().to_radians();
            let (cy, sy, cp, sp, cr, sr) = (y.cos(), y.sin(), p.cos(), p.sin(), r.cos(), r.sin());
            rotation = [
                [cy * cp, cy * sp * sr - sy * cr, cy * sp * cr + sy * sr],
                [sy * cp, sy * sp * sr + cy * cr, sy * sp * cr - cy * sr],
                [-sp, cp * sr, cp * cr],
            ]
        }
        Ok(Some(Self {
            az,
            el,
            values,
            rotation,
        }))
    }
    pub fn correction(&self, d: Point) -> Bands {
        let l = [
            dot(
                d,
                [
                    self.rotation[0][0],
                    self.rotation[1][0],
                    self.rotation[2][0],
                ],
            ),
            dot(
                d,
                [
                    self.rotation[0][1],
                    self.rotation[1][1],
                    self.rotation[2][1],
                ],
            ),
            dot(
                d,
                [
                    self.rotation[0][2],
                    self.rotation[1][2],
                    self.rotation[2][2],
                ],
            ),
        ];
        let az = l[1].atan2(l[0]).to_degrees().rem_euclid(360.);
        let el = l[2].atan2(l[0].hypot(l[1])).to_degrees();
        let i = self.az.partition_point(|a| *a <= az).saturating_sub(1);
        let j = (i + 1) % self.az.len();
        let u = if self.az.len() == 1 {
            0.
        } else {
            (az - self.az[i]) / (if j == 0 { 360. } else { self.az[j] } - self.az[i])
        };
        let k = self
            .el
            .partition_point(|a| *a <= el)
            .saturating_sub(1)
            .min(self.el.len() - 2);
        let t = ((el - self.el[k]) / (self.el[k + 1] - self.el[k])).clamp(0., 1.);
        let mut b = [0.; 8];
        for (n, out) in b.iter_mut().enumerate() {
            *out = (1. - t) * ((1. - u) * self.values[k][i][n] + u * self.values[k][j][n])
                + t * ((1. - u) * self.values[k + 1][i][n] + u * self.values[k + 1][j][n])
        }
        b
    }
}
#[derive(Clone, Debug)]
pub struct Zone {
    pub id: String,
    pub cells: Vec<Cell>,
    pub measure: f64,
    pub power: Bands,
    pub directivity: Option<Directivity>,
    pub host: Option<usize>,
}
#[derive(Clone, Debug)]
pub struct Source {
    pub id: String,
    pub kind: String,
    pub zones: Vec<Zone>,
    pub centre: Point,
    pub height: Option<f64>,
    pub height_basis: String,
    pub weight: usize,
}
#[derive(Clone, Debug)]
pub struct Receiver {
    pub id: String,
    pub point: Point,
    pub ground: f64,
    pub area: Option<String>,
    pub column: usize,
    pub row: usize,
}
#[derive(Clone, Debug)]
pub struct Region {
    pub id: String,
    pub polygon: Vec<Point>,
    pub factor: f64,
}
#[derive(Clone, Debug)]
pub struct Foliage {
    pub id: String,
    pub polygon: Vec<Point>,
    pub height: f64,
}
pub struct Scene {
    pub(crate) contact_supports: crate::path::joint_contact::Catalog,
    pub face_index: crate::spatial::SpatialIndex,
    /// Broad phase over non-solid faces only; `open_faces` maps its entries,
    /// in increasing order, to face indices. Solids are tested as buildings.
    pub(crate) open_face_index: crate::spatial::SpatialIndex,
    pub(crate) open_faces: Vec<usize>,
    pub building_index: crate::spatial::SpatialIndex,
    pub terrain: Terrain,
    pub faces: Vec<Face>,
    pub buildings: Vec<Building>,
    pub(crate) building_by_id: HashMap<String, usize>,
    /// Whole-object plan outlines for frequency-dependent screen width. The
    /// first entries are building footprints in building order; then one
    /// outline per other object, holding every vertex of its faces in order.
    pub(crate) outlines: Vec<Vec<Point>>,
    /// Outline index of each prepared face.
    pub(crate) face_outline: Vec<usize>,
    pub sources: Vec<Source>,
    pub receivers: Vec<Receiver>,
    pub area_summaries: Vec<Value>,
    pub settings: Settings,
    pub regions: Vec<Region>,
    /// XY broad phase over ground-region polygons.
    pub region_index: crate::spatial::SpatialIndex,
    pub ground_factor: f64,
    pub ground_policy: String,
    pub foliage: Vec<Foliage>,
    pub alpha: Bands,
    pub atmosphere: Value,
    pub output: Value,
}
impl Scene {
    pub fn prepare(request: &Value, cancel: &AtomicBool) -> Result<Self, Diagnostic> {
        check_cancel(cancel)?;
        let settings = Settings::from(&request["settings"]);
        let terrain = Terrain::prepare(request, cancel)?;
        // Ground-method applicability is assessor-selected. Both methods use
        // the prepared terrain without a global planarity admission gate.
        let a = &request["atmosphere"];
        let (t, h, p) = (
            number(a, "temperature_c", 10.),
            number(a, "relative_humidity_percent", 70.),
            number(a, "pressure_kpa", 101.325),
        );
        let alpha = crate::acoustics::atmospheric_absorption(t, h, p)?;
        let mut scene = Self {
            contact_supports: crate::path::joint_contact::Catalog::default(),
            face_index: crate::spatial::SpatialIndex::default(),
            open_face_index: crate::spatial::SpatialIndex::default(),
            open_faces: vec![],
            building_index: crate::spatial::SpatialIndex::default(),
            terrain,
            faces: vec![],
            buildings: vec![],
            building_by_id: HashMap::new(),
            outlines: vec![],
            face_outline: vec![],
            sources: vec![],
            receivers: vec![],
            area_summaries: vec![],
            settings,
            regions: vec![],
            region_index: crate::spatial::SpatialIndex::default(),
            ground_factor: number(&request["ground"], "factor", 0.),
            ground_policy: string(&request["ground"], "region_policy", "ordered_last_wins").into(),
            foliage: vec![],
            alpha,
            atmosphere: json!({"temperature_c":t,"relative_humidity_percent":h,"pressure_kpa":p}),
            output: request["output"].clone(),
        };
        for (index, b) in array(request, "buildings").iter().enumerate() {
            check_cancel(cancel)?;
            scene.prepare_building(b, cancel).map_err(|mut error| {
                error.message = format!("Building {}: {}", string(b, "id", ""), error.message);
                error.at(format!("buildings[{index}]"))
            })?
        }
        for r in array(request, "reflectors") {
            check_cancel(cancel)?;
            let id = string(&r, "id", "");
            let verts = if r.get("vertices").is_some() {
                points(&r["vertices"])?
            } else {
                let a = parse_point(&r["start"])?;
                let b = parse_point(&r["end"])?;
                let h = number(&r, "height_m", 0.);
                vec![a, b, add(b, [0., 0., h]), add(a, [0., 0., h])]
            };
            let face = Face::new(
                id,
                "face",
                verts,
                absorption(&r["absorption"])?,
                true,
                string(&r, "side", "both"),
                false,
                false,
            )?;
            scene.faces.push(face)
        }
        for b in array(request, "barriers") {
            check_cancel(cancel)?;
            let id = string(&b, "id", "");
            let base = if b.get("ground_vertices").is_some() {
                array(&b, "ground_vertices")
                    .iter()
                    .map(|v| scene.terrain.resolve_ground_ref(v))
                    .collect::<Result<Vec<_>, _>>()?
            } else if b.get("vertices").is_some() {
                points(&b["vertices"])?
            } else {
                vec![parse_point(&b["start"])?, parse_point(&b["end"])?]
            };
            validate_open_polyline(&base)?;
            let material = absorption(&b["absorption"])?;
            for i in 0..base.len() - 1 {
                check_cancel(cancel)?;
                let mut top = [base[i], base[i + 1]];
                for k in 0..2 {
                    top[k][2] = if let Some(v) = b.get("top_elevations_m") {
                        v[i + k].as_f64().unwrap()
                    } else {
                        top[k][2]
                            + b.get("heights_m")
                                .map(|v| v[i + k].as_f64().unwrap())
                                .unwrap_or(number(&b, "height_m", 0.))
                    };
                    if top[k][2] <= base[i + k][2] {
                        return Err(Diagnostic::invalid("Barrier top must be above base"));
                    }
                }
                let mat = if let Some(m) = b.get("segment_absorption") {
                    absorption(&m[i])?
                } else {
                    material
                };
                scene.faces.push(Face::new(
                    id,
                    &format!("segment:{i}"),
                    vec![base[i], base[i + 1], top[1], top[0]],
                    mat,
                    string(&b, "role", "screen") == "screen_and_reflector",
                    "both",
                    false,
                    true,
                )?);
            }
        }
        scene.building_by_id = scene
            .buildings
            .iter()
            .enumerate()
            .map(|(i, b)| (b.id.clone(), i))
            .collect();
        // Preserve authored face/vertex order for the whole-screen width test.
        // A bent barrier is one physical screen, regardless of segmentation.
        scene.outlines = scene.buildings.iter().map(|b| b.base.clone()).collect();
        let mut object_outline: HashMap<&str, usize> = HashMap::new();
        for face in &scene.faces {
            let index = if let Some(i) = scene.building_by_id.get(&face.object) {
                *i
            } else {
                let next = scene.outlines.len();
                let i = *object_outline.entry(face.object.as_str()).or_insert(next);
                if i == next {
                    scene.outlines.push(vec![]);
                }
                scene.outlines[i].extend_from_slice(&face.vertices);
                i
            };
            scene.face_outline.push(index);
        }
        scene.face_index =
            crate::spatial::SpatialIndex::new(scene.faces.iter().map(|f| f.vertices.as_slice()));
        scene.open_faces = (0..scene.faces.len())
            .filter(|i| !scene.faces[*i].solid)
            .collect();
        scene.open_face_index = crate::spatial::SpatialIndex::new(
            scene
                .open_faces
                .iter()
                .map(|i| scene.faces[*i].vertices.as_slice()),
        );
        scene.building_index =
            crate::spatial::SpatialIndex::new(scene.buildings.iter().map(|b| b.base.as_slice()));
        scene.contact_supports = crate::path::joint_contact::Catalog::prepare(&scene, cancel)?;
        if scene.faces.len() > 10_000 {
            return Err(Diagnostic::invalid("Face limit is 10000"));
        }
        for r in array(&request["ground"], "regions") {
            scene.regions.push(Region {
                id: string(&r, "id", "").into(),
                polygon: xy_polygon(&r["boundary"])?,
                factor: number(&r, "factor", 0.),
            })
        }
        scene.region_index =
            crate::spatial::SpatialIndex::new(scene.regions.iter().map(|r| r.polygon.as_slice()));
        if scene.ground_policy == "nonoverlapping_schema11" && scene.regions.len() > 1 {
            // Triangulate once; only pairs with intersecting XY bounds can share area.
            let triangles = scene
                .regions
                .iter()
                .map(|r| triangulate_polygon(&r.polygon))
                .collect::<Result<Vec<_>, _>>()?;
            let bounds: Vec<_> = scene
                .regions
                .iter()
                .map(|r| xy_bounds(&r.polygon))
                .collect();
            for i in 0..scene.regions.len() {
                check_cancel(cancel)?;
                for j in i + 1..scene.regions.len() {
                    let (a, b) = (bounds[i], bounds[j]);
                    if a[0] > b[2] || b[0] > a[2] || a[1] > b[3] || b[1] > a[3] {
                        continue;
                    }
                    if triangulations_overlap(
                        &scene.regions[i].polygon,
                        &triangles[i],
                        &scene.regions[j].polygon,
                        &triangles[j],
                    ) {
                        return Err(Diagnostic::invalid(
                            "Ground regions overlap under nonoverlapping_schema11",
                        ));
                    }
                }
            }
        }
        for r in array(request, "misc_regions") {
            if string(&r, "kind", "") != "foliage"
                || string(&r, "method_profile", "") != "iso9613_2024_foliage_simplified_acalc_v1"
            {
                return Err(Diagnostic::unsupported(
                    "Miscellaneous attenuation profile is not defined by the implemented algorithm",
                ));
            }
            scene.foliage.push(Foliage {
                id: string(&r, "id", "").into(),
                polygon: xy_polygon(&r["boundary"])?,
                height: number(&r, "canopy_height_m", 0.),
            })
        }
        for (index, s) in array(request, "sources").iter().enumerate() {
            check_cancel(cancel)?;
            scene.prepare_source(s, cancel).map_err(|mut error| {
                error.message = format!("Source {}: {}", string(s, "id", ""), error.message);
                error.at(format!("sources[{index}]"))
            })?
        }
        for (index, r) in array(request, "receivers").iter().enumerate() {
            check_cancel(cancel)?;
            let point = scene.terrain.resolve_location(r)?;
            let ground = scene.terrain.elevation(point[0], point[1])?;
            scene.validate_position(point).map_err(|mut error| {
                error.message = format!("Receiver {}: {}", string(r, "id", ""), error.message);
                error.at(format!("receivers[{index}]"))
            })?;
            scene.receivers.push(Receiver {
                id: string(r, "id", "").into(),
                point,
                ground,
                area: None,
                column: 0,
                row: 0,
            })
        }
        let mut ids: HashSet<String> = scene.receivers.iter().map(|r| r.id.clone()).collect();
        let mut candidates = 0usize;
        for (index, a) in array(request, "area_receivers").iter().enumerate() {
            let id = string(a, "id", "");
            let boundary = xy_polygon(&a["boundary"])?;
            let exclusions = array(a, "exclude_polygons")
                .iter()
                .map(xy_polygon)
                .collect::<Result<Vec<_>, _>>()?;
            let spacing = number(a, "spacing_m", 10.);
            let height = number(a, "height_m", 1.5);
            let xmin = boundary.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
            let xmax = boundary
                .iter()
                .map(|p| p[0])
                .fold(f64::NEG_INFINITY, f64::max);
            let ymin = boundary.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
            let ymax = boundary
                .iter()
                .map(|p| p[1])
                .fold(f64::NEG_INFINITY, f64::max);
            let cols = grid_axis_count(xmin, xmax, spacing)?;
            let rows = grid_axis_count(ymin, ymax, spacing)?;
            if cols * rows > 1_000_000. || candidates as f64 + cols * rows > 1_000_000. {
                return Err(Diagnostic::invalid("Grid candidate limit is 1000000"));
            }
            candidates += cols as usize * rows as usize;
            let mut count = 0;
            for row in 0..rows as usize {
                check_cancel(cancel)?;
                for column in 0..cols as usize {
                    if column % 256 == 0 {
                        check_cancel(cancel)?;
                    }
                    let mut p = [
                        xmin + column as f64 * spacing,
                        ymin + row as f64 * spacing,
                        0.,
                    ];
                    if p[0] == 0. {
                        p[0] = 0.
                    }
                    if p[1] == 0. {
                        p[1] = 0.
                    }
                    if !grid_contains(p, &boundary)
                        || exclusions.iter().any(|e| grid_contains(p, e))
                        || scene
                            .building_index
                            .candidates(p, p)
                            .into_iter()
                            .any(|i| grid_contains(p, &scene.buildings[i].base))
                    {
                        continue;
                    }
                    let ground = scene.terrain.elevation(p[0], p[1])?;
                    p[2] = ground + height;
                    scene.validate_position(p).map_err(|mut error| {
                        error.message = format!(
                            "Area receiver {id}, column {column}, row {row}: {}",
                            error.message
                        );
                        error.at(format!("area_receivers[{index}]"))
                    })?;
                    let rid = format!(
                        "area:{}:{:016x}:{:016x}",
                        URL_SAFE_NO_PAD.encode(id),
                        p[0].to_bits(),
                        p[1].to_bits()
                    );
                    if !ids.insert(rid.clone()) {
                        return Err(Diagnostic::invalid("Generated receiver ID collision"));
                    }
                    scene.receivers.push(Receiver {
                        id: rid,
                        point: p,
                        ground,
                        area: Some(id.into()),
                        column,
                        row,
                    });
                    count += 1;
                }
            }
            scene.area_summaries.push(json!({"area_receiver_id":id,"receiver_count":count,"height_m":height,"spacing_m":spacing,
                "grid":{"origin":[xmin,ymin],"columns":cols as usize,"rows":rows as usize}}));
        }
        for s in &scene.sources {
            check_cancel(cancel)?;
            for z in &s.zones {
                for c in &z.cells {
                    for r in &scene.receivers {
                        check_cancel(cancel)?;
                        let touch = match c {
                            Cell::Point(p) => distance(*p, r.point) <= 1e-8,
                            Cell::Line(p) => point_segment_distance(r.point, p[0], p[1]) <= 1e-8,
                            Cell::Triangle(t) => point_on_triangle(r.point, *t),
                        };
                        if touch {
                            return Err(Diagnostic::invalid(format!(
                                "Receiver {} touches emitting source {}",
                                r.id, s.id
                            )));
                        }
                    }
                }
            }
        }
        Ok(scene)
    }
    fn validate_position(&self, p: Point) -> Result<(), Diagnostic> {
        if p[2] + 1e-7 < self.terrain.elevation(p[0], p[1])? {
            return Err(Diagnostic::invalid(format!(
                "Acoustic position is below prepared terrain at {p:?} m"
            )));
        }
        for b in &self.buildings {
            if point_in_polygon_xy(p, &b.base)
                && b.top(p).is_some_and(|z| p[2] < z - 1e-7)
                && !b.faces.iter().any(|i| self.faces[*i].contains(p))
            {
                return Err(Diagnostic::invalid(format!(
                    "Acoustic position is inside a building solid {} at {p:?} m",
                    b.id
                )));
            }
        }
        Ok(())
    }
    /// A point source lies on a building face or at least
    /// `MIN_SOURCE_FACE_CLEARANCE_M` from every face whose polygon it faces.
    fn validate_face_clearance(&self, cell: &Cell) -> Result<(), Diagnostic> {
        let Cell::Point(p) = *cell else {
            return Ok(());
        };
        for i in self
            .face_index
            .candidates_within(p, p, MIN_SOURCE_FACE_CLEARANCE_M)
        {
            let f = &self.faces[i];
            let d = f.signed(p).abs();
            if !f.solid || d <= FACE_CONTACT_M || d >= MIN_SOURCE_FACE_CLEARANCE_M - 1e-9 {
                continue;
            }
            if f.contains(sub(p, scale(f.normal, f.signed(p)))) {
                return Err(Diagnostic::invalid(format!(
                    "Point source is {d:.4} m from building {} face {}; attach it on the surface or place it at least {MIN_SOURCE_FACE_CLEARANCE_M} m away",
                    f.object, f.id
                )));
            }
        }
        Ok(())
    }
    fn validate_cell_obstacles(&self, cell: &Cell, cancel: &AtomicBool) -> Result<(), Diagnostic> {
        for face in &self.faces {
            check_cancel(cancel)?;
            for [a, b] in cell_edges(cell) {
                let da = face.signed(a);
                let db = face.signed(b);
                if (da > 1e-8 && db < -1e-8) || (da < -1e-8 && db > 1e-8) {
                    let hit = lerp(a, b, da / (da - db));
                    if face.contains(hit) {
                        return Err(Diagnostic::invalid(
                            "Emitting geometry crosses an opaque surface",
                        ));
                    }
                }
            }
            if let Cell::Triangle(t) = cell {
                for i in 0..face.vertices.len() {
                    if segment_pierces_triangle(
                        face.vertices[i],
                        face.vertices[(i + 1) % face.vertices.len()],
                        *t,
                    ) {
                        return Err(Diagnostic::invalid(
                            "Opaque surface passes through an emitting triangle",
                        ));
                    }
                }
            }
        }
        for building in &self.buildings {
            check_cancel(cancel)?;
            // Partition line/triangle boundaries at the footprint before
            // checking interior intervals. Endpoint agreement proves nothing
            // about a segment spanning a solid or a concave wall recess.
            for [a, b] in cell_edges(cell) {
                let mut ts = vec![0., 1.];
                for i in 0..building.base.len() {
                    if let Some((t, _)) = segment_cross(
                        a,
                        b,
                        building.base[i],
                        building.base[(i + 1) % building.base.len()],
                    ) {
                        ts.push(t);
                    }
                }
                ts.sort_by(f64::total_cmp);
                ts.dedup();
                for w in ts.windows(2) {
                    self.validate_position(lerp(a, b, (w[0] + w[1]) * 0.5))?;
                }
            }
            if let Cell::Triangle(t) = cell {
                if orient(t[0], t[1], t[2]) == 0. {
                    continue;
                }
                for roof in &building.roof_triangles {
                    let rt = roof.map(|i| building.roof[i]);
                    let overlap = clipped_triangle_polygon(*t, rt);
                    if overlap.len() < 3 || polygon_area(&overlap).abs() <= 1e-12 {
                        continue;
                    }
                    for p in overlap {
                        let source = point_on_xy_triangle_plane(*t, p).unwrap();
                        let roof = point_on_xy_triangle_plane(rt, p).unwrap();
                        if source[2] < roof[2] - 1e-7 {
                            return Err(Diagnostic::invalid(
                                "Emitting triangle overlaps a building interior",
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }
    fn prepare_building(&mut self, b: &Value, cancel: &AtomicBool) -> Result<(), Diagnostic> {
        let id = string(b, "id", "");
        let mut base = if b.get("ground_vertices").is_some() {
            array(b, "ground_vertices")
                .iter()
                .map(|v| self.terrain.resolve_ground_ref(v))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            points(&b["footprint"])?
        };
        base = normalize_polygon(&base)?;
        for other in &self.buildings {
            check_cancel(cancel)?;
            if polygons_overlap(&base, &other.base)? {
                return Err(Diagnostic::invalid(format!(
                    "Building footprint interiors overlap building {}",
                    other.id
                )));
            }
        }
        let n = base.len();
        let mut roof = base.clone();
        let roof_rule = &b["roof"];
        let mode = string(roof_rule, "mode", "");
        let avg = base.iter().map(|p| p[2]).sum::<f64>() / n as f64;
        let highest = base.iter().map(|p| p[2]).fold(f64::NEG_INFINITY, f64::max);
        let lowest = base.iter().map(|p| p[2]).fold(f64::INFINITY, f64::min);
        for i in 0..n {
            roof[i][2] = match mode {
                "average" => avg + number(roof_rule, "height_m", 0.),
                "highest" => highest + number(roof_rule, "height_m", 0.),
                "lowest" => lowest + number(roof_rule, "height_m", 0.),
                "absolute" => number(roof_rule, "elevation_m", 0.),
                "individual" => base[i][2] + roof_rule["heights_m"][i].as_f64().unwrap(),
                "absolute_individual" => roof_rule["elevations_m"][i].as_f64().unwrap(),
                _ => {
                    if b.get("roof_elevations_m").is_some() {
                        b["roof_elevations_m"][i].as_f64().unwrap()
                    } else {
                        base[i][2] + number(b, "height_m", 0.)
                    }
                }
            };
            if roof[i][2] <= base[i][2] + 1e-8 {
                return Err(Diagnostic::invalid(
                    "Building roof must lie above every base vertex",
                ));
            }
        }
        let tris = if b.get("roof_triangles").is_some() {
            array(b, "roof_triangles")
                .iter()
                .map(|v| {
                    [
                        v[0].as_u64().unwrap() as usize,
                        v[1].as_u64().unwrap() as usize,
                        v[2].as_u64().unwrap() as usize,
                    ]
                })
                .collect::<Vec<_>>()
        } else {
            triangulate_polygon(&base)?
        };
        for t in &tris {
            if t.iter().any(|i| *i >= n) {
                return Err(Diagnostic::invalid("Roof triangle index outside footprint"));
            }
        }
        // Connectivity must cover the same polygon, not just have a matching bounding box.
        let mut mesh: Vec<Cell> = tris
            .iter()
            .map(|t| Cell::Triangle([base[t[0]], base[t[1]], base[t[2]]]))
            .collect();
        for c in &mut mesh {
            if let Cell::Triangle(t) = c {
                for p in t {
                    p[2] = 0.
                }
            }
        }
        validate_mesh(&mesh, cancel)?;
        let footprint_tris = triangulate_polygon(&base)?;
        for cell in &mesh {
            check_cancel(cancel)?;
            let Cell::Triangle(t) = cell else {
                unreachable!()
            };
            let interior: f64 = footprint_tris
                .iter()
                .map(|i| clipped_triangle_area(*t, i.map(|j| base[j])))
                .sum();
            if (interior - cell.measure()).abs() > 1e-9 * cell.measure().max(1.) {
                return Err(Diagnostic::invalid(
                    "Roof triangle crosses a concave footprint boundary",
                ));
            }
        }
        let area = polygon_area(&base).abs();
        let covered = mesh.iter().map(Cell::measure).sum::<f64>();
        if (covered - area).abs() > 1e-7 * area.max(1.)
            || mesh
                .iter()
                .flat_map(Cell::vertices)
                .any(|p| !point_in_polygon_xy(p, &base))
            || mesh
                .iter()
                .any(|c| !point_in_polygon_xy(c.centroid(), &base))
        {
            return Err(Diagnostic::invalid(
                "Roof mesh does not cover exactly its footprint",
            ));
        }
        for tri in &tris {
            let cell = Cell::Triangle(tri.map(|i| roof[i]));
            for p in cell_terrain_observations(&cell, &self.terrain, cancel)? {
                if p[2] <= self.terrain.elevation(p[0], p[1])? + 1e-8 {
                    return Err(Diagnostic::invalid(
                        "Building roof intersects prepared terrain",
                    ));
                }
            }
        }
        let material = absorption(&b["absorption"])?;
        let mut faces = vec![];
        let ccw = polygon_area(&base) > 0.;
        let custom = b.get("reflecting_faces").and_then(Value::as_array);
        for i in 0..n {
            let j = (i + 1) % n;
            // Schema-11 wall frames follow the CCW boundary while their
            // indices continue to refer to the unchanged authored ring.
            let fid = if ccw {
                format!("wall:{i}:{j}")
            } else {
                format!("wall:{j}:{i}")
            };
            let mut verts = vec![base[i], base[j], roof[j], roof[i]];
            if !ccw {
                verts.reverse()
            }
            let reflect = custom.is_none_or(|a| a.iter().any(|s| s.as_str() == Some(&fid)));
            let mat = b["face_absorption"]
                .get(&fid)
                .map(absorption)
                .transpose()?
                .unwrap_or(material);
            faces.push(self.faces.len());
            self.faces.push(Face::new(
                id, &fid, verts, mat, reflect, "front", true, true,
            )?)
        }
        let planar = Face::new(
            id,
            "roof",
            roof.clone(),
            material,
            false,
            "front",
            true,
            false,
        )
        .ok();
        if let Some(mut f) = planar {
            if f.normal[2] < 0. {
                let mut verts = roof.clone();
                verts.reverse();
                f = Face::new(id, "roof", verts, material, false, "front", true, false)?
            }
            f.reflects = custom.is_none_or(|a| a.iter().any(|v| v.as_str() == Some("roof")));
            if let Some(m) = b["face_absorption"].get("roof") {
                f.absorption = absorption(m)?
            }
            faces.push(self.faces.len());
            self.faces.push(f)
        } else {
            if b.get("roof_triangles").is_none() {
                return Err(Diagnostic::invalid(
                    "Nonplanar roof requires explicit roof_triangles",
                ));
            }
            for t in &tris {
                let mut idx = *t;
                idx.sort();
                let fid = format!("roof:{}:{}:{}", idx[0], idx[1], idx[2]);
                let mut verts = vec![roof[t[0]], roof[t[1]], roof[t[2]]];
                if cross(sub(verts[1], verts[0]), sub(verts[2], verts[0]))[2] < 0. {
                    verts.reverse()
                }
                let mat = b["face_absorption"]
                    .get(&fid)
                    .map(absorption)
                    .transpose()?
                    .unwrap_or(material);
                let reflect = custom.is_none_or(|a| a.iter().any(|v| v.as_str() == Some(&fid)));
                faces.push(self.faces.len());
                self.faces.push(Face::new(
                    id, &fid, verts, mat, reflect, "front", true, false,
                )?);
            }
        }
        for fid in custom
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .chain(
                b["face_absorption"]
                    .as_object()
                    .into_iter()
                    .flat_map(|m| m.keys().map(String::as_str)),
            )
        {
            if !faces.iter().any(|i| self.faces[*i].id == fid) {
                return Err(Diagnostic::invalid(format!(
                    "Unknown building face {id}/{fid}"
                )));
            }
        }
        self.buildings.push(Building {
            id: id.into(),
            base,
            roof,
            roof_triangles: tris,
            faces,
        });
        Ok(())
    }
    fn attachment(&self, v: &Value, parent: &Value, kind: &str) -> Result<Vec<Cell>, Diagnostic> {
        let host = string(parent, "host_building_id", "");
        let fid = string(parent, "host_face_id", "");
        let b = self
            .buildings
            .iter()
            .find(|b| b.id == host)
            .ok_or_else(|| Diagnostic::invalid("Attachment building not found"))?;
        let f = b
            .faces
            .iter()
            .map(|i| &self.faces[*i])
            .find(|f| f.id == fid)
            .ok_or_else(|| Diagnostic::invalid("Attachment face not found"))?;
        let a = &v["attachment"];
        let (mut right, mut down, mut origin) = if fid.starts_with("wall:") {
            let parts: Vec<_> = fid.split(':').collect();
            let i = parts[1]
                .parse::<usize>()
                .map_err(|_| Diagnostic::invalid("Wall face identity"))?;
            let j = parts[2]
                .parse::<usize>()
                .map_err(|_| Diagnostic::invalid("Wall face identity"))?;
            let d = [b.base[j][0] - b.base[i][0], b.base[j][1] - b.base[i][1], 0.];
            (scale(d, 1. / norm(d)), [0., 0., -1.], b.roof[i])
        } else {
            let frame = string(a, "frame", "legacy_world");
            let axis = if frame == "building_local" {
                sub(b.base[1], b.base[0])
            } else {
                [1., 0., 0.]
            };
            let axis = [axis[0], axis[1], 0.];
            let r = sub(axis, scale(f.normal, dot(axis, f.normal)));
            if norm(r) < 1e-10 {
                return Err(Diagnostic::unsupported("Degenerate roof attachment frame"));
            }
            let r = scale(r, 1. / norm(r));
            let d = cross(r, f.normal);
            (r, d, f.origin)
        };
        if !fid.starts_with("wall:") {
            let frame = string(a, "frame", "legacy_world");
            if frame != "legacy_world" {
                let mut best: Option<(f64, f64, usize)> = None;
                for i in 0..f.vertices.len() {
                    let j = (i + 1) % f.vertices.len();
                    let mid = lerp(f.vertices[i], f.vertices[j], 0.5);
                    let y = dot(sub(mid, origin), down);
                    let x = dot(sub(mid, origin), right);
                    if best.is_none_or(|(by, bx, _)| {
                        y < by - 1e-8 || ((y - by).abs() <= 1e-8 && x < bx)
                    }) {
                        best = Some((y, x, i))
                    }
                }
                let i = best.unwrap().2;
                let edge = sub(f.vertices[(i + 1) % f.vertices.len()], f.vertices[i]);
                let mut edge = scale(edge, 1. / norm(edge));
                if dot(edge, right) < -1e-8
                    || (dot(edge, right).abs() <= 1e-8 && dot(edge, down) < 0.)
                {
                    edge = scale(edge, -1.)
                }
                right = edge;
                down = cross(right, f.normal)
            }
            let minx = f
                .vertices
                .iter()
                .map(|p| dot(sub(*p, origin), right))
                .fold(f64::INFINITY, f64::min);
            let miny = f
                .vertices
                .iter()
                .map(|p| dot(sub(*p, origin), down))
                .fold(f64::INFINITY, f64::min);
            origin = add(origin, add(scale(right, minx), scale(down, miny)));
        }
        let anchor = add(
            origin,
            add(
                scale(right, number(a, "x_m", 0.)),
                scale(down, number(a, "y_m", 0.)),
            ),
        );
        if !f.contains(anchor) {
            return Err(Diagnostic::invalid(
                "Attachment anchor is outside host face",
            ));
        }
        if kind == "point" {
            let distance = if string(a, "mounting", "") == "standoff" {
                number(a, "distance_m", 0.)
            } else {
                0.
            };
            return Ok(vec![Cell::Point(add(anchor, scale(f.normal, distance)))]);
        }
        let p1 = add(anchor, scale(right, number(a, "width_m", 0.)));
        let p3 = add(anchor, scale(down, number(a, "height_m", 0.)));
        let p2 = add(p1, scale(down, number(a, "height_m", 0.)));
        for p in [p1, p2, p3] {
            if !f.contains(p) {
                return Err(Diagnostic::invalid(
                    "Attached rectangle leaves supporting face",
                ));
            }
        }
        let rect = [anchor, p1, p2, p3];
        for i in 0..4 {
            let x = sub(rect[i], f.origin);
            let y = sub(rect[(i + 1) % 4], f.origin);
            let a = [dot(x, f.u), dot(x, f.v), 0.];
            let b = [dot(y, f.u), dot(y, f.v), 0.];
            for j in 0..f.projected.len() {
                if proper_cross(
                    a,
                    b,
                    f.projected[j],
                    f.projected[(j + 1) % f.projected.len()],
                ) {
                    return Err(Diagnostic::invalid(
                        "Attached rectangle crosses a concave host boundary",
                    ));
                }
            }
        }
        let project = |p: Point| {
            let d = sub(p, f.origin);
            [dot(d, f.u), dot(d, f.v), 0.]
        };
        let host_tris = triangulate_polygon(&f.projected)?;
        for triangle in [[anchor, p1, p2], [anchor, p2, p3]] {
            let projected = triangle.map(project);
            let covered: f64 = host_tris
                .iter()
                .map(|t| clipped_triangle_area(projected, t.map(|i| f.projected[i])))
                .sum();
            if (covered - triangle_area(triangle)).abs() > 1e-9 * triangle_area(triangle).max(1.) {
                return Err(Diagnostic::invalid(
                    "Attached rectangle crosses a concave host cutout",
                ));
            }
        }
        Ok(vec![
            Cell::Triangle([anchor, p1, p2]),
            Cell::Triangle([anchor, p2, p3]),
        ])
    }
    fn cells(&self, v: &Value, parent: &Value, kind: &str) -> Result<Vec<Cell>, Diagnostic> {
        if v.get("attachment").is_some() {
            return self.attachment(v, parent, kind);
        }
        if kind == "point" {
            return Ok(vec![Cell::Point(self.terrain.resolve_location(v)?)]);
        }
        let geom = &v["geometry"];
        if kind == "line" {
            let p = if v.get("geometry").is_some() {
                array(geom, "vertices")
                    .iter()
                    .map(|v| self.terrain.resolve_location(v))
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                points(&v["vertices"])?
            };
            let mut c = vec![];
            for w in p.windows(2) {
                if distance(w[0], w[1]) <= 1e-10 {
                    return Err(Diagnostic::invalid("Line contains zero-length segments"));
                }
                c.push(Cell::Line([w[0], w[1]]))
            }
            return Ok(c);
        }
        if v.get("geometry").is_some() {
            let p = array(geom, "vertices")
                .iter()
                .map(|v| self.terrain.resolve_location(v))
                .collect::<Result<Vec<_>, _>>()?;
            let mut used = HashSet::new();
            let cells = array(geom, "triangles")
                .iter()
                .map(|t| {
                    let idx = [
                        t[0].as_u64().unwrap() as usize,
                        t[1].as_u64().unwrap() as usize,
                        t[2].as_u64().unwrap() as usize,
                    ];
                    if idx.iter().any(|i| *i >= p.len()) {
                        return Err(Diagnostic::invalid("Triangle index outside geometry"));
                    }
                    if idx[0] == idx[1] || idx[1] == idx[2] || idx[0] == idx[2] {
                        return Err(Diagnostic::invalid("Triangle indices must be distinct"));
                    }
                    used.extend(idx);
                    Ok(Cell::Triangle([p[idx[0]], p[idx[1]], p[idx[2]]]))
                })
                .collect::<Result<Vec<_>, Diagnostic>>()?;
            if used.len() != p.len() {
                return Err(Diagnostic::invalid(
                    "Indexed area geometry has unused vertices",
                ));
            }
            Ok(cells)
        } else {
            array(v, "triangles")
                .iter()
                .map(|v| {
                    let p = points(v)?;
                    Ok(Cell::Triangle([p[0], p[1], p[2]]))
                })
                .collect()
        }
    }
    fn prepare_source(&mut self, s: &Value, cancel: &AtomicBool) -> Result<(), Diagnostic> {
        let kind = string(s, "kind", "");
        let raw_zones = if s.get("zones").is_some() {
            array(s, "zones")
        } else {
            vec![s.clone()]
        };
        let inherit = s.get("zones").is_some() && s.get("spectrum").is_some();
        let mut zones = vec![];
        let mut total_measure = 0.;
        for z in &raw_zones {
            check_cancel(cancel)?;
            let cells = self.cells(z, s, kind)?;
            if cells.is_empty() {
                return Err(Diagnostic::invalid("Source emitting domain is empty"));
            }
            let measure = cells.iter().map(Cell::measure).sum::<f64>();
            if cells
                .iter()
                .any(|c| c.measure() <= if kind == "area" { 1e-12 } else { 0. })
            {
                return Err(Diagnostic::invalid("Degenerate emitting cell"));
            }
            for c in &cells {
                for p in cell_terrain_observations(c, &self.terrain, cancel)? {
                    self.validate_position(p)?
                }
                self.validate_cell_obstacles(c, cancel)?;
            }
            let mut supporting = vec![];
            let host = string(s, "host_building_id", "");
            let fid = string(s, "host_face_id", "");
            if host.is_empty() {
                for cell in &cells {
                    let contacts: Vec<_> = self
                        .faces
                        .iter()
                        .filter(|f| f.solid && cell.vertices().iter().all(|p| f.contains(*p)))
                        .collect();
                    if let Some(first) = contacts.first()
                        && contacts
                            .iter()
                            .any(|f| norm(cross(first.normal, f.normal)) > 1e-8)
                    {
                        return Err(Diagnostic::invalid(
                            "Untagged source has ambiguous contact between building face planes",
                        ));
                    }
                }
            }
            for (i, f) in self.faces.iter().enumerate() {
                if !f.solid {
                    continue;
                }
                if !host.is_empty() && f.object != host {
                    continue;
                }
                if !fid.is_empty() && f.id != fid {
                    continue;
                }
                if cells
                    .iter()
                    .all(|c| c.vertices().iter().all(|p| f.contains(*p)))
                {
                    supporting.push(i)
                }
            }
            if supporting.len() > 1 {
                let n = self.faces[supporting[0]].normal;
                if supporting
                    .iter()
                    .any(|i| norm(cross(n, self.faces[*i].normal)) > 1e-8)
                {
                    return Err(Diagnostic::invalid(
                        "Ambiguous source contact between different building faces",
                    ));
                }
            }
            let displaced = z.get("attachment").is_some()
                && kind == "point"
                && string(&z["attachment"], "mounting", "") == "standoff";
            if kind == "point" {
                self.validate_face_clearance(&cells[0])?;
            }
            if !host.is_empty() && supporting.is_empty() && !displaced {
                return Err(Diagnostic::invalid(
                    "Source geometry does not lie on declared host face",
                ));
            }
            let dir = Directivity::from(
                z.get("directivity")
                    .or_else(|| s.get("directivity"))
                    .unwrap_or(&Value::Null),
            )?;
            let spec = if inherit {
                power(&s["spectrum"])?
            } else {
                power(&z["spectrum"])?
            };
            let basis = string(z, "power_basis", string(s, "power_basis", "total_power"));
            let mut total = spec;
            if basis != "total_power" {
                for w in &mut total {
                    if *w > 0. {
                        *w = positive(*w * measure, "Density-normalized source power")?;
                    }
                }
            }
            zones.push(Zone {
                id: if s.get("zones").is_some() {
                    string(z, "id", "").into()
                } else {
                    "uniform".into()
                },
                cells,
                measure,
                power: total,
                directivity: dir,
                host: supporting.first().copied(),
            });
            total_measure += measure;
        }
        if inherit && string(s, "power_basis", "total_power") == "total_power" {
            for z in &mut zones {
                for p in &mut z.power {
                    if *p > 0. {
                        *p =
                            positive(*p * (z.measure / total_measure), "Distributed source power")?;
                    }
                }
            }
        }
        let mesh: Vec<_> = zones.iter().flat_map(|z| z.cells.iter().copied()).collect();
        if kind == "area" {
            validate_mesh(&mesh, cancel)?
        }
        if kind == "area" && !string(s, "host_building_id", "").is_empty() {
            let first = zones[0].host.unwrap();
            if zones.iter().any(|z| {
                z.host.is_none_or(|i| {
                    norm(cross(self.faces[first].normal, self.faces[i].normal)) > 1e-8
                        || self.faces[first].signed(self.faces[i].origin).abs() > 1e-7
                })
            }) {
                return Err(Diagnostic::invalid(
                    "One attached area source must occupy one supporting face plane",
                ));
            }
        }
        let mut centre = [0.; 3];
        let mut wsum = Sum::default();
        let mut minheight = f64::INFINITY;
        let mut maxheight = f64::NEG_INFINITY;
        let silent = zones.iter().all(|z| z.power.iter().all(|w| *w == 0.));
        for z in &zones {
            let mut c = [0.; 3];
            for cell in &z.cells {
                c = add(c, scale(cell.centroid(), cell.measure() / z.measure));
                for p in cell_terrain_observations(cell, &self.terrain, cancel)? {
                    let h = p[2] - self.terrain.elevation(p[0], p[1])?;
                    minheight = minheight.min(h);
                    maxheight = maxheight.max(h)
                }
            }
            let wa = z
                .power
                .iter()
                .enumerate()
                .map(|(i, w)| w * 10f64.powf(crate::acoustics::A_WEIGHTING[i] / 10.))
                .sum::<f64>();
            let w = if silent { z.measure } else { wa };
            centre = add(centre, scale(c, w));
            wsum.add(w)
        }
        centre = scale(
            centre,
            1. / positive(wsum.total(), "Source centroid weight")?,
        );
        let constant = (maxheight - minheight).abs() <= 1e-7;
        let (height, basis) = if silent {
            (None, "not_required")
        } else if constant {
            (Some(0.5 * (minheight + maxheight)), "actual_constant")
        } else if self.settings.long_term {
            (
                Some(s["predominant_height_m"].as_f64().ok_or_else(|| {
                    Diagnostic::invalid(
                        "Long-term variable-height source requires predominant_height_m",
                    )
                })?),
                "predominant",
            )
        } else {
            (None, "not_required")
        };
        self.sources.push(Source {
            id: string(s, "id", "").into(),
            kind: kind.into(),
            zones,
            centre,
            height,
            height_basis: basis.into(),
            weight: match kind {
                "line" => 2,
                "area" => 3,
                _ => 1,
            },
        });
        Ok(())
    }
    pub fn ground_at(&self, p: Point) -> f64 {
        // Candidates are ascending region indices: authored order is preserved.
        let candidates = self.region_index.candidates(p, p);
        if self.ground_policy == "ordered_last_wins" {
            candidates
                .iter()
                .rev()
                .map(|i| &self.regions[*i])
                .find(|r| point_in_polygon_xy(p, &r.polygon))
                .map(|r| r.factor)
                .unwrap_or(self.ground_factor)
        } else {
            let mut sum = 0.;
            let mut count = 0;
            for r in candidates.iter().map(|i| &self.regions[*i]) {
                if point_in_polygon_xy(p, &r.polygon) {
                    sum += r.factor;
                    count += 1
                }
            }
            if count == 0 {
                self.ground_factor
            } else {
                sum / count as f64
            }
        }
    }
    pub fn prepared_json(&self) -> Value {
        json!({"terrain":self.terrain.prepared_json(),"sources":self.sources.iter().map(|s|json!({"source_id":s.id,"kind":s.kind,"zones":s.zones.iter().map(|z|{let mut v=json!({"zone_id":z.id,"measure":z.measure,"measure_unit":if s.kind=="area"{"m2"}else if s.kind=="line"{"m"}else{"point"},"normalized_total_octave_power":z.power,"host_face":z.host.map(|i|self.faces[i].key())});match z.cells[0]{Cell::Point(p)=>v["position"]=json!(p),Cell::Line(_)=>{let mut pts=vec![z.cells[0].vertices()[0]];for c in &z.cells{pts.push(c.vertices()[1])}v["vertices"]=json!(pts)},Cell::Triangle(_)=>v["triangles"]=json!(z.cells.iter().map(Cell::vertices).collect::<Vec<_>>())};v}).collect::<Vec<_>>()})).collect::<Vec<_>>(),"source_descriptors":self.sources.iter().map(|s|json!({"source_id":s.id,"centre":s.centre,"height_m":s.height,"height_basis":s.height_basis,"centroid_weighting":"linear A-weighted emitted zone power; uniform measure within zones"})).collect::<Vec<_>>(),"buildings":self.buildings.iter().map(|b|json!({"id":b.id,"footprint":b.base,"roof":b.roof,"roof_triangles":b.roof_triangles,"faces":b.faces.iter().map(|i|self.faces[*i].json()).collect::<Vec<_>>()})).collect::<Vec<_>>(),"barriers":self.faces.iter().filter(|f|f.anchored&&!f.solid).map(Face::json).collect::<Vec<_>>(),"reflectors":self.faces.iter().filter(|f|!f.anchored&&!f.solid).map(Face::json).collect::<Vec<_>>(),"misc_regions":self.foliage.iter().map(|f|json!({"id":f.id,"kind":"foliage","boundary":f.polygon,"canopy_height_m":f.height,"method_profile":"iso9613_2024_foliage_simplified_acalc_v1"})).collect::<Vec<_>>()})
    }
}
pub fn polygon_area(p: &[Point]) -> f64 {
    polygon_signed_area(p)
}
pub fn orient(a: Point, b: Point, c: Point) -> f64 {
    robust::orient2d(
        robust::Coord { x: a[0], y: a[1] },
        robust::Coord { x: b[0], y: b[1] },
        robust::Coord { x: c[0], y: c[1] },
    )
}
pub fn proper_cross(a: Point, b: Point, c: Point, d: Point) -> bool {
    let x = orient(a, b, c);
    let y = orient(a, b, d);
    let z = orient(c, d, a);
    let w = orient(c, d, b);
    ((x > 0. && y < 0.) || (x < 0. && y > 0.)) && ((z > 0. && w < 0.) || (z < 0. && w > 0.))
}
pub fn segment_cross(a: Point, b: Point, c: Point, d: Point) -> Option<(f64, f64)> {
    // Preserve exact shared XY endpoints before a near-parallel determinant
    // ratio can move them off the finite edge. Adaptive orientation separates
    // a unique endpoint intersection from collinear/degenerate segments.
    for (p, q, other_p, other_q, t, u) in [
        (a, c, b, d, 0., 0.),
        (a, d, b, c, 0., 1.),
        (b, c, a, d, 1., 0.),
        (b, d, a, c, 1., 1.),
    ] {
        if p[0] == q[0] && p[1] == q[1] {
            return (orient(p, other_p, other_q).abs() >= 1e-14).then_some((t, u));
        }
    }
    let r = sub(b, a);
    let s = sub(d, c);
    let den = r[0] * s[1] - r[1] * s[0];
    if den.abs() < 1e-14 {
        return None;
    }
    let v = sub(c, a);
    let t = (v[0] * s[1] - v[1] * s[0]) / den;
    let u = (v[0] * r[1] - v[1] * r[0]) / den;
    if (-1e-10..=1. + 1e-10).contains(&t) && (-1e-10..=1. + 1e-10).contains(&u) {
        Some((t.clamp(0., 1.), u.clamp(0., 1.)))
    } else {
        None
    }
}
pub fn point_segment_distance(p: Point, a: Point, b: Point) -> f64 {
    let d = sub(b, a);
    if dot(d, d) == 0. {
        return distance(p, a);
    }
    let t = (dot(sub(p, a), d) / dot(d, d)).clamp(0., 1.);
    distance(p, lerp(a, b, t))
}
pub fn cell_edges(c: &Cell) -> Vec<[Point; 2]> {
    match c {
        Cell::Point(_) => vec![],
        Cell::Line(p) => vec![*p],
        Cell::Triangle(t) => vec![[t[0], t[1]], [t[1], t[2]], [t[2], t[0]]],
    }
}
fn clipped_triangle_polygon(a: [Point; 3], b: [Point; 3]) -> Vec<Point> {
    let mut p = a.to_vec();
    let sign = orient(b[0], b[1], b[2]).signum();
    for i in 0..3 {
        let x = b[i];
        let y = b[(i + 1) % 3];
        let old = p;
        p = vec![];
        if old.is_empty() {
            return vec![];
        }
        for j in 0..old.len() {
            let a = old[j];
            let c = old[(j + 1) % old.len()];
            let da = orient(x, y, a) * sign;
            let dc = orient(x, y, c) * sign;
            if da >= 0. {
                p.push(a)
            }
            if (da < 0.) != (dc < 0.) {
                p.push(lerp(a, c, da / (da - dc)))
            }
        }
    }
    p
}
fn clipped_triangle_area(a: [Point; 3], b: [Point; 3]) -> f64 {
    polygon_area(&clipped_triangle_polygon(a, b)).abs()
}
pub fn polygons_overlap(a: &[Point], b: &[Point]) -> Result<bool, Diagnostic> {
    Ok(triangulations_overlap(
        a,
        &triangulate_polygon(a)?,
        b,
        &triangulate_polygon(b)?,
    ))
}
fn triangulations_overlap(a: &[Point], ta: &[[usize; 3]], b: &[Point], tb: &[[usize; 3]]) -> bool {
    ta.iter().any(|x| {
        tb.iter().any(|y| {
            clipped_triangle_area([a[x[0]], a[x[1]], a[x[2]]], [b[y[0]], b[y[1]], b[y[2]]]) > 1e-12
        })
    })
}
/// `[min_x, min_y, max_x, max_y]`.
fn xy_bounds(p: &[Point]) -> [f64; 4] {
    p.iter().fold(
        [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
        |b, q| {
            [
                b[0].min(q[0]),
                b[1].min(q[1]),
                b[2].max(q[0]),
                b[3].max(q[1]),
            ]
        },
    )
}
fn validate_mesh(cells: &[Cell], cancel: &AtomicBool) -> Result<(), Diagnostic> {
    let tris: Vec<_> = cells
        .iter()
        .filter_map(|c| {
            if let Cell::Triangle(t) = c {
                Some(*t)
            } else {
                None
            }
        })
        .collect();
    if tris.len() > 10_000 {
        return Err(Diagnostic::invalid("Triangle limit is 10000 per source"));
    }
    for i in 0..tris.len() {
        check_cancel(cancel)?;
        let a = tris[i];
        if triangle_area(a) <= 1e-12 {
            return Err(Diagnostic::invalid(
                "Degenerate emitting or roof mesh triangle",
            ));
        }
        let n = cross(sub(a[1], a[0]), sub(a[2], a[0]));
        let n = scale(n, 1. / norm(n));
        let u = scale(sub(a[1], a[0]), 1. / distance(a[1], a[0]));
        let v = cross(n, u);
        for b in &tris[i + 1..] {
            check_cancel(cancel)?;
            if (0..3).any(|k| {
                a.iter().map(|p| p[k]).fold(f64::INFINITY, f64::min)
                    > b.iter().map(|p| p[k]).fold(f64::NEG_INFINITY, f64::max) + 1e-8
                    || b.iter().map(|p| p[k]).fold(f64::INFINITY, f64::min)
                        > a.iter().map(|p| p[k]).fold(f64::NEG_INFINITY, f64::max) + 1e-8
            }) {
                continue;
            }
            if b.iter().all(|p| dot(sub(*p, a[0]), n).abs() < 1e-8) {
                let project = |p: Point| [dot(sub(p, a[0]), u), dot(sub(p, a[0]), v), 0.];
                if clipped_triangle_area(a.map(project), b.map(project)) > 1e-12 {
                    return Err(Diagnostic::invalid("Emitting triangle interiors overlap"));
                }
            } else {
                for j in 0..3 {
                    if segment_pierces_triangle(b[j], b[(j + 1) % 3], a)
                        || segment_pierces_triangle(a[j], a[(j + 1) % 3], *b)
                    {
                        return Err(Diagnostic::invalid("Emitting mesh triangles intersect"));
                    }
                }
            }
        }
    }
    Ok(())
}

fn segment_pierces_triangle(a: Point, b: Point, t: [Point; 3]) -> bool {
    let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
    let length = norm(n);
    if length <= 1e-12 {
        return false;
    }
    let da = dot(sub(a, t[0]), n) / length;
    let db = dot(sub(b, t[0]), n) / length;
    if !((da > 1e-8 && db < -1e-8) || (da < -1e-8 && db > 1e-8)) {
        return false;
    }
    point_on_triangle(lerp(a, b, da / (da - db)), t)
}

/// Affine source-plane elevation over its projected triangle, inclusive of its boundary.
fn point_on_xy_triangle_plane(t: [Point; 3], p: Point) -> Option<Point> {
    let den = orient(t[0], t[1], t[2]);
    if den == 0. {
        return None;
    }
    let a = orient(t[1], t[2], p) / den;
    let b = orient(t[2], t[0], p) / den;
    let c = 1. - a - b;
    if a < -1e-10 || b < -1e-10 || c < -1e-10 {
        None
    } else {
        Some([p[0], p[1], a * t[0][2] + b * t[1][2] + c * t[2][2]])
    }
}

/// The extrema of affine emitting height above piecewise-linear terrain occur
/// on the overlay vertices: source boundary/profile intersections and terrain
/// vertices inside the source projection. This also detects interior peaks.
fn cell_terrain_observations(
    cell: &Cell,
    terrain: &Terrain,
    cancel: &AtomicBool,
) -> Result<Vec<Point>, Diagnostic> {
    check_cancel(cancel)?;
    let mut result = cell.vertices();
    for [a, b] in cell_edges(cell) {
        for (t, _) in terrain.profile(a, b)? {
            result.push(lerp(a, b, t));
        }
    }
    if let Cell::Triangle(t) = cell
        && orient(t[0], t[1], t[2]) != 0.
    {
        let xmin = t.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
        let xmax = t.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max);
        let lo = terrain.vertices.partition_point(|p| p[0] < xmin);
        let hi = terrain.vertices.partition_point(|p| p[0] <= xmax);
        for (i, &p) in terrain.vertices[lo..hi].iter().enumerate() {
            if i % 256 == 0 {
                check_cancel(cancel)?;
            }
            if let Some(p) = point_on_xy_triangle_plane(*t, p) {
                result.push(p);
            }
        }
    }
    Ok(result)
}

fn validate_open_polyline(points: &[Point]) -> Result<(), Diagnostic> {
    if points.len() < 2 {
        return Err(Diagnostic::invalid(
            "Barrier needs at least two base vertices",
        ));
    }
    if points.len() > 4096 {
        return Err(Diagnostic::new(
            "resource_limit",
            "Barrier exceeds 4096 base vertices",
        ));
    }
    for i in 0..points.len() - 1 {
        let a = points[i];
        let b = points[i + 1];
        if a[0] == b[0] && a[1] == b[1] {
            return Err(Diagnostic::invalid("Barrier has a zero-length XY segment"));
        }
        if i > 0 {
            let before = sub(points[i - 1], a);
            let after = sub(b, a);
            if orient(points[i - 1], a, b) == 0. && before[0] * after[0] + before[1] * after[1] > 0.
            {
                return Err(Diagnostic::invalid("Barrier retraces a segment"));
            }
        }
        for j in i + 2..points.len() - 1 {
            let c = points[j];
            let d = points[j + 1];
            if proper_cross(a, b, c, d)
                || point_on_segment_xy(a, c, d)
                || point_on_segment_xy(b, c, d)
                || point_on_segment_xy(c, a, b)
                || point_on_segment_xy(d, a, b)
            {
                return Err(Diagnostic::invalid(
                    "Barrier must be a simple open polyline without self-touching",
                ));
            }
        }
    }
    Ok(())
}

fn grid_axis_count(min: f64, max: f64, spacing: f64) -> Result<f64, Diagnostic> {
    if !spacing.is_finite() || spacing <= 0. {
        return Err(Diagnostic::invalid(
            "Grid spacing must be positive and finite",
        ));
    }
    if min + spacing == min || max + spacing == max {
        return Err(Diagnostic::numerical(
            "Grid spacing cannot advance coordinates",
        ));
    }
    let mut count = ((max - min) / spacing).floor() + 1.;
    let next = min + count * spacing;
    if next > max && next - max <= 1e-8 {
        count += 1.;
    }
    if !count.is_finite() {
        return Err(Diagnostic::new(
            "resource_limit",
            "Grid candidate count overflows",
        ));
    }
    Ok(count)
}

fn grid_contains(p: Point, polygon: &[Point]) -> bool {
    if point_in_polygon_xy(p, polygon) {
        return true;
    }
    let p = [p[0], p[1], 0.];
    (0..polygon.len()).any(|i| {
        let a = polygon[i];
        let b = polygon[(i + 1) % polygon.len()];
        point_segment_distance(p, [a[0], a[1], 0.], [b[0], b[1], 0.]) <= 1e-8
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn prepare(v: Value) -> Result<Scene, Diagnostic> {
        Scene::prepare(&v, &AtomicBool::new(false))
    }
    fn building() -> Value {
        json!({"id":"b","footprint":[[0,0,0],[4,0,0],[4,4,0],[0,4,0]],"height_m":4})
    }
    fn source(kind: &str) -> Value {
        json!({"id":"s","kind":kind,"spectrum":[80,80,80,80,80,80,80,80]})
    }
    #[test]
    fn invalid_source_position_identifies_source_building_and_coordinates() {
        let mut s = source("point");
        s["id"] = json!("source-in-building");
        s["position"] = json!([2, 2, 1]);
        // Silent sources retain the same geometric validation and context.
        s["spectrum"] = json!([null, null, null, null, null, null, null, null]);
        let error = prepare(json!({"sources":[s],"buildings":[building()]}))
            .err()
            .unwrap();
        assert_eq!(error.code, "invalid_input");
        assert_eq!(error.field.as_deref(), Some("sources[0]"));
        assert_eq!(
            error.message,
            "Source source-in-building: Acoustic position is inside a building solid b at [2.0, 2.0, 1.0] m"
        );
    }
    #[test]
    fn invalid_receiver_position_identifies_receiver_building_and_coordinates() {
        let error = prepare(json!({
            "receivers":[{"id":"measurement-in-building","position":[2,2,1]}],
            "buildings":[building()]
        }))
        .err()
        .unwrap();
        assert_eq!(error.code, "invalid_input");
        assert_eq!(error.field.as_deref(), Some("receivers[0]"));
        assert_eq!(
            error.message,
            "Receiver measurement-in-building: Acoustic position is inside a building solid b at [2.0, 2.0, 1.0] m"
        );
    }
    #[test]
    fn below_terrain_position_identifies_source_and_coordinates() {
        let mut s = source("point");
        s["position"] = json!([1, 2, -1]);
        let error = prepare(json!({"sources":[s]})).err().unwrap();
        assert_eq!(error.field.as_deref(), Some("sources[0]"));
        assert_eq!(
            error.message,
            "Source s: Acoustic position is below prepared terrain at [1.0, 2.0, -1.0] m"
        );
    }
    #[test]
    fn exterior_building_contact_remains_valid() {
        let mut s = source("point");
        s["position"] = json!([2, 0, 1]);
        assert!(
            prepare(json!({"sources":[s],"buildings":[building()],
            "receivers":[{"id":"roof","position":[2,2,4]}]}))
            .is_ok()
        );
    }
    #[test]
    fn interior_terrain_peak_cannot_be_hidden_by_clear_area_edges() {
        let mut s = source("area");
        s["triangles"] = json!([[[-10, -10, 5], [10, -10, 5], [0, 10, 5]]]);
        let request = json!({"sources":[s],"settings":{"ground_method":"mean_path_height"},"terrain":{"samples":[[-20,-20,0],[20,-20,0],[0,20,0],[0,0,8]]}});
        assert!(
            prepare(request)
                .err()
                .unwrap()
                .message
                .contains("below prepared terrain")
        );
    }
    #[test]
    fn interior_roof_peak_cannot_be_hidden_by_clear_source_boundary() {
        let mut s = source("area");
        s["triangles"] = json!([[[-20, -20, 2], [20, -20, 2], [0, 20, 2]]]);
        assert!(prepare(json!({"sources":[s],"buildings":[building()]})).is_err());
    }
    #[test]
    fn roof_cannot_cover_an_interior_terrain_peak() {
        let mut b = building();
        b["height_m"] = json!(2);
        assert!(prepare(json!({"buildings":[b],"terrain":{"samples":[[2,2,3]]},"settings":{"ground_method":"mean_path_height"}})).err().unwrap().message.contains("roof intersects"));
    }
    #[test]
    fn long_term_height_uses_complete_profile() {
        let mut s = source("line");
        s["vertices"] = json!([[-5, 1, 5], [15, 1, 15]]);
        let request = json!({"sources":[s],"terrain":{"samples":[[0,0,0],[10,0,10]]},"settings":{"meteorological_mode":"long_term"}});
        assert!(
            prepare(request)
                .err()
                .unwrap()
                .message
                .contains("predominant_height_m")
        );
    }
    #[test]
    fn mesh_intersection_is_checked_in_both_directions() {
        let a = Cell::Triangle([[-1., -1., 0.], [1., -1., 0.], [0., 1., 0.]]);
        let b = Cell::Triangle([[0., -10., -10.], [0., 10., -10.], [0., 0., 10.]]);
        assert!(validate_mesh(&[a, b], &AtomicBool::new(false)).is_err());
        assert!(validate_mesh(&[b, a], &AtomicBool::new(false)).is_err());
        let c = Cell::Triangle([[1., -1., 0.], [0., 1., 0.], [2., 1., 0.]]);
        assert!(validate_mesh(&[a, c], &AtomicBool::new(false)).is_ok());
    }
    #[test]
    fn barrier_self_crossing_and_retracing_are_invalid() {
        assert!(
            validate_open_polyline(&[[0., 0., 0.], [2., 2., 0.], [0., 2., 0.], [2., 0., 0.]])
                .is_err()
        );
        assert!(validate_open_polyline(&[[0., 0., 0.], [2., 0., 0.], [1., 0., 0.]]).is_err());
        assert!(validate_open_polyline(&[[0., 0., 0.], [2., 0., 0.], [2., 2., 0.]]).is_ok());
    }
    #[test]
    fn adjacent_buildings_allowed_but_overlapping_footprints_rejected() {
        let b = building();
        let mut second = building();
        second["id"] = json!("b2");
        second["footprint"] = json!([[2, 0, 0], [6, 0, 0], [6, 4, 0], [2, 4, 0]]);
        let error = prepare(json!({"buildings":[b,second]})).err().unwrap();
        assert_eq!(error.code, "invalid_input");
        assert_eq!(error.field.as_deref(), Some("buildings[1]"));
        assert_eq!(
            error.message,
            "Building b2: Building footprint interiors overlap building b"
        );
    }
    #[test]
    fn grid_tolerance_adds_at_most_one_upper_endpoint() {
        assert_eq!(grid_axis_count(0., 1e-8, 1e-9).unwrap(), 12.);
        let p = [[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.]];
        assert!(grid_contains([1. + 5e-9, 0.5, 0.], &p));
        assert!(!grid_contains([1. + 2e-8, 0.5, 0.], &p));
    }
    #[test]
    fn generated_receiver_order_ids_and_building_exclusion() {
        let s=prepare(json!({"area_receivers":[{"id":"grid","boundary":[[-1,-1],[5,-1],[5,5],[-1,5]],"spacing_m":2,"height_m":20}],"buildings":[building()]})).unwrap();
        assert_eq!(s.receivers.len(), 12);
        assert_eq!(s.receivers[0].point, [-1., -1., 20.]);
        assert_eq!(s.receivers[1].point, [1., -1., 20.]);
        assert_eq!(
            s.receivers[0].id,
            "area:Z3JpZA:bff0000000000000:bff0000000000000"
        );
    }
    #[test]
    fn wall_attachment_preserves_height_and_outward_displacement() {
        let mut s = source("point");
        s["host_building_id"] = json!("b");
        s["host_face_id"] = json!("wall:0:1");
        s["attachment"] = json!({"placement_profile":"acalc_schema11","x_m":1,"y_m":2,"mounting":"standoff","distance_m":3});
        let scene = prepare(json!({"sources":[s],"buildings":[building()]})).unwrap();
        assert_eq!(scene.sources[0].zones[0].cells[0].centroid(), [1., -3., 2.]);
        assert!(scene.sources[0].zones[0].host.is_none());
    }
    #[test]
    fn clockwise_authored_wall_attachment_keeps_outside_view_frame() {
        let mut b = building();
        b["footprint"] = json!([[0, 0, 0], [0, 4, 0], [4, 4, 0], [4, 0, 0]]);
        b["height_m"] = json!(5);
        let mut s = source("point");
        s["host_building_id"] = json!("b");
        s["host_face_id"] = json!("wall:1:0");
        s["attachment"] = json!({"placement_profile":"acalc_schema11","x_m":1,"y_m":2,"mounting":"standoff","distance_m":3});
        let scene = prepare(json!({"sources":[s],"buildings":[b]})).unwrap();
        assert_eq!(scene.sources[0].zones[0].cells[0].centroid(), [-3., 3., 3.]);
    }

    #[test]
    fn roof_building_local_frame_rotates_with_authored_edge() {
        let mut b = building();
        b["footprint"] = json!([[0, 0, 0], [0, 4, 0], [-4, 4, 0], [-4, 0, 0]]);
        let mut s = source("point");
        s["host_building_id"] = json!("b");
        s["host_face_id"] = json!("roof");
        s["attachment"] = json!({"placement_profile":"acalc_schema11","frame":"building_local","x_m":1,"y_m":2,"mounting":"surface"});
        let scene = prepare(json!({"sources":[s],"buildings":[b]})).unwrap();
        assert_eq!(scene.sources[0].zones[0].cells[0].centroid(), [-2., 1., 4.]);
        assert!(distance(scene.sources[0].centre, [-2., 1., 4.]) < 1e-12);
    }
    fn wall_point(mounting: &str, distance: f64) -> Result<Scene, Diagnostic> {
        let mut s = source("point");
        s["host_building_id"] = json!("b");
        s["host_face_id"] = json!("wall:0:1");
        s["attachment"] = json!({"placement_profile":"acalc_schema11","x_m":1,"y_m":2,"mounting":mounting,"distance_m":distance});
        prepare(json!({"sources":[s],"buildings":[building()]}))
    }
    #[test]
    fn surface_point_lies_on_host_and_standoff_point_is_unhosted() {
        let surface = wall_point("surface", 0.).unwrap();
        let zone = &surface.sources[0].zones[0];
        assert_eq!(zone.cells[0].centroid(), [1., 0., 2.]);
        assert_eq!(surface.faces[zone.host.unwrap()].id, "wall:0:1");
        let standoff = wall_point("standoff", MIN_SOURCE_FACE_CLEARANCE_M).unwrap();
        let zone = &standoff.sources[0].zones[0];
        assert!(distance(zone.cells[0].centroid(), [1., -0.05, 2.]) < 1e-12);
        assert!(zone.host.is_none());
    }
    #[test]
    fn point_sources_near_but_off_a_building_face_are_rejected() {
        let error = wall_point("standoff", 0.01).err().unwrap();
        assert!(
            error.message.contains("at least 0.05 m away"),
            "{}",
            error.message
        );
        let untagged = |y: f64| {
            let mut s = source("point");
            s["position"] = json!([1, y, 2]);
            prepare(json!({"sources":[s],"buildings":[building()]}))
        };
        assert!(untagged(-0.02).is_err());
        // The near side of the solid is already invalid as an interior point.
        assert!(untagged(0.02).is_err());
        assert!(
            untagged(-MIN_SOURCE_FACE_CLEARANCE_M).unwrap().sources[0].zones[0]
                .host
                .is_none()
        );
        assert!(untagged(0.).unwrap().sources[0].zones[0].host.is_some());
        // Beyond the face's extent the plane distance alone is not a contact.
        let mut s = source("point");
        s["position"] = json!([5, -0.02, 2]);
        assert!(prepare(json!({"sources":[s],"buildings":[building()]})).is_ok());
    }
    #[test]
    fn attached_power_is_actual_area_without_automatic_gain() {
        let mut s = source("area");
        s["host_building_id"] = json!("b");
        s["host_face_id"] = json!("wall:0:1");
        s["power_basis"] = json!("power_per_m2");
        s["attachment"] =
            json!({"placement_profile":"acalc_schema11","x_m":1,"y_m":1,"width_m":2,"height_m":2});
        let scene = prepare(json!({"sources":[s],"buildings":[building()]})).unwrap();
        assert_eq!(scene.sources[0].zones[0].measure, 4.);
        assert_eq!(scene.sources[0].zones[0].power, [4e8; 8]);
    }
}

#[cfg(test)]
mod pair_count_tests {
    use super::*;

    #[test]
    fn preparation_accepts_more_than_one_million_pairs() {
        let request = json!({
            "sources": (0..1001).map(|i| json!({"id":format!("s{i}"),"kind":"point","position":[0,0,2],"spectrum":[80,80,80,80,80,80,80,80]})).collect::<Vec<_>>(),
            "receivers": (0..1000).map(|i| json!({"id":format!("r{i}"),"position":[100,i,2]})).collect::<Vec<_>>()
        });
        let cancel = AtomicBool::new(false);
        let scene = Scene::prepare(&request, &cancel).unwrap();
        assert_eq!(scene.sources.len() * scene.receivers.len(), 1_001_000);
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(Scene::prepare(&request, &cancel).is_err());
    }
}

#[cfg(test)]
mod segment_contact_tests {
    use super::*;
    #[test]
    fn near_parallel_shared_endpoint_remains_on_both_segments() {
        // Retained geometric inputs; the independent answer follows solely
        // from the identical endpoint, not a production acoustic result.
        let a = [600720.5297801523, 300604.2736134132, 0.];
        let b = [600725.4150408071, 300600.64208072826, 0.];
        let c = [600735.8112765217, 300592.9138810547, 0.];
        for offset in [[0.; 3], [-600000., -300000., 0.]] {
            let shift = |p| crate::geometry::add(p, offset);
            assert_eq!(
                segment_cross(shift(a), shift(b), shift(c), shift(b)),
                Some((1., 1.))
            );
            assert_eq!(
                segment_cross(shift(b), shift(a), shift(c), shift(b)),
                Some((0., 1.))
            );
            assert_eq!(
                segment_cross(shift(a), shift(b), shift(b), shift(c)),
                Some((1., 0.))
            );
            assert_eq!(
                segment_cross(shift(b), shift(a), shift(b), shift(c)),
                Some((0., 0.))
            );
            assert_eq!(segment_cross(shift(a), shift(b), shift(a), shift(b)), None);
            assert_eq!(segment_cross(shift(b), shift(b), shift(c), shift(b)), None);
        }
    }
    #[test]
    fn near_parallel_interior_crossing_retains_exact_half_parameters() {
        // Authored integer segments have their unique intersection at the
        // midpoint of each: (500000,499999.5). Determinant magnitude is 2.
        for offset in [[0.; 3], [600000., 300000., 200.]] {
            let shift = |p| crate::geometry::add(p, offset);
            assert_eq!(
                segment_cross(
                    shift([0., 0., 0.]),
                    shift([1e6, 999999., 0.]),
                    shift([1., 1., 0.]),
                    shift([999999., 999998., 0.])
                ),
                Some((0.5, 0.5))
            );
        }
    }
}
