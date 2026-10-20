//! Application transfer-contract and linearity regressions. These constructed
//! cases do not provide independent ISO scene validation or conformity evidence.
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;

fn request() -> Value {
    json!({
        "sources":[{"id":"s","kind":"point","position":[0,0,2],"spectrum":[100,100,100,100,100,100,100,100]}],
        "receivers":[{"id":"r","position":[100,0,2]}],
        "settings":{"minimum_contribution_db":null,"area_absolute_tolerance":0},
        "output":{"source_power_transfers":true}
    })
}
fn calculate(input: &Value) -> Value {
    iso_rust::calculate(input, &AtomicBool::new(false), &|_, _| {}).unwrap()
}
fn reject(input: &Value, field: &str) {
    let error = iso_rust::validation::validate(input).unwrap_err();
    assert_eq!(error.field.as_deref(), Some(field), "{error:?}");
}

#[test]
fn source_transfer_contract_rejects_unsafe_reuse() {
    assert!(
        iso_rust::capabilities()["features"]
            .as_array()
            .unwrap()
            .contains(&json!("source_receiver_transfers"))
    );
    let input = request();
    iso_rust::validation::validate(&input).unwrap();
    for cutoff in [json!(0), json!(-100)] {
        let mut value = input.clone();
        value["settings"]["minimum_contribution_db"] = cutoff;
        reject(&value, "settings.minimum_contribution_db");
    }
    let mut value = input.clone();
    value["settings"]
        .as_object_mut()
        .unwrap()
        .remove("minimum_contribution_db");
    reject(&value, "settings.minimum_contribution_db");
    let mut value = input.clone();
    value["settings"]["pruning_strategy"] = json!("dba");
    reject(&value, "settings.pruning_strategy");
    for tolerance in [Some(json!(1e-12)), None] {
        let mut value = input.clone();
        value["settings"]
            .as_object_mut()
            .unwrap()
            .remove("area_absolute_tolerance");
        if let Some(tolerance) = tolerance {
            value["settings"]["area_absolute_tolerance"] = tolerance;
        }
        reject(&value, "settings.area_absolute_tolerance");
    }
    let mut value = input.clone();
    value["settings"]["ground_method"] = json!("mean_path_height");
    reject(&value, "settings.ground_method");
    let mut value = input.clone();
    value["area_receivers"] =
        json!([{"id":"a","boundary":[[0,0],[2,0],[0,2]],"spacing_m":1,"height_m":2}]);
    reject(&value, "area_receivers");
    for level in [json!(99), Value::Null] {
        let mut value = input.clone();
        value["sources"][0]["spectrum"][0] = level;
        reject(&value, "sources[0].spectrum");
    }
    let mut value = input.clone();
    value["sources"][0] = json!({"id":"s","kind":"line","zones":[
        {"id":"z","vertices":[[0,0,2],[1,0,2]],"spectrum":[100,100,100,100,100,100,100,100]}]});
    reject(&value, "sources[0].zones");
}

#[test]
fn homogeneous_source_transfers_keep_power_basis_and_long_term_scaling() {
    let sources = vec![
        json!({"id":"s","kind":"point","position":[0,0,2],"power_basis":"total_power"}),
        json!({"id":"s","kind":"line","vertices":[[0,0,2],[8,0,2]],"power_basis":"total_power"}),
        json!({"id":"s","kind":"line","vertices":[[0,0,2],[8,0,2]],"power_basis":"power_per_m"}),
        json!({"id":"s","kind":"area","triangles":[[[0,0,2],[8,0,2],[0,4,2]]],"power_basis":"total_power"}),
        json!({"id":"s","kind":"area","triangles":[[[0,0,2],[8,0,2],[0,4,2]]],"power_basis":"power_per_m2"}),
    ];
    for mut source in sources {
        source["spectrum"] = json!([100, 100, 100, 100, 100, 100, 100, 100]);
        let mut input = request();
        input["sources"] = json!([source]);
        input["settings"]["meteorological_mode"] = json!("long_term");
        input["settings"]["c0"] = json!(2);
        let transfer = calculate(&input);
        assert_eq!(
            transfer["source_power_transfers"],
            json!({
            "reference_db":100.,"power_basis":"per_source_input","pruning":"disabled"})
        );
        assert_eq!(transfer["warning_summary"]["warnings"], 0);
        for b in 0..8 {
            assert_eq!(transfer["receivers"][0]["omitted_energy"][b], 0.);
        }
        input["output"] = json!({});
        input["sources"][0]["spectrum"] = json!([70, 75, 80, 85, 90, 95, 105, 110]);
        let forward = calculate(&input);
        assert!(forward.get("source_power_transfers").is_none());
        for b in 0..8 {
            let reference = transfer["receivers"][0]["sources"][0]["octave_db"][b]
                .as_f64()
                .unwrap();
            let power = input["sources"][0]["spectrum"][b].as_f64().unwrap();
            let predicted = reference - 100. + power;
            let actual = forward["receivers"][0]["octave_db"][b].as_f64().unwrap();
            assert!(
                (predicted - actual).abs() < 1e-9,
                "{source}: band {b}: {predicted} vs {actual}"
            );
        }
    }
}

#[test]
fn attached_source_transfer_preserves_host_scene() {
    for kind in ["point", "area"] {
        let mut input = request();
        input["buildings"] =
            json!([{"id":"b","footprint":[[0,0,0],[10,0,0],[10,10,0],[0,10,0]],"height_m":6}]);
        input["sources"][0] = json!({"id":"s","kind":kind,"host_building_id":"b","host_face_id":"wall:0:1",
            "spectrum":[100,100,100,100,100,100,100,100],"power_basis":"total_power",
            "attachment":{"placement_profile":"acalc_schema11","x_m":2,"y_m":2}});
        if kind == "point" {
            input["sources"][0]["attachment"]["mounting"] = json!("surface");
        } else {
            input["sources"][0]["attachment"]["width_m"] = json!(2);
            input["sources"][0]["attachment"]["height_m"] = json!(2);
        }
        input["receivers"] =
            json!([{"id":"front","position":[5,-30,2]},{"id":"back","position":[5,30,2]}]);
        let result = calculate(&input);
        assert_eq!(result["receivers"][0]["state"], "calculated");
        assert_eq!(result["warning_summary"]["warnings"], 0);
        input["output"] = json!({});
        input["sources"][0]["spectrum"] = json!([70, 70, 70, 70, 70, 70, 70, 70]);
        let forward = calculate(&input);
        for receiver in 0..2 {
            for band in 0..8 {
                let reference = result["receivers"][receiver]["sources"][0]["octave_db"][band]
                    .as_f64()
                    .unwrap();
                let actual = forward["receivers"][receiver]["sources"][0]["octave_db"][band]
                    .as_f64()
                    .unwrap();
                assert!((reference - 30. - actual).abs() < 1e-8);
            }
        }
    }
}
