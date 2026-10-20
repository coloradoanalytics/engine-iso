//! Strict wire validation for the shared API revision 1.1.
//!
//! Structural JSON errors are distinct from invalid acoustic values. Geometry
//! preparation performs the remaining topology, terrain, contact and supported
//! combination checks before a result can be published.

use crate::types::Diagnostic;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

type Result<T = ()> = std::result::Result<T, Diagnostic>;
type Object = Map<String, Value>;

fn error(code: &str, field: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        message: message.into(),
        field: Some(field.into()),
    }
}
fn structural(field: &str, message: impl Into<String>) -> Diagnostic {
    error("invalid_json", field, message)
}
fn invalid(field: &str, message: impl Into<String>) -> Diagnostic {
    error("invalid_input", field, message)
}
fn unsupported(field: &str, message: impl Into<String>) -> Diagnostic {
    error("unsupported", field, message)
}
fn path(base: &str, key: &str) -> String {
    if base.is_empty() {
        key.into()
    } else {
        format!("{base}.{key}")
    }
}
fn object<'a>(v: &'a Value, p: &str, allowed: &[&str]) -> Result<&'a Object> {
    let o = v
        .as_object()
        .ok_or_else(|| structural(p, "Expected a JSON object."))?;
    if let Some(k) = o.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(structural(&path(p, k), "Unknown request field."));
    }
    Ok(o)
}
fn required<'a>(o: &'a Object, k: &str, p: &str) -> Result<&'a Value> {
    o.get(k)
        .ok_or_else(|| invalid(&path(p, k), "Required field is missing."))
}
fn array<'a>(v: &'a Value, p: &str, min: usize, max: usize) -> Result<&'a Vec<Value>> {
    let a = v
        .as_array()
        .ok_or_else(|| structural(p, "Expected an array."))?;
    if a.len() < min || a.len() > max {
        return Err(invalid(
            p,
            format!("Array must contain between {min} and {max} entries."),
        ));
    }
    Ok(a)
}
fn string<'a>(v: &'a Value, p: &str) -> Result<&'a str> {
    v.as_str()
        .ok_or_else(|| structural(p, "Expected a string."))
}
fn id<'a>(v: &'a Value, p: &str) -> Result<&'a str> {
    let s = string(v, p)?;
    if s.is_empty() {
        return Err(invalid(p, "Identifier must be nonempty."));
    }
    Ok(s)
}
fn number(v: &Value, p: &str) -> Result<f64> {
    let n = v
        .as_f64()
        .ok_or_else(|| structural(p, "Expected a finite JSON number."))?;
    if !n.is_finite() {
        return Err(structural(p, "Number must be finite."));
    }
    Ok(n)
}
fn range(v: &Value, p: &str, lo: f64, hi: f64, open_lo: bool) -> Result<f64> {
    let n = number(v, p)?;
    if n < lo || n > hi || (open_lo && n == lo) {
        return Err(invalid(
            p,
            format!(
                "Value is outside the supported range {}{lo}, {hi}].",
                if open_lo { "(" } else { "[" }
            ),
        ));
    }
    Ok(n)
}
fn integer(v: &Value, p: &str, lo: u64, hi: u64) -> Result<u64> {
    if !v.is_number() {
        return Err(structural(p, "Expected an integer."));
    }
    let n = v
        .as_u64()
        .ok_or_else(|| invalid(p, "Expected a nonnegative integer."))?;
    if n < lo || n > hi {
        return Err(invalid(p, format!("Integer must be in {lo}..={hi}.")));
    }
    Ok(n)
}
fn boolean(v: &Value, p: &str) -> Result<bool> {
    v.as_bool()
        .ok_or_else(|| structural(p, "Expected a boolean."))
}
fn choice<'a>(v: &'a Value, p: &str, allowed: &[&str]) -> Result<&'a str> {
    let s = string(v, p)?;
    if !allowed.contains(&s) {
        return Err(unsupported(
            p,
            format!(
                "Unsupported value {s:?}; expected one of {}.",
                allowed.join(", ")
            ),
        ));
    }
    Ok(s)
}
fn coordinate(v: &Value, p: &str, n: usize) -> Result {
    for (i, value) in array(v, p, n, n)?.iter().enumerate() {
        range(value, &format!("{p}[{i}]"), -1e9, 1e9, false)?;
    }
    Ok(())
}
fn spectrum(v: &Value, p: &str) -> Result {
    for (i, x) in array(v, p, 8, 8)?.iter().enumerate() {
        if !x.is_null() {
            range(x, &format!("{p}[{i}]"), -200., 300., false)?;
        }
    }
    Ok(())
}
fn material(v: &Value, p: &str, nullable: bool) -> Result {
    if nullable && v.is_null() {
        return Ok(());
    }
    for (i, x) in array(v, p, 8, 8)?.iter().enumerate() {
        range(x, &format!("{p}[{i}]"), 0., 1., false)?;
    }
    Ok(())
}
fn coords(v: &Value, p: &str, dimension: usize, min: usize) -> Result {
    for (i, x) in array(v, p, min, 10_000)?.iter().enumerate() {
        coordinate(x, &format!("{p}[{i}]"), dimension)?;
    }
    Ok(())
}
fn triangles(v: &Value, p: &str) -> Result {
    for (i, t) in array(v, p, 1, 10_000)?.iter().enumerate() {
        coords(t, &format!("{p}[{i}]"), 3, 3)?;
        if t.as_array().unwrap().len() != 3 {
            return Err(invalid(p, "A triangle has exactly three vertices."));
        }
    }
    Ok(())
}
fn optional_number(
    o: &Object,
    k: &str,
    p: &str,
    lo: f64,
    hi: f64,
    positive: bool,
    nullable: bool,
) -> Result {
    if let Some(v) = o.get(k)
        && !(nullable && v.is_null())
    {
        range(v, &path(p, k), lo, hi, positive)?;
    }
    Ok(())
}
fn unique<'a>(o: &'a Object, p: &str, seen: &mut HashSet<String>) -> Result<&'a str> {
    let name = id(required(o, "id", p)?, &path(p, "id"))?;
    if !seen.insert(name.into()) {
        return Err(invalid(&path(p, "id"), "Duplicate identifier."));
    }
    Ok(name)
}
fn exactly_one(o: &Object, keys: &[&str], p: &str) -> Result {
    if keys.iter().filter(|k| o.contains_key(**k)).count() != 1 {
        return Err(invalid(
            p,
            format!("Supply exactly one of {}.", keys.join(", ")),
        ));
    }
    Ok(())
}

