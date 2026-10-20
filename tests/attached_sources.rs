//! Supplementary analytic checks of the building-attached point mounting
//! convention (ALGORITHM.md, building-attached sources). Constructed scenes,
//! not published ISO acceptance cases.
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;

const ABSORPTION: f64 = 0.2;

/// 100 m wall facing -Y, source 2 m high at its centre, receiver 30 m out.
fn run(attachment: Value) -> Value {
    let request = json!({
        "sources":[{"id":"s","kind":"point","host_building_id":"b","host_face_id":"wall:0:1",
            "attachment":attachment,"spectrum":[null,null,null,null,100,null,null,null]}],
        "receivers":[{"id":"r","position":[0.,-30.,2.]}],
        "buildings":[{"id":"b","footprint":[[-50,0,0],[50,0,0],[50,20,0],[-50,20,0]],"height_m":10,
            "absorption":[ABSORPTION,ABSORPTION,ABSORPTION,ABSORPTION,ABSORPTION,ABSORPTION,ABSORPTION,ABSORPTION]}],
        "settings":{"max_reflect_order":1,"minimum_contribution_db":null},
        "output":{"path_diagnostics":"full"}
    });
    iso_rust::calculate(&request, &AtomicBool::new(false), &|_, _| {}).unwrap()
}

fn route_energy(result: &Value, reflected: bool) -> Option<f64> {
    result["path_diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p.get("physical_route").is_some())
        .filter(|p| p["reflection_faces"].as_array().unwrap().is_empty() != reflected)
        .map(|p| p["unit_route_energy"].as_f64().unwrap())
        .reduce(f64::max)
}

#[test]
fn surface_point_has_no_host_reflection() {
    let result =
        run(json!({"placement_profile":"acalc_schema11","x_m":50,"y_m":8,"mounting":"surface"}));
    assert!(route_energy(&result, false).unwrap() > 0.);
    assert_eq!(route_energy(&result, true).unwrap_or(0.), 0.);
}

#[test]
fn standoff_point_is_reflected_by_its_host() {
    let d: f64 = 0.05;
    let result = run(
        json!({"placement_profile":"acalc_schema11","x_m":50,"y_m":8,"mounting":"standoff","distance_m":d}),
    );
    let direct = route_energy(&result, false).unwrap();
    let reflected = route_energy(&result, true).unwrap();
    // Normal incidence: image at 2d behind the source, so the reflected/direct
    // ratio is (1 - alpha) (r_direct / r_image)^2. Ground and atmospheric
    // terms differ only through the 0.1 m longer path.
    let expected = (1. - ABSORPTION) * ((30. - d) / (30. + d)).powi(2);
    let error_db = 10. * (reflected / direct / expected).log10();
    assert!(
        error_db.abs() < 0.02,
        "reflected/direct differs by {error_db} dB"
    );
}
