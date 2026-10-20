#![allow(clippy::cloned_ref_to_slice_refs)]
//! ISO 9613-2:2024 A.2.2 with the API's named Acalc circular-arc profile.
//! Clear vertical reflections unfold into one arc; diffraction supports split
//! arcs. Physical 3D vegetation intervals are united before applying Table A.1.

use crate::geometry::{Terrain, lerp, point_in_polygon_xy};
use crate::scene::{Foliage, Scene};
use crate::types::{Bands, Diagnostic, Point, Sum};

const RADIUS: f64 = 5000.0;

/// Apply the named foliage profile to a completed physical route. A signed
/// near-clear diffraction calculation must pass its unobstructed route here.
pub fn attenuation(
    scene: &Scene,
    route: &[Point],
    diffraction_vertices: &[usize],
) -> Result<Bands, Diagnostic> {
    attenuation_for_length(traversal_length(
        &scene.terrain,
        &scene.foliage,
        route,
        diffraction_vertices,
    )?)
}

/// Published Table A.1 values; the API explicitly assigns exactly 20 m to the
/// first row and no loss below 10 m. These endpoints are application conventions.
pub fn attenuation_for_length(length: f64) -> Result<Bands, Diagnostic> {
    if !length.is_finite() || length < 0.0 {
        return Err(Diagnostic::numerical(
            "Foliage traversal length is outside numerical range",
        ));
    }
    if length < 10.0 {
        return Ok([0.0; 8]);
    }
    if length <= 20.0 {
        return Ok([0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 2.0, 3.0]);
    }
    let used = length.min(200.0);
    Ok([0.02, 0.03, 0.04, 0.05, 0.06, 0.08, 0.09, 0.12].map(|coefficient| coefficient * used))
}

/// Physical metres inside the union of foliage volumes, including repeated
/// traversals. Exposed for independent geometry diagnostics and tests.
pub fn traversal_length(
    terrain: &Terrain,
    regions: &[Foliage],
    route: &[Point],
    diffraction_vertices: &[usize],
) -> Result<f64, Diagnostic> {
    if regions.is_empty() {
        return Ok(0.0);
    }
    if route.len() < 2 || route.iter().flatten().any(|x| !x.is_finite()) {
        return Err(Diagnostic::invalid(
            "Foliage requires a finite source-to-receiver route",
        ));
    }
    let mut boundaries = vec![0];
    for &vertex in diffraction_vertices {
        if vertex == 0 || vertex + 1 >= route.len() {
            return Err(Diagnostic::invalid(
                "Foliage diffraction support must be an interior route vertex",
            ));
        }
        boundaries.push(vertex);
    }
    boundaries.push(route.len() - 1);
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut sum = Sum::default();
    for pair in boundaries.windows(2) {
        sum.add(section_length(terrain, regions, &route[pair[0]..=pair[1]])?);
    }
    let total = sum.total();
    if total.is_finite() && total >= 0.0 {
        Ok(total)
    } else {
        Err(Diagnostic::numerical(
            "Foliage traversal accumulation overflowed",
        ))
    }
}

#[derive(Clone, Copy, Debug)]
struct Arc {
    /// Circle-centre horizontal coordinate in the unfolded section.
    cx: f64,
    z0: f64,
    y0: f64,
}

impl Arc {
    fn new(span: f64, z0: f64, z1: f64) -> Result<Self, Diagnostic> {
        if span <= 0.0 {
            return Err(Diagnostic::unsupported(
                "Relevant vertical foliage rays are outside the circular-arc profile",
            ));
        }
        let dz = z1 - z0;
        let chord = span.hypot(dz);
        if !chord.is_finite() || chord >= 2.0 * RADIUS {
            return Err(Diagnostic::unsupported(
                "Foliage arc chords must be shorter than 10000 m",
            ));
        }
        let offset = ((RADIUS - chord / 2.0) * (RADIUS + chord / 2.0)).sqrt();
        let cx = span / 2.0 + offset * dz / chord;
        let y0 = offset * span / chord - dz / 2.0;
        let y1 = offset * span / chord + dz / 2.0;
        if y0 <= 0.0 || y1 <= 0.0 {
            return Err(Diagnostic::unsupported(
                "Foliage arc turns back horizontally; steep arcs are outside this profile",
            ));
        }
        Ok(Self { cx, z0, y0 })
    }