#[derive(Default)]
struct Context {
    inference: HashSet<String>,
    inference_xy: HashMap<String, [f64; 2]>,
    ground_references: Vec<(String, String)>,
    uses_ground_ref: bool,
    staged: bool,
    buildings: HashSet<String>,
    hosts: Vec<(String, String)>,
}
fn ground_ref(v: &Value, p: &str, c: &mut Context) -> Result {
    let o = object(v, p, &["position", "inference_id"])?;
    exactly_one(o, &["position", "inference_id"], p)?;
    c.uses_ground_ref = true;
    if let Some(v) = o.get("position") {
        coordinate(v, &path(p, "position"), 3)?;
    }
    if let Some(v) = o.get("inference_id") {
        c.ground_references
            .push((id(v, &path(p, "inference_id"))?.into(), p.into()));
    }
    Ok(())
}
const LOCATION: &[&str] = &[
    "position",
    "position_xy",
    "ground_ref",
    "height_m",
    "elevation_m",
];
fn location(o: &Object, p: &str, c: &mut Context) -> Result {
    exactly_one(o, &["position", "position_xy", "ground_ref"], p)?;
    if let Some(v) = o.get("position") {
        coordinate(v, &path(p, "position"), 3)?;
        if o.contains_key("height_m") || o.contains_key("elevation_m") {
            return Err(invalid(
                p,
                "Absolute position excludes height_m and elevation_m.",
            ));
        }
    } else if let Some(v) = o.get("position_xy") {
        coordinate(v, &path(p, "position_xy"), 2)?;
        range(
            required(o, "height_m", p)?,
            &path(p, "height_m"),
            0.,
            1e9,
            false,
        )?;
        if o.contains_key("elevation_m") {
            return Err(invalid(p, "position_xy excludes elevation_m."));
        }
    } else {
        ground_ref(&o["ground_ref"], &path(p, "ground_ref"), c)?;
        exactly_one(o, &["height_m", "elevation_m"], p)?;
        optional_number(o, "height_m", p, 0., 1e9, false, false)?;
        optional_number(o, "elevation_m", p, -1e9, 1e9, false, false)?;
    }
    Ok(())
}
fn indices(v: &Value, p: &str, vertex_count: usize, require_all: bool) -> Result {
    let mut used = HashSet::new();
    for (i, t) in array(v, p, 1, 10_000)?.iter().enumerate() {
        let tp = format!("{p}[{i}]");
        let mut local = HashSet::new();
        for (j, x) in array(t, &tp, 3, 3)?.iter().enumerate() {
            let n = integer(
                x,
                &format!("{tp}[{j}]"),
                0,
                vertex_count.saturating_sub(1) as u64,
            )? as usize;
            if !local.insert(n) {
                return Err(invalid(&tp, "Triangle indices must be distinct."));
            }
            used.insert(n);
        }
    }
    if require_all && used.len() != vertex_count {
        return Err(invalid(
            p,
            "Indexed emitting geometry contains unused vertices.",
        ));
    }
    Ok(())
}
fn geometry(v: &Value, p: &str, kind: &str, c: &mut Context) -> Result {
    let allowed = if kind == "line" {
        &["vertices"][..]
    } else {
        &["vertices", "triangles"][..]
    };
    let o = object(v, p, allowed)?;
    let a = array(
        required(o, "vertices", p)?,
        &path(p, "vertices"),
        if kind == "line" { 2 } else { 3 },
        10_000,
    )?;
    for (i, x) in a.iter().enumerate() {
        let vp = format!("{p}.vertices[{i}]");
        location(object(x, &vp, LOCATION)?, &vp, c)?;
    }
    if kind == "area" {
        indices(
            required(o, "triangles", p)?,
            &path(p, "triangles"),
            a.len(),
            true,
        )?;
    }
    Ok(())
}
fn directivity(v: &Value, p: &str) -> Result {
    let o = object(
        v,
        p,
        &[
            "azimuth_deg",
            "elevation_deg",
            "correction_db",
            "orientation_deg",
            "orientation_matrix",
            "provenance",
        ],
    )?;
    let az = array(
        required(o, "azimuth_deg", p)?,
        &path(p, "azimuth_deg"),
        1,
        720,
    )?;
    let el = array(
        required(o, "elevation_deg", p)?,
        &path(p, "elevation_deg"),
        2,
        361,
    )?;
    for (name, values, lo, hi) in [
        ("azimuth_deg", az, 0., 360.),
        ("elevation_deg", el, -90., 90.),
    ] {
        let mut previous = f64::NEG_INFINITY;
        for (i, x) in values.iter().enumerate() {
            let n = range(x, &format!("{p}.{name}[{i}]"), lo, hi, false)?;
            if n <= previous || (name == "azimuth_deg" && n == 360.) {
                return Err(invalid(
                    &path(p, name),
                    "Angles must be strictly increasing; azimuth excludes 360 degrees.",
                ));
            }
            previous = n;
        }
    }
    if az[0].as_f64() != Some(0.)
        || el[0].as_f64() != Some(-90.)
        || el.last().unwrap().as_f64() != Some(90.)
    {
        return Err(invalid(
            p,
            "Azimuth must start at 0; elevation must span -90 to 90.",
        ));
    }
    let rows = array(
        required(o, "correction_db", p)?,
        &path(p, "correction_db"),
        el.len(),
        el.len(),
    )?;
    for (r, row) in rows.iter().enumerate() {
        let cols = array(row, &format!("{p}.correction_db[{r}]"), az.len(), az.len())?;
        for (col, cell) in cols.iter().enumerate() {
            let cp = format!("{p}.correction_db[{r}][{col}]");
            for (b, val) in array(cell, &cp, 8, 8)?.iter().enumerate() {
                range(val, &format!("{cp}[{b}]"), -200., 60., false)?;
            }
            if (r == 0 || r + 1 == rows.len())
                && col > 0
                && (0..8).any(|b| cell[b].as_f64() != cols[0][b].as_f64())
            {
                return Err(invalid(
                    &cp,
                    "All azimuth values at a pole must be identical.",
                ));
            }
        }
    }
    if o.contains_key("orientation_deg") && o.contains_key("orientation_matrix") {
        return Err(invalid(p, "Orientation forms are mutually exclusive."));
    }
    if let Some(v) = o.get("orientation_deg") {
        for (i, x) in array(v, &path(p, "orientation_deg"), 3, 3)?
            .iter()
            .enumerate()
        {
            range(x, &format!("{p}.orientation_deg[{i}]"), -360., 360., false)?;
        }
    }
    if let Some(v) = o.get("orientation_matrix") {
        let mut m = [[0.; 3]; 3];
        for (i, row) in array(v, &path(p, "orientation_matrix"), 3, 3)?
            .iter()
            .enumerate()
        {
            for (j, x) in array(row, p, 3, 3)?.iter().enumerate() {
                m[i][j] = number(x, p)?;
            }
        }
        for i in 0..3 {
            for j in 0..3 {
                let dot: f64 = (0..3).map(|k| m[k][i] * m[k][j]).sum();
                if (dot - if i == j { 1. } else { 0. }).abs() > 1e-9 {
                    return Err(invalid(
                        p,
                        "Orientation matrix must be orthonormal within 1e-9.",
                    ));
                }
            }
        }
        let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        if (det - 1.).abs() > 1e-9 {
            return Err(invalid(p, "Orientation matrix determinant must be +1."));
        }
    }
    if let Some(v) = o.get("provenance") {
        let po = object(
            v,
            &path(p, "provenance"),
            &[
                "catalog_id",
                "catalog_revision",
                "quantity",
                "reference",
                "conversion_profile",
                "conversion_revision",
            ],
        )?;
        for (k, x) in po {
            string(x, &format!("{p}.provenance.{k}"))?;
        }
        if let Some(v) = po.get("quantity") {
            choice(v, &format!("{p}.provenance.quantity"), &["correction"])?;
        }
    }
    Ok(())
}
fn attachment(v: &Value, p: &str, kind: &str) -> Result {
    let fields = if kind == "point" {
        &[
            "placement_profile",
            "frame",
            "x_m",
            "y_m",
            "mounting",
            "distance_m",
        ][..]
    } else {
        &[
            "placement_profile",
            "frame",
            "x_m",
            "y_m",
            "width_m",
            "height_m",
        ][..]
    };
    let o = object(v, p, fields)?;
    choice(
        required(o, "placement_profile", p)?,
        &path(p, "placement_profile"),
        &["acalc_schema11"],
    )?;
    if let Some(v) = o.get("frame") {
        choice(
            v,
            &path(p, "frame"),
            &["building_local", "roof_top_edge", "legacy_world"],
        )?;
    }
    for k in ["x_m", "y_m"] {
        range(required(o, k, p)?, &path(p, k), 0., 1e9, false)?;
    }
    if kind == "point" {
        // Surface openings lie on the face; standoff units keep a minimum
        // clearance so the host image source is never near-coincident.
        let mounting = choice(
            required(o, "mounting", p)?,
            &path(p, "mounting"),
            &["surface", "standoff"],
        )?;
        if mounting == "surface" {
            if let Some(v) = o.get("distance_m") {
                range(v, &path(p, "distance_m"), 0., 0., false)?;
            }
        } else {
            range(
                required(o, "distance_m", p)?,
                &path(p, "distance_m"),
                crate::scene::MIN_SOURCE_FACE_CLEARANCE_M,
                1e9,
                false,
            )?;
        }
    } else {
        for k in ["width_m", "height_m"] {
            range(required(o, k, p)?, &path(p, k), 0., 1e9, true)?;
        }
    }
    Ok(())
}
fn source(v: &Value, p: &str, c: &mut Context, ids: &mut HashSet<String>) -> Result {
    let o = object(
        v,
        p,
        &[
            "id",
            "kind",
            "spectrum",
            "power_basis",
            "position",
            "position_xy",
            "ground_ref",
            "height_m",
            "elevation_m",
            "triangles",
            "vertices",
            "geometry",
            "zones",
            "directivity",
            "predominant_height_m",
            "host_building_id",
            "host_face_id",
            "attachment",
        ],
    )?;
    unique(o, p, ids)?;
    let kind = choice(
        required(o, "kind", p)?,
        &path(p, "kind"),
        &["point", "line", "area"],
    )?;
    if let Some(v) = o.get("spectrum") {
        spectrum(v, &path(p, "spectrum"))?;
    }
    if let Some(v) = o.get("power_basis") {
        power_basis(v, &path(p, "power_basis"), kind)?;
    }
    if let Some(v) = o.get("directivity") {
        directivity(v, &path(p, "directivity"))?;
    }
    optional_number(o, "predominant_height_m", p, 0., 1e9, false, false)?;
    if let Some(v) = o.get("host_building_id") {
        c.hosts
            .push((id(v, &path(p, "host_building_id"))?.into(), p.into()));
    }
    if let Some(v) = o.get("host_face_id") {
        id(v, &path(p, "host_face_id"))?;
        if !o.contains_key("host_building_id") {
            return Err(invalid(p, "host_face_id requires host_building_id."));
        }
    }
    if kind == "point" {
        for k in ["vertices", "triangles", "geometry", "zones"] {
            if o.contains_key(k) {
                return Err(invalid(
                    &path(p, k),
                    "Point sources cannot contain extended geometry or zones.",
                ));
            }
        }
        spectrum(required(o, "spectrum", p)?, &path(p, "spectrum"))?;
        if let Some(v) = o.get("attachment") {
            attachment(v, &path(p, "attachment"), kind)?;
            attachment_host(o, p)?;
            if LOCATION.iter().any(|k| o.contains_key(*k)) {
                return Err(invalid(p, "Attachment excludes other location fields."));
            }
        } else {
            location(o, p, c)?;
        }
        return Ok(());
    }
    if LOCATION.iter().any(|k| o.contains_key(*k)) {
        return Err(invalid(
            p,
            "Extended sources cannot contain point location fields.",
        ));
    }
    if let Some(zones) = o.get("zones") {
        if ["vertices", "triangles", "geometry", "attachment"]
            .iter()
            .any(|k| o.contains_key(*k))
        {
            return Err(invalid(p, "Parent and zone geometry cannot be mixed."));
        }
        let a = array(zones, &path(p, "zones"), 1, 10_000)?;
        let inherited = o.contains_key("spectrum");
        let mut zone_ids = HashSet::new();
        for (i, z) in a.iter().enumerate() {
            let zp = format!("{p}.zones[{i}]");
            let zo = object(
                z,
                &zp,
                &[
                    "id",
                    "spectrum",
                    "power_basis",
                    "triangles",
                    "vertices",
                    "geometry",
                    "attachment",
                    "directivity",
                ],
            )?;
            unique(zo, &zp, &mut zone_ids)?;
            if inherited && (zo.contains_key("spectrum") || zo.contains_key("power_basis")) {
                return Err(invalid(
                    &zp,
                    "Inherited zones omit spectrum and power_basis.",
                ));
            }
            if !inherited {
                spectrum(required(zo, "spectrum", &zp)?, &path(&zp, "spectrum"))?;
            }
            if let Some(v) = zo.get("power_basis") {
                power_basis(v, &path(&zp, "power_basis"), kind)?;
            }
            if let Some(v) = zo.get("directivity") {
                directivity(v, &path(&zp, "directivity"))?;
            }
            extended_geometry(zo, &zp, kind, c)?;
            if zo.contains_key("attachment") {
                attachment_host(o, p)?;
            }
        }
    } else {
        spectrum(required(o, "spectrum", p)?, &path(p, "spectrum"))?;
        extended_geometry(o, p, kind, c)?;
        if o.contains_key("attachment") {
            attachment_host(o, p)?;
        }
    }
    Ok(())
}
fn attachment_host(o: &Object, p: &str) -> Result {
    for k in ["host_building_id", "host_face_id"] {
        id(required(o, k, p)?, &path(p, k))?;
    }
    Ok(())
}
fn power_basis(v: &Value, p: &str, kind: &str) -> Result {
    choice(
        v,
        p,
        match kind {
            "point" => &["total_power"][..],
            "line" => &["total_power", "power_per_m"][..],
            _ => &["total_power", "power_per_m2"][..],
        },
    )?;
    Ok(())
}
fn extended_geometry(o: &Object, p: &str, kind: &str, c: &mut Context) -> Result {
    exactly_one(o, &["vertices", "triangles", "geometry", "attachment"], p)?;
    if let Some(v) = o.get("geometry") {
        geometry(v, &path(p, "geometry"), kind, c)?;
    }
    if let Some(v) = o.get("vertices") {
        if kind != "line" {
            return Err(invalid(p, "Area sources require triangle connectivity."));
        }
        coords(v, &path(p, "vertices"), 3, 2)?;
    }
    if let Some(v) = o.get("triangles") {
        if kind != "area" {
            return Err(invalid(p, "Line sources cannot contain triangles."));
        }
        triangles(v, &path(p, "triangles"))?;
    }
    if let Some(v) = o.get("attachment") {
        if kind != "area" {
            return Err(invalid(p, "Line attachment placement is not defined."));
        }
        attachment(v, &path(p, "attachment"), kind)?;
    }
    Ok(())
}

