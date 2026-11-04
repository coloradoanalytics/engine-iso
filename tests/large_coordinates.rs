//! Building geometry checks at projected map coordinates. Footprints authored
//! in UTM or Web Mercator have coordinates of 10^5 to 10^7 m, where one unit
//! in the last place is 10^-11 to 10^-9 m. The same footprint must prepare
//! the same way wherever it lies; a genuinely crossing roof must still fail.
//! Engineering robustness checks with synthetic footprints, not ISO cases.
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;

/// Local origin, a UTM zone 13N position and a Web Mercator position.
const OFFSETS: [[f64; 2]; 3] = [
    [0., 0.],
    [495_944.3, 4_486_221.9],
    [-11_694_671.721_295_39, 4_942_538.661_818_305],
];
const GROUND: f64 = 1515.7;

fn run(buildings: Vec<Value>, at: [f64; 2]) -> Result<Value, String> {
    let request = json!({
        "sources":[{"id":"s","kind":"point","position":[at[0] + 90_000., at[1], GROUND + 1.],
            "spectrum":vec![90.0_f64; 8]}],
        "receivers":[{"id":"r","position":[at[0] - 90_000., at[1], GROUND + 1.5]}],
        "buildings":buildings,
        "ground":{"elevation_m":GROUND},
        "settings":{"max_reflect_order":0}
    });
    iso_rust::calculate(&request, &AtomicBool::new(false), &|_, _| {})
        .map_err(|e| format!("{}: {}", e.code, e.message))
}

/// Coordinates rounded to 0.1 m as exported map footprints often are.
fn place(local: &[[f64; 2]], at: [f64; 2]) -> Vec<[f64; 3]> {
    local
        .iter()
        .map(|p| {
            [
                ((at[0] + p[0]) * 10.).round() / 10.,
                ((at[1] + p[1]) * 10.).round() / 10.,
                GROUND,
            ]
        })
        .collect()
}

fn building(id: &str, footprint: Vec<[f64; 3]>, triangles: Option<&[[usize; 3]]>) -> Value {
    let mut b = json!({"id":id,"footprint":footprint,"height_m":6.0});
    if let Some(t) = triangles {
        b["roof_triangles"] = json!(t);
    }
    b
}

/// Deterministic concave star footprints with uneven arm lengths: ear
/// clipping produces long thin roof triangles across the notches.
fn stars(size: f64) -> Vec<Vec<[f64; 2]>> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    };
    (0..40)
        .map(|_| {
            let n = 10 + (next() * 20.) as usize;
            (0..2 * n)
                .map(|i| {
                    let angle = i as f64 * std::f64::consts::PI / n as f64;
                    let radius = if i % 2 == 0 {
                        size * (20. + 40. * next())
                    } else {
                        size * (4. + 12. * next())
                    };
                    [
                        (radius * angle.cos() * 10.).round() / 10.,
                        (radius * angle.sin() * 10.).round() / 10.,
                    ]
                })
                .collect()
        })
        .collect()
}

/// A valid roof mesh that differs from the engine's own triangulation:
/// ear clipping started from another vertex, as an authoring tool may do.
/// `None` when it would contain a zero-area triangle on the 0.1 m grid.
fn rotated_triangulation(local: &[[f64; 2]], start: usize) -> Option<Vec<[usize; 3]>> {
    let n = local.len();
    let rotated: Vec<[f64; 3]> = (0..n)
        .map(|i| {
            let p = local[(i + start) % n];
            [p[0], p[1], 0.]
        })
        .collect();
    let mesh: Vec<[usize; 3]> = iso_rust::geometry::triangulate_polygon(&rotated)
        .unwrap()
        .into_iter()
        .map(|t| t.map(|i| (i + start) % n))
        .collect();
    let tenths = |i: usize| local[i].map(|x| (x * 10.).round() as i64);
    let collinear = |t: &[usize; 3]| {
        let [a, b, c] = t.map(tenths);
        (b[0] - a[0]) * (c[1] - a[1]) == (b[1] - a[1]) * (c[0] - a[0])
    };
    (!mesh.iter().any(collinear)).then_some(mesh)
}

#[test]
fn concave_footprints_prepare_at_map_coordinates() {
    for at in OFFSETS {
        for (i, star) in [1., 40., 1e3].into_iter().flat_map(stars).enumerate() {
            for start in [0, 1, star.len() / 2] {
                let Some(mesh) = rotated_triangulation(&star, start) else {
                    continue;
                };
                let mesh = (start > 0).then_some(mesh.as_slice());
                let result = run(vec![building("b", place(&star, at), mesh)], at);
                assert!(
                    result.is_ok(),
                    "star {i} start {start} at {at:?}: {}",
                    result.unwrap_err()
                );
            }
        }
    }
}