    fn ordinate(&self, x: f64) -> f64 {
        let u = x - self.cx;
        ((RADIUS - u.abs()) * (RADIUS + u.abs())).max(0.0).sqrt()
    }

    fn height(&self, x: f64) -> f64 {
        // Rationalized difference of square roots avoids loss of the sagitta
        // at short distances. y(x)-y(0)=x(2cx-x)/(y(x)+y(0)).
        self.z0 + x * (2.0 * self.cx - x) / (self.ordinate(x) + self.y0)
    }

    fn length(&self, a: f64, b: f64) -> f64 {
        let dx = b - a;
        let dz = dx * (2.0 * self.cx - a - b) / (self.ordinate(a) + self.ordinate(b));
        2.0 * RADIUS * (dx.hypot(dz) / (2.0 * RADIUS)).clamp(0.0, 1.0).asin()
    }

    fn rebased(&self, origin: f64) -> Self {
        Self {
            cx: self.cx - origin,
            z0: self.height(origin),
            y0: self.ordinate(origin),
        }
    }

    /// All crossings with an affine boundary on [a,b]. Concavity permits at
    /// most two roots; splitting at the derivative zero makes each bracket
    /// monotone, including when both interval endpoints are below the canopy.
    fn boundary_roots(
        &self,
        a: f64,
        b: f64,
        za: f64,
        zb: f64,
        roots: &mut Vec<f64>,
    ) -> Result<(), Diagnostic> {
        let slope = (zb - za) / (b - a);
        let critical = self.cx - RADIUS * slope / slope.hypot(1.0);
        let delta = |x: f64| self.height(x) - (za + slope * (x - a));
        let mut splits = vec![a];
        if critical > a && critical < b {
            splits.push(critical);
        }
        splits.push(b);
        for interval in splits.windows(2) {
            let (mut left, mut right) = (interval[0], interval[1]);
            let (mut fl, fr) = (delta(left), delta(right));
            if !fl.is_finite() || !fr.is_finite() {
                return Err(Diagnostic::numerical(
                    "Foliage boundary evaluation is nonfinite",
                ));
            }
            if fl == 0.0 {
                roots.push(left);
            }
            if fr == 0.0 {
                roots.push(right);
            }
            if fl.signum() == fr.signum() || fl == 0.0 || fr == 0.0 {
                continue;
            }
            for _ in 0..80 {
                let middle = left + (right - left) / 2.0;
                if middle == left || middle == right {
                    break;
                }
                let fm = delta(middle);
                if fm == 0.0 {
                    left = middle;
                    right = middle;
                    break;
                }
                if fm.signum() == fl.signum() {
                    left = middle;
                    fl = fm;
                } else {
                    right = middle;
                }
            }
            roots.push(left + (right - left) / 2.0);
        }
        Ok(())
    }
}

fn push_fraction(ts: &mut Vec<f64>, t: f64) {
    if t.is_finite() && t > 0.0 && t < 1.0 {
        ts.push(t);
    }
}

fn polygon_splits(a: Point, b: Point, polygon: &[Point], ts: &mut Vec<f64>) {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let length2 = dx * dx + dy * dy;
    if length2 == 0.0 {
        return;
    }
    for i in 0..polygon.len() {
        let (p, q) = (polygon[i], polygon[(i + 1) % polygon.len()]);
        let (ex, ey) = (q[0] - p[0], q[1] - p[1]);
        let (px, py) = (p[0] - a[0], p[1] - a[1]);
        let denominator = dx * ey - dy * ex;
        if denominator != 0.0 {
            let t = (px * ey - py * ex) / denominator;
            let u = (px * dy - py * dx) / denominator;
            if (0.0..=1.0).contains(&u) {
                push_fraction(ts, t);
            }
        } else if px * dy - py * dx == 0.0 {
            push_fraction(ts, (px * dx + py * dy) / length2);
            push_fraction(ts, ((q[0] - a[0]) * dx + (q[1] - a[1]) * dy) / length2);
        }
    }
}