fn terrain(v: &Value, c: &mut Context) -> Result {
    let p = "terrain";
    let o = object(
        v,
        p,
        &[
            "samples",
            "breaklines",
            "construction_profile",
            "inference_points",
            "breakline_definitions",
        ],
    )?;
    if let Some(v) = o.get("construction_profile") {
        choice(v, "terrain.construction_profile", &["acalc_schema11"])?;
        c.staged = true;
    }
    if let Some(v) = o.get("samples") {
        coords(v, "terrain.samples", 3, 0)?;
    }
    if let Some(v) = o.get("breaklines") {
        for (i, x) in array(v, "terrain.breaklines", 0, 10_000)?
            .iter()
            .enumerate()
        {
            coords(x, &format!("terrain.breaklines[{i}]"), 3, 2)?;
        }
    }
    if let Some(v) = o.get("inference_points") {
        for (i, x) in array(v, "terrain.inference_points", 0, 10_000)?
            .iter()
            .enumerate()
        {
            let p = format!("terrain.inference_points[{i}]");
            let o = object(x, &p, &["id", "position_xy"])?;
            let name = unique(o, &p, &mut c.inference)?.to_string();
            coordinate(required(o, "position_xy", &p)?, &path(&p, "position_xy"), 2)?;
            c.inference_xy.insert(
                name,
                [
                    o["position_xy"][0].as_f64().unwrap(),
                    o["position_xy"][1].as_f64().unwrap(),
                ],
            );
        }
    }
    if let Some(v) = o.get("breakline_definitions") {
        let mut seen = HashSet::new();
        for (i, x) in array(v, "terrain.breakline_definitions", 0, 10_000)?
            .iter()
            .enumerate()
        {
            let p = format!("terrain.breakline_definitions[{i}]");
            let o = object(x, &p, &["id", "vertices"])?;
            unique(o, &p, &mut seen)?;
            for (j, x) in array(
                required(o, "vertices", &p)?,
                &path(&p, "vertices"),
                2,
                10_000,
            )?
            .iter()
            .enumerate()
            {
                ground_ref(x, &format!("{p}.vertices[{j}]"), c)?;
            }
        }
    }
    if (o.contains_key("inference_points") || o.contains_key("breakline_definitions")) && !c.staged
    {
        return Err(invalid(
            p,
            "Staged terrain fields require construction_profile.",
        ));
    }
    Ok(())
}
fn ground(v: &Value) -> Result {
    let o = object(
        v,
        "ground",
        &["factor", "elevation_m", "regions", "region_policy"],
    )?;
    optional_number(o, "factor", "ground", 0., 1., false, false)?;
    optional_number(o, "elevation_m", "ground", -1e9, 1e9, false, false)?;
    if let Some(v) = o.get("region_policy") {
        choice(
            v,
            "ground.region_policy",
            &["ordered_last_wins", "nonoverlapping_schema11"],
        )?;
    }
    if let Some(v) = o.get("regions") {
        let mut seen = HashSet::new();
        for (i, x) in array(v, "ground.regions", 0, 10_000)?.iter().enumerate() {
            let p = format!("ground.regions[{i}]");
            let o = object(x, &p, &["id", "boundary", "factor"])?;
            unique(o, &p, &mut seen)?;
            coords(required(o, "boundary", &p)?, &path(&p, "boundary"), 2, 3)?;
            range(
                required(o, "factor", &p)?,
                &path(&p, "factor"),
                0.,
                1.,
                false,
            )?;
        }
    }
    Ok(())
}
fn settings(v: &Value) -> Result {
    let p = "settings";
    let o = object(
        v,
        p,
        &[
            "ground_method",
            "max_reflect_order",
            "minimum_contribution_db",
            "receiver_margin_db", // Legacy input only; not a resolved solver setting.
            "area_raster_factor",
            "area_max_extent_m",
            "area_relative_tolerance",
            "area_absolute_tolerance",
            "area_max_depth",
            "meteorological_mode",
            "c0",
            "pruning_strategy",
        ],
    )?;
    if let Some(v) = o.get("pruning_strategy").filter(|v| !v.is_null()) {
        choice(v, "settings.pruning_strategy", &["dba", "dbc"])?;
        if o.get("minimum_contribution_db")
            .is_some_and(|c| !c.is_null())
        {
            return Err(invalid(
                "settings.pruning_strategy",
                "A pruning strategy sets its own thresholds; send minimum_contribution_db as null.",
            ));
        }
    }
    if let Some(v) = o.get("ground_method") {
        choice(
            v,
            "settings.ground_method",
            &["general", "mean_path_height"],
        )?;
    }
    if let Some(v) = o.get("meteorological_mode") {
        choice(
            v,
            "settings.meteorological_mode",
            &["downwind", "long_term"],
        )?;
    }
    for (k, lo, hi) in [("max_reflect_order", 0, 5), ("area_max_depth", 1, 24)] {
        if let Some(v) = o.get(k) {
            integer(v, &path(p, k), lo, hi)?;
        }
    }
    optional_number(
        o,
        "minimum_contribution_db",
        p,
        -f64::MAX,
        f64::MAX,
        false,
        true,
    )?;
    for k in ["receiver_margin_db", "area_max_extent_m"] {
        optional_number(o, k, p, 0., f64::MAX, true, true)?;
    }
    optional_number(o, "area_raster_factor", p, 0., 0.5, true, false)?;
    optional_number(o, "area_relative_tolerance", p, 0., 1., true, false)?;
    optional_number(o, "area_absolute_tolerance", p, 0., f64::MAX, false, false)?;
    optional_number(o, "c0", p, 0., f64::MAX, false, false)?;
    if o.get("meteorological_mode").and_then(Value::as_str) == Some("long_term") {
        required(o, "c0", p)?;
    } else if o.get("c0").and_then(Value::as_f64).unwrap_or(0.) != 0. {
        return Err(invalid(
            "settings.c0",
            "Downwind calculations require c0=0.",
        ));
    }
    Ok(())
}
fn scalar_array(v: &Value, p: &str, n: usize, lo: f64, positive: bool) -> Result {
    for (i, x) in array(v, p, n, n)?.iter().enumerate() {
        range(x, &format!("{p}[{i}]"), lo, 1e9, positive)?;
    }
    Ok(())
}
fn base_refs(v: &Value, p: &str, min: usize, c: &mut Context) -> Result<usize> {
    let a = array(v, p, min, 10_000)?;
    for (i, x) in a.iter().enumerate() {
        ground_ref(x, &format!("{p}[{i}]"), c)?;
    }
    Ok(a.len())
}
fn perimeter_count(v: &Value, c: &Context) -> usize {
    let a = v.as_array().unwrap();
    let xy = |value: &Value| -> Option<[f64; 2]> {
        if let Some(id) = value.get("inference_id").and_then(Value::as_str) {
            return c.inference_xy.get(id).copied();
        }
        let value = value.get("position").unwrap_or(value);
        let values = value.as_array()?;
        Some([values.first()?.as_f64()?, values.get(1)?.as_f64()?])
    };
    // Authored integer and floating-point spellings represent the same vertex.
    // Reference IDs can differ while resolving to the same closing XY point.
    let repeated = a.len() > 1
        && (a.first() == a.last()
            || matches!((xy(&a[0]),xy(a.last().unwrap())),(Some(first),Some(last)) if first==last));
    a.len() - usize::from(repeated)
}
fn roof(v: &Value, p: &str, n: usize) -> Result {
    let o = object(
        v,
        p,
        &[
            "mode",
            "height_m",
            "elevation_m",
            "heights_m",
            "elevations_m",
        ],
    )?;
    let mode = choice(
        required(o, "mode", p)?,
        &path(p, "mode"),
        &[
            "average",
            "highest",
            "lowest",
            "absolute",
            "individual",
            "absolute_individual",
        ],
    )?;
    let field = match mode {
        "absolute" => "elevation_m",
        "individual" => "heights_m",
        "absolute_individual" => "elevations_m",
        _ => "height_m",
    };
    if o.keys().any(|k| k != "mode" && k != field) {
        return Err(invalid(p, "Roof contains data for a different mode."));
    }
    let value = required(o, field, p)?;
    match field {
        "heights_m" => scalar_array(value, &path(p, field), n, 0., false)?,
        "elevations_m" => scalar_array(value, &path(p, field), n, -1e9, false)?,
        "height_m" => {
            range(value, &path(p, field), 0., 1e9, false)?;
        }
        _ => {
            range(value, &path(p, field), -1e9, 1e9, false)?;
        }
    }
    Ok(())
}
fn obstacle(v: &Value, p: &str, kind: &str, c: &mut Context, ids: &mut HashSet<String>) -> Result {
    let allowed = match kind {
        "buildings" => &[
            "id",
            "footprint",
            "ground_vertices",
            "height_m",
            "roof_elevations_m",
            "roof",
            "roof_triangles",
            "reflecting_faces",
            "face_absorption",
            "absorption",
        ][..],
        "barriers" => &[
            "id",
            "start",
            "end",
            "height_m",
            "vertices",
            "ground_vertices",
            "heights_m",
            "top_elevations_m",
            "surface_density_kg_m2",
            "closed",
            "role",
            "opacity",
            "absorption",
            "segment_absorption",
        ][..],
        _ => &[
            "id",
            "start",
            "end",
            "height_m",
            "vertices",
            "absorption",
            "side",
        ][..],
    };
    let o = object(v, p, allowed)?;
    let name = unique(o, p, ids)?;
    if let Some(v) = o.get("absorption") {
        material(v, &path(p, "absorption"), true)?;
    }
    if kind == "buildings" {
        c.buildings.insert(name.into());
        exactly_one(o, &["footprint", "ground_vertices"], p)?;
        exactly_one(o, &["height_m", "roof_elevations_m", "roof"], p)?;
        let n = if let Some(v) = o.get("footprint") {
            coords(v, &path(p, "footprint"), 3, 3)?;
            perimeter_count(v, c)
        } else {
            base_refs(&o["ground_vertices"], &path(p, "ground_vertices"), 3, c)?;
            perimeter_count(&o["ground_vertices"], c)
        };
        optional_number(o, "height_m", p, 0., 1e9, false, false)?;
        if let Some(v) = o.get("roof_elevations_m") {
            scalar_array(v, &path(p, "roof_elevations_m"), n, -1e9, false)?;
        }
        if let Some(v) = o.get("roof") {
            roof(v, &path(p, "roof"), n)?;
        }
        if let Some(v) = o.get("roof_triangles") {
            indices(v, &path(p, "roof_triangles"), n, true)?;
        }
        if let Some(v) = o.get("reflecting_faces") {
            let mut seen = HashSet::new();
            for (i, x) in array(v, &path(p, "reflecting_faces"), 0, 20_000)?
                .iter()
                .enumerate()
            {
                let f = id(x, &format!("{p}.reflecting_faces[{i}]"))?;
                if !seen.insert(f) {
                    return Err(invalid(p, "Duplicate reflecting face."));
                }
            }
        }
        if let Some(v) = o.get("face_absorption") {
            let m = v.as_object().ok_or_else(|| {
                structural(
                    &path(p, "face_absorption"),
                    "Expected a material mapping object.",
                )
            })?;
            for (k, x) in m {
                if k.is_empty() {
                    return Err(invalid(p, "Face ID must be nonempty."));
                }
                material(x, &format!("{p}.face_absorption.{k}"), false)?;
            }
        }
    } else {
        exactly_one(
            o,
            if kind == "barriers" {
                &["start", "vertices", "ground_vertices"][..]
            } else {
                &["start", "vertices"][..]
            },
            p,
        )?;
        let n = if let Some(v) = o.get("start") {
            coordinate(v, &path(p, "start"), 3)?;
            coordinate(required(o, "end", p)?, &path(p, "end"), 3)?;
            range(
                required(o, "height_m", p)?,
                &path(p, "height_m"),
                1e-6,
                1e9,
                true,
            )?;
            if o.contains_key("heights_m") || o.contains_key("top_elevations_m") {
                return Err(invalid(p, "Legacy rectangle uses only height_m."));
            }
            2
        } else {
            if o.contains_key("end") || o.contains_key("height_m") {
                return Err(invalid(
                    p,
                    "Polyline/polygon excludes legacy end and height_m.",
                ));
            }
            if let Some(v) = o.get("vertices") {
                coords(
                    v,
                    &path(p, "vertices"),
                    3,
                    if kind == "barriers" { 2 } else { 3 },
                )?;
                v.as_array().unwrap().len()
            } else {
                base_refs(&o["ground_vertices"], &path(p, "ground_vertices"), 2, c)?
            }
        };
        if kind == "barriers" {
            if !o.contains_key("start") {
                exactly_one(o, &["heights_m", "top_elevations_m"], p)?;
                if let Some(v) = o.get("heights_m") {
                    scalar_array(v, &path(p, "heights_m"), n, 0., true)?;
                }
                if let Some(v) = o.get("top_elevations_m") {
                    scalar_array(v, &path(p, "top_elevations_m"), n, -1e9, false)?;
                }
            }
            if let Some(v) = o.get("opacity") {
                if o.contains_key("surface_density_kg_m2") || o.contains_key("closed") {
                    return Err(invalid(p, "Opacity declarations are mutually exclusive."));
                }
                let op = path(p, "opacity");
                let oo = object(v, &op, &["kind", "basis"])?;
                choice(
                    required(oo, "kind", &op)?,
                    &path(&op, "kind"),
                    &["declared_opaque"],
                )?;
                if string(required(oo, "basis", &op)?, &path(&op, "basis"))?
                    .trim()
                    .is_empty()
                {
                    return Err(invalid(&op, "Opacity basis must be nonempty."));
                }
            } else {
                range(
                    required(o, "surface_density_kg_m2", p)?,
                    &path(p, "surface_density_kg_m2"),
                    10.,
                    f64::MAX,
                    false,
                )?;
                if !boolean(required(o, "closed", p)?, &path(p, "closed"))? {
                    return Err(invalid(p, "An applicable opaque barrier must be closed."));
                }
            }
            if let Some(v) = o.get("role") {
                choice(v, &path(p, "role"), &["screen", "screen_and_reflector"])?;
            }
            if let Some(v) = o.get("segment_absorption") {
                for (i, x) in array(v, &path(p, "segment_absorption"), n - 1, n - 1)?
                    .iter()
                    .enumerate()
                {
                    material(x, &format!("{p}.segment_absorption[{i}]"), false)?;
                }
            }
        } else if let Some(v) = o.get("side") {
            choice(v, &path(p, "side"), &["both", "front", "back"])?;
        }
    }
    Ok(())
}

