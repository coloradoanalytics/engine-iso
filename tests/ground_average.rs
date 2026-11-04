//! General-method ground factors averaged along the path at map coordinates.
//! The length-weighted mean of a constant G over several ground regions can
//! round just outside the [0,1] range the method validates. That rounding must
//! not fail the calculation, and a G that really is out of range still must.
//! Engineering robustness checks with synthetic geometry, not ISO cases.
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;

const EAST: f64 = 495_000.3;
const NORTH: f64 = 4_486_000.7;
const GROUND: f64 = 1515.7;

/// One ground region covering everything from `edge` metres east of the
/// source; the same G on both sides unless stated. The boundary falls inside
/// the middle region, so the mean is (edge - start) + (end - edge) over
/// (end - start), and those lengths round differently. The path is diagonal
/// because lengths along an axis at map coordinates are all multiples of the
/// coordinate spacing, whose differences do not round.
fn region(edge: f64, factor: f64) -> Vec<Value> {
    let w = EAST + edge;
    let e = EAST + 5000.;
    vec![json!({"id":"g","factor":factor,
        "boundary":[[w,NORTH-3000.],[e,NORTH-3000.],[e,NORTH+3000.],[w,NORTH+3000.]]})]
}

fn run(default_g: f64, regions: Vec<Value>, length: f64) -> Result<Value, String> {
    let request = json!({
        "sources":[{"id":"s","kind":"point","position":[EAST, NORTH, GROUND + 3.],
            "spectrum":vec![90.0_f64; 8]}],
        "receivers":[{"id":"r","position":[EAST + 0.6 * length, NORTH + 0.8 * length, GROUND + 2.]}],
        "ground":{"elevation_m":GROUND,"factor":default_g,"regions":regions},
        "settings":{"max_reflect_order":0}
    });
    iso_rust::calculate(&request, &AtomicBool::new(false), &|_, _| {})
        .map_err(|e| format!("{}: {}", e.code, e.message))
}

/// G = 1 on both sides of a boundary, over many path lengths and boundary
/// positions: some of these means round above 1.
#[test]
fn constant_full_ground_across_a_boundary_is_not_rejected() {
    let mut failures = vec![];
    for i in 0..512 {
        for j in 0..8 {
            let length = 800. + i as f64 * 0.37;
            let edge = 100. + j as f64 * 11.3;
            if let Err(e) = run(1., region(edge, 1.), length) {
                failures.push(e);
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of 4096: {}",
        failures.len(),
        failures[0]
    );
}

/// Crossing a boundary between equal G changes nothing; different G still
/// changes the result.
#[test]
fn a_boundary_between_equal_factors_has_no_effect() {
    let level = |r: Value| r["receivers"][0]["a_weighted_db"].as_f64().unwrap();
    for i in 0..64 {
        let length = 800. + i as f64 * 0.37;
        for g in [0., 0.25, 1.] {
            let plain = level(run(g, vec![], length).unwrap());
            let split = level(run(g, region(150., g), length).unwrap());
            assert!((plain - split).abs() < 1e-9, "G {g}: {plain} {split}");
        }
        let hard = level(run(0., vec![], length).unwrap());
        let mixed = level(run(0., region(150., 1.), length).unwrap());
        assert!((hard - mixed).abs() > 0.1, "{hard} {mixed}");
    }
}

/// Request validation still rejects a G that really is out of range, whether
/// or not a mean over the path would land inside [0,1].
#[test]
fn out_of_range_factors_are_still_rejected() {
    let rejected = |r: Result<Value, String>| {
        let e = r.expect_err("out-of-range G must fail");
        assert!(e.contains("outside the supported range"), "{e}");
    };
    rejected(run(1.5, vec![], 800.));
    rejected(run(-0.5, vec![], 800.));
    rejected(run(0.5, region(150., 1.5), 800.));
    rejected(run(0., region(150., -0.5), 800.));
    rejected(run(1. + 1e-9, vec![], 800.));
}