fn candidates(a: Point, b: Point, regions: &[Foliage]) -> Vec<usize> {
    regions
        .iter()
        .enumerate()
        .filter_map(|(i, region)| {
            let mut ts = vec![0.0, 1.0];
            polygon_splits(a, b, &region.polygon, &mut ts);
            ts.sort_by(f64::total_cmp);
            ts.dedup();
            if point_in_polygon_xy(a, &region.polygon)
                || point_in_polygon_xy(b, &region.polygon)
                || ts
                    .windows(2)
                    .any(|w| point_in_polygon_xy(lerp(a, b, (w[0] + w[1]) / 2.0), &region.polygon))
            {
                Some(i)
            } else {
                None
            }
        })
        .collect()
}

fn section_length(
    terrain: &Terrain,
    regions: &[Foliage],
    route: &[Point],
) -> Result<f64, Diagnostic> {
    let lengths: Vec<f64> = route
        .windows(2)
        .map(|w| (w[0][0] - w[1][0]).hypot(w[0][1] - w[1][1]))
        .collect();
    let selected: Vec<Vec<usize>> = route
        .windows(2)
        .map(|w| candidates(w[0], w[1], regions))
        .collect();
    if selected.iter().all(Vec::is_empty) {
        return Ok(0.0);
    }
    let horizontal: f64 = lengths.iter().sum();
    let arc = Arc::new(horizontal, route[0][2], route[route.len() - 1][2])?;
    let mut sum = Sum::default();
    let mut offset = 0.0;
    for (index, leg) in route.windows(2).enumerate() {
        let length = lengths[index];
        if length == 0.0 {
            if leg[0] != leg[1] && !selected[index].is_empty() {
                return Err(Diagnostic::unsupported(
                    "Relevant vertical foliage legs are outside this profile",
                ));
            }
            continue;
        }
        // Non-diffraction bends must unfold into the same elevation chord.
        let expected =
            route[0][2] + (route[route.len() - 1][2] - route[0][2]) * offset / horizontal;
        if (leg[0][2] - expected).abs() > 1e-6 {
            return Err(Diagnostic::unsupported(
                "Foliage reflection section cannot be unfolded into a vertical-wall ray",
            ));
        }
        if !selected[index].is_empty() {
            let profile = terrain.profile(leg[0], leg[1])?;
            let mut ts: Vec<f64> = profile.iter().map(|(t, _)| *t).collect();
            for &i in &selected[index] {
                polygon_splits(leg[0], leg[1], &regions[i].polygon, &mut ts);
            }
            ts.sort_by(f64::total_cmp);
            ts.dedup();
            for interval in ts.windows(2) {
                if interval[1] <= interval[0] {
                    continue;
                }
                let mid = lerp(leg[0], leg[1], (interval[0] + interval[1]) / 2.0);
                let inside: Vec<usize> = selected[index]
                    .iter()
                    .copied()
                    .filter(|&i| point_in_polygon_xy(mid, &regions[i].polygon))
                    .collect();
                if inside.is_empty() {
                    continue;
                }
                // All regions share the terrain base; overlapping canopy heights
                // form one interval up to the greatest canopy at this XY position.
                let canopy = inside
                    .iter()
                    .map(|&i| regions[i].height)
                    .fold(0.0, f64::max);
                let p = lerp(leg[0], leg[1], interval[0]);
                let q = lerp(leg[0], leg[1], interval[1]);
                let ga = terrain.elevation(p[0], p[1])?;
                let gb = terrain.elevation(q[0], q[1])?;
                let a = offset + interval[0] * length;
                let b = offset + interval[1] * length;
                // Distinct fractions can collapse after adding a preceding
                // reflected leg. Preserve their positive span in local arc
                // coordinates instead of discarding the thin interval.
                let (arc, a, b) = if a < b {
                    (arc, a, b)
                } else {
                    let width = (interval[1] - interval[0]) * length;
                    if !width.is_finite() || width <= 0.0 {
                        return Err(Diagnostic::numerical(
                            "Foliage interval lost horizontal precision",
                        ));
                    }
                    (arc.rebased(a), 0.0, width)
                };
                let mut roots = vec![a, b];
                arc.boundary_roots(a, b, ga, gb, &mut roots)?;
                arc.boundary_roots(a, b, ga + canopy, gb + canopy, &mut roots)?;
                roots.sort_by(f64::total_cmp);
                roots.dedup();
                for part in roots.windows(2) {
                    let x = part[0] + (part[1] - part[0]) / 2.0;
                    let ground = ga + (gb - ga) * (x - a) / (b - a);
                    let height = arc.height(x);
                    if height >= ground && height <= ground + canopy {
                        let traversed = arc.length(part[0], part[1]);
                        if !traversed.is_finite() || traversed < 0.0 {
                            return Err(Diagnostic::numerical("Foliage arc length is nonfinite"));
                        }
                        sum.add(traversed);
                    }
                }
            }
        }
        offset += length;
    }
    Ok(sum.total())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicBool;

    fn terrain() -> Terrain {
        Terrain::prepare(
            &json!({"ground":{"elevation_m":0.0}}),
            &AtomicBool::new(false),
        )
        .unwrap()
    }
    fn rectangle(x0: f64, x1: f64, y0: f64, y1: f64, height: f64) -> Foliage {
        Foliage {
            id: "trees".into(),
            polygon: vec![[x0, y0, 0.0], [x1, y0, 0.0], [x1, y1, 0.0], [x0, y1, 0.0]],
            height,
        }
    }
    fn near(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1e-7 * expected.abs().max(1.0),
            "actual {actual}, expected {expected}"
        );
    }

    #[test]
    fn published_foliage_table_and_explicit_boundaries() {
        assert_eq!(attenuation_for_length(9.999).unwrap(), [0.0; 8]);
        assert_eq!(
            attenuation_for_length(10.0).unwrap(),
            [0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 2.0, 3.0]
        );
        assert_eq!(
            attenuation_for_length(20.0).unwrap(),
            [0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 2.0, 3.0]
        );
        assert_eq!(
            attenuation_for_length(100.0).unwrap(),
            [2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 9.0, 12.0]
        );
        assert_eq!(
            attenuation_for_length(500.0).unwrap(),
            [4.0, 6.0, 8.0, 10.0, 12.0, 16.0, 18.0, 24.0]
        );
    }

    #[test]
    fn whole_arc_and_overlapping_canopies_conserve_length() {
        let forest = rectangle(-1.0, 101.0, -1.0, 1.0, 100.0);
        let route = [[0.0, 0.0, 1.0], [100.0, 0.0, 1.0]];
        let expected = 2.0 * RADIUS * (100.0 / (2.0 * RADIUS)).asin();
        let actual = traversal_length(&terrain(), &[forest.clone(), forest], &route, &[]).unwrap();
        near(actual, expected);
        assert!(actual > 100.0);
    }

    #[test]
    fn canopy_exit_and_reentry_are_found_when_endpoints_agree() {
        let forest = rectangle(-1.0, 1001.0, -1.0, 1.0, 10.0);
        let route = [[0.0, 0.0, 1.0], [1000.0, 0.0, 1.0]];
        // Circle centre is (500,1-sqrt(R²-500²)). Canopy intersection
        // x-offset from centre is sqrt(R²-(sqrt(R²-500²)+9)²).
        let base = (RADIUS * RADIUS - 500.0_f64.powi(2)).sqrt();
        let offset = (RADIUS * RADIUS - (base + 9.0).powi(2)).sqrt();
        let expected = 2.0 * RADIUS * ((500.0 / RADIUS).asin() - (offset / RADIUS).asin());
        near(
            traversal_length(&terrain(), &[forest], &route, &[]).unwrap(),
            expected,
        );
    }

    #[test]
    fn reflected_arc_stays_whole_and_diffraction_splits_it() {
        let forest = rectangle(-1.0, 501.0, -1.0, 501.0, 10.0);
        let reflected = [[0.0, 0.0, 1.0], [500.0, 0.0, 1.0], [500.0, 500.0, 1.0]];
        let direct = [[0.0, 0.0, 1.0], [1000.0, 0.0, 1.0]];
        let broad = rectangle(-1.0, 1001.0, -1.0, 1.0, 10.0);
        let actual = traversal_length(&terrain(), &[forest.clone()], &reflected, &[]).unwrap();
        near(
            actual,
            traversal_length(&terrain(), &[broad], &direct, &[]).unwrap(),
        );
        let split = traversal_length(&terrain(), &[forest], &reflected, &[1]).unwrap();
        near(split, 4.0 * RADIUS * (500.0 / (2.0 * RADIUS)).asin());
        assert!(split > actual);
    }

    #[test]
    fn partial_footprints_and_sloped_terrain_have_actual_arc_lengths() {
        let forest = rectangle(20.0, 80.0, -1.0, 1.0, 100.0);
        let route = [[0.0, 0.0, 1.0], [100.0, 0.0, 1.0]];
        let expected = 2.0 * RADIUS * (30.0 / RADIUS).asin();
        near(
            traversal_length(&terrain(), &[forest], &route, &[]).unwrap(),
            expected,
        );
        let t = Terrain::prepare(
            &json!({"terrain":{"samples":[[0.0,0.0,0.0],[100.0,0.0,10.0]]}}),
            &AtomicBool::new(false),
        )
        .unwrap();
        let route = [[0.0, 0.0, 1.0], [100.0, 0.0, 11.0]];
        let forest = rectangle(-1.0, 101.0, -1.0, 1.0, 100.0);
        near(
            traversal_length(&t, &[forest], &route, &[]).unwrap(),
            2.0 * RADIUS * (100.0_f64.hypot(10.0) / (2.0 * RADIUS)).asin(),
        );
    }

    #[test]
    fn reflected_endpoint_sliver_retains_positive_traversal() {
        // Each 512 m leg crosses a strip of exact width 2^-44 m. On the
        // return leg, adding its offset rounds both unfolded endpoints to
        // 1024 m, although the interval itself has positive physical length.
        let width = 2.0_f64.powi(-44);
        let forest = rectangle(0.0, width, -1.0, 1.0, 100.0);
        let route = [[0.0, 0.0, 1.0], [512.0, 0.0, 1.0], [0.0, 0.0, 1.0]];
        assert_eq!(512.0 + (1.0 - 2.0_f64.powi(-53)) * 512.0, 1024.0);
        let actual = traversal_length(&terrain(), &[forest], &route, &[]).unwrap();
        // Analytic circle derivative ds/dx=1/sqrt(1-(x-cx)^2/R^2).
        // Curvature across this strip is far below the relative tolerance.
        let expected = 2.0 * width / (1.0 - (512.0 / RADIUS).powi(2)).sqrt();
        assert!(actual > 0.0);
        assert!(
            (actual / expected - 1.0).abs() < 1e-12,
            "{actual} != {expected}"
        );
    }

    #[test]
    fn unsupported_arcs_fail_only_when_relevant() {
        let forest = rectangle(-1.0, 1.0, -1.0, 1.0, 10.0);
        assert!(
            traversal_length(
                &terrain(),
                &[forest.clone()],
                &[[0.0, 0.0, 1.0], [0.0, 0.0, 2.0]],
                &[]
            )
            .is_err()
        );
        assert!(
            traversal_length(
                &terrain(),
                &[forest.clone()],
                &[[0.0, 0.0, 1.0], [10000.0, 0.0, 1.0]],
                &[]
            )
            .is_err()
        );
        assert!(Arc::new(1.0, 0.0, 9990.0).is_err());
        assert_eq!(
            traversal_length(
                &terrain(),
                &[forest],
                &[[20.0, 0.0, 1.0], [20.0, 0.0, 2.0]],
                &[]
            )
            .unwrap(),
            0.0
        );
    }
}