/// Fixed-reference transfers use the ordinary scene and job lifecycle. The
/// homogeneous source restriction keeps long-term source centres independent of
/// fitted powers, while relative-only integration avoids a power-dependent floor.
fn source_power_transfers(o: &Object) -> Result {
    let settings = o.get("settings");
    if settings.and_then(|s| s.get("minimum_contribution_db")) != Some(&Value::Null) {
        return Err(invalid(
            "settings.minimum_contribution_db",
            "Source-power transfers require an explicit null cutoff.",
        ));
    }
    if settings
        .and_then(|s| s.get("pruning_strategy"))
        .is_some_and(|v| !v.is_null())
    {
        return Err(unsupported(
            "settings.pruning_strategy",
            "Source-power transfers require all contribution pruning to be disabled.",
        ));
    }
    if settings
        .and_then(|s| s.get("area_absolute_tolerance"))
        .and_then(Value::as_f64)
        != Some(0.)
    {
        return Err(invalid(
            "settings.area_absolute_tolerance",
            "Source-power transfers require zero absolute tolerance and relative-only integration.",
        ));
    }
    if settings
        .and_then(|s| s.get("ground_method"))
        .and_then(Value::as_str)
        .unwrap_or("general")
        != "general"
    {
        return Err(unsupported(
            "settings.ground_method",
            "Octave source-power transfers require the general ground method.",
        ));
    }
    if o.get("area_receivers")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty())
    {
        return Err(unsupported(
            "area_receivers",
            "Source-power transfers require explicit point receivers, not receiver areas.",
        ));
    }
    for (i, source) in o["sources"].as_array().unwrap().iter().enumerate() {
        if source.get("zones").is_some() {
            return Err(unsupported(
                &format!("sources[{i}].zones"),
                "Source-power transfers support homogeneous sources without emission zones.",
            ));
        }
        if !source
            .get("spectrum")
            .and_then(Value::as_array)
            .is_some_and(|bands| bands.len() == 8 && bands.iter().all(|v| v.as_f64() == Some(100.)))
        {
            return Err(invalid(
                &format!("sources[{i}].spectrum"),
                "Source-power transfers require 100 dB in all eight bands in each source's input power basis.",
            ));
        }
    }
    Ok(())
}

