//! Generated degenerate geometry: identical flat-roof modules with collinear
//! facades, roof sources at exactly roof height and receivers at, below and
//! above that height. Engineering robustness
//! checks, not ISO acceptance cases: every scene must resolve, or complete
//! with its missed-route bound reported; none may fail its contact search.
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;

const ORIGIN: [f64; 3] = [600_000., 300_000., 211.836];
const HEIGHT: f64 = 4.2672; // Roof at 216.1032 m.

fn at(x: f64, y: f64, z: f64) -> [f64; 3] {
    [ORIGIN[0] + x, ORIGIN[1] + y, ORIGIN[2] + z]
}

/// Three rows of four 10 m modules with 2 m gaps: facades in each row and
/// column are coplanar, and every roof edge is at the same elevation.
fn buildings() -> Vec<Value> {
    let mut out = vec![];
    for row in 0..3 {
        for column in 0..4 {
            let (x, y) = (column as f64 * 12., row as f64 * 12.);
            out.push(
                json!({"id":format!("m{row}{column}"),"height_m":HEIGHT,"footprint":[
                at(x, y, 0.), at(x + 10., y, 0.), at(x + 10., y + 10., 0.), at(x, y + 10., 0.)]}),
            );
        }
    }
    out
}

fn sources() -> Vec<(&'static str, Value)> {
    let roof = |x: f64, y: f64| {
        json!({"id":"s","kind":"point","host_building_id":"m11","host_face_id":"roof",
            "attachment":{"placement_profile":"acalc_schema11","frame":"building_local",
                "x_m":x,"y_m":y,"mounting":"surface"},"spectrum":vec![90.0_f64; 8]})
    };
    let free = |z: f64| json!({"id":"s","kind":"point","position":at(17., 17., HEIGHT + z),"spectrum":vec![90.0_f64; 8]});
    vec![
        ("surface roof centre", roof(5., 5.)),
        ("surface roof near corner", roof(9.5, 9.5)),
        ("free 0.05 m above roof", free(0.05)),
        ("free 1 m above roof", free(1.)),
    ]
}

fn receivers() -> Vec<Value> {
    let mut out = vec![];
    for (i, angle) in (0..8)
        .map(|k| k as f64 * std::f64::consts::FRAC_PI_4 + 0.3)
        .enumerate()
    {
        for (j, z) in [1.5, HEIGHT, 6.].into_iter().enumerate() {
            let (x, y) = (17. + 60. * angle.cos(), 17. + 60. * angle.sin());
            out.push(json!({"id":format!("r{i}_{j}"),"position":at(x, y, z)}));
        }
    }
    out
}

fn run(source: &Value, order: u64) -> Result<Value, String> {
    let request = json!({
        "sources":[source],
        "receivers":receivers(),
        "buildings":buildings(),
        "ground":{"elevation_m":ORIGIN[2]},
        "settings":{"max_reflect_order":order,"minimum_contribution_db":null}
    });
    iso_rust::calculate(&request, &AtomicBool::new(false), &|_, _| {})
        .map_err(|e| format!("{}: {}", e.code, e.message))
}

fn bounded_receivers(result: &Value) -> usize {
    result["diagnostics"]["integration"]
        .as_array()
        .map_or(0, |r| {
            r.iter()
                .filter(|r| r.get("unresolved_contact_allowance").is_some())
                .count()
        })
}

#[test]
fn flat_roof_modules_resolve_or_bound_every_contact_search() {
    let mut failures = vec![];
    for (name, source) in sources() {
        match run(&source, 2) {
            Ok(result) => eprintln!(
                "{name}: ok, {} bounded receivers",
                bounded_receivers(&result)
            ),
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
#[ignore = "order-3 sweep; run in release mode"]
fn flat_roof_modules_resolve_or_bound_every_contact_search_at_order_three() {
    let mut failures = vec![];
    for (name, source) in sources() {
        match run(&source, 3) {
            Ok(result) => eprintln!(
                "{name}: ok, {} bounded receivers",
                bounded_receivers(&result)
            ),
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
#[ignore = "known: path::ray_blocked reports a leg grazing a flat roof at roof height as blocked, so a surface roof source is silent below roof height"]
fn surface_roof_source_is_heard_below_roof_height() {
    // A source 0.05 m above the same roof is 43-49 dBA at every receiver here.
    // At roof height the route over the roof edge starts with a leg in the
    // roof plane, which the boundary-inclusive building test rejects.
    let (_, source) = sources().remove(0);
    let result = run(&source, 0).unwrap();
    for receiver in result["receivers"].as_array().unwrap() {
        assert!(
            receiver["a_weighted_db"].is_number(),
            "{} is silent",
            receiver["receiver_id"]
        );
    }
}