/// A rectilinear plan with wings and a 0.1 m step: the authored roof mesh
/// has thin triangles along the step.
const WINGS: [[f64; 2]; 12] = [
    [0., 0.],
    [8.9, -0.2],
    [9.0, 2.3],
    [18.9, 2.1],
    [18.9, 0.],
    [28.4, -0.1],
    [28.1, -17.4],
    [72.3, -18.0],
    [72.1, -67.3],
    [-15.8, -65.8],
    [-15.0, -17.0],
    [-0.3, -17.3],
];

#[test]
fn authored_roof_mesh_prepares_at_map_coordinates() {
    // As the QGIS plugin's ear clipping emits it; [1, 5, 4] is 1.45 m².
    let triangles = [
        [1, 0, 11],
        [11, 10, 9],
        [1, 11, 9],
        [1, 9, 8],
        [8, 7, 6],
        [1, 8, 6],
        [1, 6, 5],
        [1, 5, 4],
        [1, 4, 3],
        [3, 2, 1],
    ];
    for at in OFFSETS {
        let result = run(vec![building("b", place(&WINGS, at), Some(&triangles))], at);
        assert!(result.is_ok(), "at {at:?}: {}", result.unwrap_err());
    }
}

#[test]
fn roof_triangle_filling_a_dent_is_rejected_at_map_coordinates() {
    // A square with a V dent in its top edge at vertex 3. The mesh does not
    // overlap itself and uses every vertex, but [2, 4, 3] fills the dent,
    // which lies outside the footprint.
    let dented = [[0., 0.], [20., 0.], [20., 20.], [10., 12.], [0., 20.]];
    let crossing = [[0, 1, 3], [1, 2, 3], [3, 4, 0], [2, 4, 3]];
    for at in OFFSETS {
        let result = run(vec![building("b", place(&dented, at), Some(&crossing))], at);
        let error = result.expect_err("a crossing roof mesh must be rejected");
        assert!(
            error.ends_with("Roof triangle crosses a concave footprint boundary"),
            "at {at:?}: {error}"
        );
    }
}

#[test]
fn buildings_sharing_a_wall_prepare_at_map_coordinates() {
    let west = [[0., 0.], [12.3, 0.1], [12.4, 9.7], [0.1, 9.6]];
    let east = [[12.3, 0.1], [25.1, 0.2], [25.0, 9.9], [12.4, 9.7]];
    for at in OFFSETS {
        let result = run(
            vec![
                building("west", place(&west, at), None),
                building("east", place(&east, at), None),
            ],
            at,
        );
        assert!(result.is_ok(), "at {at:?}: {}", result.unwrap_err());
    }
}

/// A triangle of size `l` and a second one whose corner intrudes `depth` past
/// the first's long edge: the overlap is a compact (2/3)·depth² triangle.
fn intruding_corner(l: f64, depth: f64) -> ([[f64; 3]; 3], [[f64; 3]; 3]) {
    let a = [[0., 0., 0.], [l, 0., 0.], [0., l, 0.]];
    let (x, y) = (l / 2. - depth, l / 2. - depth);
    let b = [
        [x, y, 0.],
        [x + l / 2., y + l / 4., 0.],
        [x + l / 4., y + l / 2., 0.],
    ];
    (a, b)
}

/// The overlap tolerance bounds the *width* of what it can hide (64 ε L, a
/// few dozen ulps of the span), so compact overlaps wider than that are found
/// at every span the request contract allows, up to the 1e-12 m² floor. An area
/// floor of 64 ε L² hid each of these.
#[test]
fn compact_overlaps_are_found_at_every_span() {
    let overlaps =
        |a: &[[f64; 3]], b: &[[f64; 3]]| iso_rust::scene::polygons_overlap(a, b).unwrap();
    for (l, depth) in [(1e5, 0.01), (1e6, 0.1), (1e9, 100.), (1e9, 0.01)] {
        let (a, b) = intruding_corner(l, depth);
        assert!(overlaps(&a, &b), "span {l} depth {depth}");
    }
}

#[test]
fn touching_triangles_do_not_overlap_at_any_span() {
    for l in [1e3, 1e6, 1e9] {
        // Shared long edge, irrational-ish vertices so clip points round.
        let p = [l * 0.137_035_999, l * 0.271_828_182, 0.];
        let q = [l * 0.914_159_265, l * 0.577_215_123, 0.];
        let side_a = [l * 0.161_803_398, l * 0.933_012_701, 0.];
        let side_b = [l * 0.702_106_123, l * 0.012_345_678, 0.];
        assert!(
            !iso_rust::scene::polygons_overlap(&[p, q, side_a], &[q, p, side_b]).unwrap(),
            "span {l}"
        );
    }
}