/// Validate every accepted request field without inventing acoustic defaults.
pub fn validate(v: &Value) -> Result {
    let o = object(
        v,
        "",
        &[
            "sources",
            "receivers",
            "area_receivers",
            "atmosphere",
            "ground",
            "settings",
            "reflectors",
            "barriers",
            "buildings",
            "misc_regions",
            "profile_id",
            "terrain",
            "applicability",
            "output",
        ],
    )?;
    let mut c = Context::default();
    if let Some(v) = o.get("profile_id") {
        id(v, "profile_id")?;
    }
    if let Some(v) = o.get("terrain") {
        terrain(v, &mut c)?;
    }
    if let Some(v) = o.get("atmosphere") {
        let a = object(
            v,
            "atmosphere",
            &["temperature_c", "relative_humidity_percent", "pressure_kpa"],
        )?;
        optional_number(a, "temperature_c", "atmosphere", -20., 50., false, false)?;
        optional_number(
            a,
            "relative_humidity_percent",
            "atmosphere",
            10.,
            100.,
            false,
            false,
        )?;
        optional_number(a, "pressure_kpa", "atmosphere", 0., 200., true, false)?;
    }
    if let Some(v) = o.get("ground") {
        ground(v)?;
    }
    if let Some(v) = o.get("settings") {
        settings(v)?;
    }
    let mut obstacles = HashSet::new();
    for kind in ["buildings", "barriers", "reflectors"] {
        if let Some(v) = o.get(kind) {
            for (i, x) in array(v, kind, 0, 10_000)?.iter().enumerate() {
                obstacle(x, &format!("{kind}[{i}]"), kind, &mut c, &mut obstacles)?;
            }
        }
    }
    let mut sources = HashSet::new();
    for (i, x) in array(required(o, "sources", "")?, "sources", 1, 10_000)?
        .iter()
        .enumerate()
    {
        source(x, &format!("sources[{i}]"), &mut c, &mut sources)?;
    }
    let mut receiver_count = 0;
    let mut receivers = HashSet::new();
    if let Some(v) = o.get("receivers") {
        for (i, x) in array(v, "receivers", 0, 10_000)?.iter().enumerate() {
            let p = format!("receivers[{i}]");
            let r = object(
                x,
                &p,
                &[
                    "id",
                    "position",
                    "position_xy",
                    "ground_ref",
                    "height_m",
                    "elevation_m",
                ],
            )?;
            unique(r, &p, &mut receivers)?;
            location(r, &p, &mut c)?;
            receiver_count += 1;
        }
    }
    if let Some(v) = o.get("area_receivers") {
        let mut seen = HashSet::new();
        for (i, x) in array(v, "area_receivers", 0, 10_000)?.iter().enumerate() {
            let p = format!("area_receivers[{i}]");
            let a = object(
                x,
                &p,
                &[
                    "id",
                    "boundary",
                    "height_m",
                    "spacing_m",
                    "exclude_polygons",
                ],
            )?;
            unique(a, &p, &mut seen)?;
            coords(required(a, "boundary", &p)?, &path(&p, "boundary"), 2, 3)?;
            optional_number(a, "height_m", &p, 0., 1e9, false, false)?;
            optional_number(a, "spacing_m", &p, 0., f64::MAX, true, false)?;
            if let Some(v) = a.get("exclude_polygons") {
                for (j, x) in array(v, &path(&p, "exclude_polygons"), 0, 10_000)?
                    .iter()
                    .enumerate()
                {
                    coords(x, &format!("{p}.exclude_polygons[{j}]"), 2, 3)?;
                }
            }
            receiver_count += 1;
        }
    }
    if receiver_count == 0 {
        return Err(invalid(
            "receivers",
            "At least one explicit receiver or receiver area is required.",
        ));
    }
    if let Some(v) = o.get("misc_regions") {
        let mut seen = HashSet::new();
        for (i, x) in array(v, "misc_regions", 0, 10_000)?.iter().enumerate() {
            let p = format!("misc_regions[{i}]");
            let raw = x
                .as_object()
                .ok_or_else(|| structural(&p, "Expected an object."))?;
            let kind = string(required(raw, "kind", &p)?, &path(&p, "kind"))?;
            if kind != "foliage" {
                return Err(unsupported(
                    &path(&p, "kind"),
                    "No executable attenuation profile is defined for this miscellaneous region kind.",
                ));
            }
            let m = object(
                x,
                &p,
                &[
                    "id",
                    "kind",
                    "method_profile",
                    "boundary",
                    "canopy_height_m",
                ],
            )?;
            unique(m, &p, &mut seen)?;
            choice(
                required(m, "method_profile", &p)?,
                &path(&p, "method_profile"),
                &["iso9613_2024_foliage_simplified_acalc_v1"],
            )?;
            coords(required(m, "boundary", &p)?, &path(&p, "boundary"), 2, 3)?;
            range(
                required(m, "canopy_height_m", &p)?,
                &path(&p, "canopy_height_m"),
                0.,
                1e9,
                true,
            )?;
        }
    }
    if let Some(v) = o.get("applicability") {
        let a = object(
            v,
            "applicability",
            &[
                "sound_character",
                "ground_character",
                "assessment_quantity",
                "evidence",
            ],
        )?;
        for (k, choices) in [
            (
                "sound_character",
                &["not_pure_tone", "pure_tone", "unknown"][..],
            ),
            (
                "ground_character",
                &["porous_or_predominantly_porous", "other", "unknown"][..],
            ),
            ("assessment_quantity", &["a_weighted", "octave"][..]),
        ] {
            if let Some(v) = a.get(k) {
                choice(v, &path("applicability", k), choices)?;
            }
        }
        if let Some(v) = a.get("evidence") {
            string(v, "applicability.evidence")?;
        }
        if v.get("assessment_quantity").and_then(Value::as_str) == Some("octave")
            && o.get("settings")
                .and_then(|s| s.get("ground_method"))
                .and_then(Value::as_str)
                == Some("mean_path_height")
        {
            return Err(unsupported(
                "applicability.assessment_quantity",
                "Mean-path-height assessment is A-weighted.",
            ));
        }
        if o.get("settings")
            .and_then(|s| s.get("ground_method"))
            .and_then(Value::as_str)
            == Some("mean_path_height")
        {
            for (k, want) in [
                ("sound_character", "not_pure_tone"),
                ("ground_character", "porous_or_predominantly_porous"),
                ("assessment_quantity", "a_weighted"),
            ] {
                if a.get(k).and_then(Value::as_str) != Some(want) {
                    return Err(unsupported(
                        &path("applicability", k),
                        format!("Mean-path-height applicability requires {want}."),
                    ));
                }
            }
        }
    }
    if let Some(v) = o.get("output") {
        let out = object(
            v,
            "output",
            &[
                "prepared_geometry",
                "path_diagnostics",
                "integration_reports",
                "area_source_contributions",
                "source_power_transfers",
            ],
        )?;
        if let Some(enabled) = out.get("source_power_transfers")
            && boolean(enabled, "output.source_power_transfers")?
        {
            source_power_transfers(o)?;
        }
        if let Some(v) = out.get("area_source_contributions") {
            boolean(v, "output.area_source_contributions")?;
        }
        if let Some(v) = out.get("prepared_geometry") {
            boolean(v, "output.prepared_geometry")?;
        }
        if let Some(v) = out.get("path_diagnostics") {
            choice(v, "output.path_diagnostics", &["none", "summary", "full"])?;
        }
        if let Some(v) = out.get("integration_reports") {
            choice(v, "output.integration_reports", &["summary", "full"])?;
        }
    }
    if c.uses_ground_ref && !c.staged {
        return Err(invalid(
            "terrain.construction_profile",
            "ground_ref requires acalc_schema11 staged terrain construction.",
        ));
    }
    for (name, p) in c.ground_references {
        if !c.inference.contains(&name) {
            return Err(invalid(
                &p,
                format!("Unknown ground inference target {name:?}."),
            ));
        }
    }
    for (name, p) in c.hosts {
        if !c.buildings.contains(&name) {
            return Err(invalid(&p, format!("Unknown host building {name:?}.")));
        }
    }
    Ok(())
}

/// Denominator is based only on original sources, before any preparation.
pub fn task_points(v: &Value) -> f64 {
    1. + v["sources"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| match s["kind"].as_str() {
                    Some("area") => 3.,
                    Some("line") => 2.,
                    _ => 1.,
                })
                .sum::<f64>()
        })
        .unwrap_or(0.)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn request() -> Value {
        json!({"sources":[{"id":"s","kind":"point","position":[0,0,1],"spectrum":[0,null,80,80,80,80,80,80]}],"receivers":[{"id":"r","position":[10,0,1]}]})
    }
    #[test]
    fn point_attachments_declare_mounting_and_standoff_clearance() {
        let with = |attachment: Value| {
            let mut v = request();
            v["buildings"] =
                json!([{"id":"b","footprint":[[0,0,0],[4,0,0],[4,4,0],[0,4,0]],"height_m":4}]);
            v["sources"][0] = json!({"id":"s","kind":"point","host_building_id":"b","host_face_id":"wall:0:1",
                "attachment":attachment,"spectrum":[80,80,80,80,80,80,80,80]});
            validate(&v)
        };
        let base = json!({"placement_profile":"acalc_schema11","x_m":1,"y_m":1});
        let mut a = base.clone();
        a["distance_m"] = json!(0);
        assert!(with(a).is_err(), "mounting is required");
        for (mounting, distance, ok) in [
            ("surface", None, true),
            ("surface", Some(0.), true),
            ("surface", Some(0.3), false),
            ("standoff", None, false),
            ("standoff", Some(0.049), false),
            ("standoff", Some(0.05), true),
        ] {
            let mut a = base.clone();
            a["mounting"] = json!(mounting);
            if let Some(d) = distance {
                a["distance_m"] = json!(d);
            }
            assert_eq!(with(a).is_ok(), ok, "{mounting} {distance:?}");
        }
    }
    #[test]
    fn strict_types_and_nulls() {
        let mut v = request();
        assert!(validate(&v).is_ok());
        v["sources"][0]["position"][0] = json!(true);
        assert_eq!(validate(&v).unwrap_err().code, "invalid_json");
        let mut v = request();
        v["settings"] = Value::Null;
        assert_eq!(validate(&v).unwrap_err().code, "invalid_json");
    }
    #[test]
    fn unknown_nested_field_is_not_discarded() {
        let mut v = request();
        v["sources"][0]["directivity"] = json!({"typo":1});
        assert_eq!(validate(&v).unwrap_err().code, "invalid_json");
    }
    #[test]
    fn removed_omission_setting_is_rejected() {
        for value in [Value::Null, json!(0.1)] {
            let mut v = request();
            v["settings"] = json!({"receiver_omission_budget_db":value});
            assert!(validate(&v).is_err());
        }
    }
    #[test]
    fn legacy_receiver_margin_remains_accepted_for_compatibility() {
        let mut v = request();
        v["settings"] = json!({"minimum_contribution_db":0,"receiver_margin_db":30});
        assert!(validate(&v).is_ok());
        v["settings"]["minimum_contribution_db"] = Value::Null;
        assert!(validate(&v).is_ok());
    }
    #[test]
    fn staged_references_resolve_inside_request() {
        let mut v = request();
        v["sources"][0].as_object_mut().unwrap().remove("position");
        v["sources"][0]["ground_ref"] = json!({"inference_id":"missing"});
        v["sources"][0]["height_m"] = json!(1);
        v["terrain"] = json!({"construction_profile":"acalc_schema11","samples":[[0,0,0]],"inference_points":[]});
        assert!(validate(&v).unwrap_err().message.contains("Unknown ground"));
        v["terrain"]["inference_points"] = json!([{"id":"missing","position_xy":[0,0]}]);
        assert!(validate(&v).is_ok());
    }
    #[test]
    fn zones_cannot_duplicate_parent_power() {
        let mut v = request();
        v["sources"][0] = json!({"id":"s","kind":"line","spectrum":[80,80,80,80,80,80,80,80],"zones":[{"id":"z","vertices":[[0,0,1],[1,0,1]],"spectrum":[70,70,70,70,70,70,70,70]}]});
        assert!(validate(&v).is_err());
        v["sources"][0].as_object_mut().unwrap().remove("spectrum");
        assert!(validate(&v).is_ok());
        assert_eq!(task_points(&v), 3.);
    }
    #[test]
    fn improper_orientation_is_rejected() {
        let mut v = request();
        v["sources"][0]["directivity"] = json!({"azimuth_deg":[0],"elevation_deg":[-90,90],"correction_db":[[[0,0,0,0,0,0,0,0]],[[0,0,0,0,0,0,0,0]]],"orientation_matrix":[[1,0,0],[0,1,0],[0,0,-1]]});
        assert!(validate(&v).unwrap_err().message.contains("determinant"));
    }

    #[test]
    fn complete_contract_examples_have_valid_wire_shapes() {
        let contract = include_str!("../docs/API.md");
        let mut checked = 0;
        for block in contract.split("```json\n").skip(1) {
            let Some(json) = block.split("```").next() else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(json) else {
                continue;
            };
            if value.get("sources").is_some()
                && (value.get("receivers").is_some() || value.get("area_receivers").is_some())
            {
                validate(&value).unwrap_or_else(|error| {
                    panic!("Complete API example failed validation: {error}")
                });
                checked += 1;
            }
        }
        assert!(
            checked >= 2,
            "Expected the minimal and combined contract examples."
        );
    }

    #[test]
    fn malformed_nested_values_return_diagnostics_without_panics() {
        let replacements = [
            Value::Null,
            json!(true),
            json!("12"),
            json!(12),
            json!([]),
            json!({}),
            json!([null, null, null]),
        ];
        for replacement in replacements {
            for key in [
                "sources",
                "receivers",
                "settings",
                "atmosphere",
                "terrain",
                "ground",
                "buildings",
                "barriers",
                "reflectors",
                "output",
                "applicability",
            ] {
                let mut value = request();
                value[key] = replacement.clone();
                assert!(
                    std::panic::catch_unwind(|| validate(&value)).is_ok(),
                    "{key}"
                );
            }
            for key in [
                "position",
                "spectrum",
                "kind",
                "directivity",
                "attachment",
                "geometry",
                "zones",
            ] {
                let mut value = request();
                value["sources"][0][key] = replacement.clone();
                assert!(
                    std::panic::catch_unwind(|| validate(&value)).is_ok(),
                    "{key}"
                );
            }
        }
    }

    #[test]
    fn building_array_lengths_use_normalized_closing_vertex() {
        let mut value = request();
        value["buildings"] = json!([{"id":"b","footprint":[[0,0,0],[2,0,0],[0,2,0],[0.0,0.0,0.0]],"roof_elevations_m":[3,3,3]}]);
        assert!(validate(&value).is_ok());
        value["buildings"][0]
            .as_object_mut()
            .unwrap()
            .remove("footprint");
        value["buildings"][0]["ground_vertices"] = json!([{"inference_id":"a"},{"position":[2,0,0]},{"position":[0,2,0]},{"inference_id":"closing"}]);
        value["terrain"] = json!({"construction_profile":"acalc_schema11","samples":[[0,0,0]],"inference_points":[{"id":"a","position_xy":[0,0]},{"id":"closing","position_xy":[0.0,0.0]}]});
        assert!(validate(&value).is_ok());
    }
}
