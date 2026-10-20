//! End-to-end protocol/solver checks and supplementary analytical evidence.
//! These constructed scenes are not ISO/TR reference cases or conformity passes.
mod common;
use common::*;
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;

fn point_request(spectrum: Value) -> Value {
    json!({
        "sources":[{"id":"source-α","kind":"point","position":[0,0,1.5],"spectrum":spectrum}],
        "receivers":[{"id":"receiver-β","position":[100,0,1.5]}],
        "settings":{"minimum_contribution_db":null}
    })
}
/// The result of a completed calculation.
fn result(engine: &Engine, input: &Value) -> Value {
    calculate(engine, input).result
}

#[test]
fn direct_hard_ground_matches_independent_analytical_and_published_terms() {
    let engine = Engine::start(2);
    let result = self::result(
        &engine,
        &point_request(json!([90, 90, 90, 90, 90, 90, 90, 90])),
    );
    let receiver = &result["receivers"][0];
    assert_eq!(receiver["receiver_id"], "receiver-β");
    assert_eq!(receiver["sources"][0]["source_id"], "source-α");
    // Constructed analytical case: h_s=h_r=1.5 m, d=d_p=100 m,
    // G=0, source=90 dB. Table 3 reduces to uncorrected A_gr=-3.3 dB.
    // 2024 equations 12-13 give the following closed-form ground gain.
    // These formula reductions do not invoke production acoustic functions.
    let k = 10_000_f64 / 10_009_f64;
    let ground_gain = 10. * (1. + k * (10_f64.powf(0.33) - 1.)).log10();
    let divergence = 51.; // 20 log10(100)+11, equation 8.
    // Independent rounded entries from ISO 9613-1:1993 Table 1(g), printed
    // page 8, 10 C/70%/101.325 kPa, dB/km.
    let published_alpha = [0.122, 0.411, 1.04, 1.93, 3.66, 9.66, 32.8, 117.0];
    for (band, alpha) in published_alpha.into_iter().enumerate() {
        let expected = 90. - divergence + ground_gain - 0.1 * alpha;
        let actual = receiver["octave_db"][band].as_f64().unwrap();
        // The 0.7% engineering allowance covers table rounding and the Annex B
        // saturation-pressure approximation; this is not an ISO/TR published
        // acceptance interval.
        let tolerance = 0.0007 * alpha + 1e-10;
        assert!(
            (actual - expected).abs() <= tolerance,
            "band {band}: {actual} vs {expected}, tolerance {tolerance}"
        );
        assert_eq!(receiver["band_states"][band], "calculated");
        assert_eq!(receiver["omitted_energy"][band], 0.);
    }
    let weights = [-26.2, -16.1, -8.6, -3.2, 0., 1.2, 1., -1.1];
    let sum: f64 = (0..8)
        .map(|b| 10_f64.powf((receiver["octave_db"][b].as_f64().unwrap() + weights[b]) / 10.))
        .sum();
    assert!((receiver["a_weighted_db"].as_f64().unwrap() - 10. * sum.log10()).abs() < 1e-10);
    assert_eq!(result["atmosphere"]["temperature_c"], 10.);
    assert_eq!(result["ground"]["factor"], 0.);
    assert!(result["settings"]["minimum_contribution_db"].is_null());
    let method = result["method"].as_str().unwrap();
    assert!(method.contains("9613-2:2024") && method.contains("9613-1:1993"));
}

#[test]
fn numeric_zero_silence_and_cutoff_remain_distinct() {
    let engine = Engine::start(2);
    let result = self::result(
        &engine,
        &point_request(json!([0, null, null, null, null, null, null, null])),
    );
    let r = &result["receivers"][0];
    assert!(r["octave_db"][0].as_f64().unwrap() < 0.);
    assert_eq!(r["band_states"][0], "calculated");
    for b in 1..8 {
        assert!(r["octave_db"][b].is_null());
        assert_eq!(r["band_states"][b], "exact_zero");
        assert_eq!(r["omission_increase_db"][b], 0.);
    }
    let silent = self::result(
        &engine,
        &point_request(json!([null, null, null, null, null, null, null, null])),
    );
    assert_eq!(silent["receivers"][0]["state"], "exact_zero");
    assert!(silent["receivers"][0]["a_weighted_db"].is_null());
    // Absolute pruning is opt-in: without settings nothing is cut.
    let mut uncut = point_request(json!([0, 0, 0, 0, 0, 0, 0, 0]));
    uncut.as_object_mut().unwrap().remove("settings");
    let result = self::result(&engine, &uncut);
    let r = &result["receivers"][0];
    assert_eq!(r["state"], "calculated");
    assert_eq!(result["settings"]["minimum_contribution_db"], Value::Null);
    for b in 0..8 {
        assert_eq!(r["band_states"][b], "calculated");
        assert_eq!(r["omitted_energy"][b], 0.);
    }
    let mut cut = uncut;
    cut["settings"] = json!({"minimum_contribution_db":0});
    let result = self::result(&engine, &cut);
    let r = &result["receivers"][0];
    assert_eq!(r["state"], "cutoff_limited");
    assert!(r["a_weighted_db"].is_null());
    for b in 0..8 {
        assert!(r["octave_db"][b].is_null());
        assert_eq!(r["band_states"][b], "cutoff_limited");
        assert!(r["omitted_energy"][b].as_f64().unwrap() > 0.);
        assert!(r["omission_increase_db"][b].is_null());
        assert!(r["omission_upper_db"][b].as_f64().unwrap().is_finite());
    }
}

fn mixed_sources() -> Value {
    json!({
        "sources":[
            {"id":"point","kind":"point","position":[0,0,2],"spectrum":[null,null,null,null,80,null,null,null]},
            {"id":"line","kind":"line","vertices":[[0,10,2],[1,10,2]],"spectrum":[null,null,null,null,83,null,null,null]},
            {"id":"area","kind":"area","triangles":[[[0,20,2],[1,20,2],[0,21,2]]],"spectrum":[null,null,null,null,86,null,null,null]}
        ],
        "receivers":[{"id":"a","position":[100,0,2]},{"id":"b","position":[150,5,3]}],
        "settings":{"minimum_contribution_db":null}
    })
}

#[test]
fn original_source_identity_weighting_and_parallel_reduction_are_stable() {
    let request = mixed_sources();
    let single = calculate(&Engine::start(1), &request);
    assert_eq!(single.final_progress()["total_task_points"], 7.);
    assert_eq!(single.final_progress()["completed_task_points"], 7.);
    let single = single.result;
    let available = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let parallel = self::result(&Engine::start(available), &request);
    assert_eq!(
        single["receivers"], parallel["receivers"],
        "Acoustic result changed with CPU worker count."
    );
    assert_eq!(
        single["operations"], parallel["operations"],
        "Local operation counts must be merged exactly once per receiver stub."
    );
    for (i, r) in single["receivers"].as_array().unwrap().iter().enumerate() {
        assert_eq!(r["receiver_id"], if i == 0 { "a" } else { "b" });
        let contributions = r["sources"].as_array().unwrap();
        assert_eq!(contributions.len(), 3);
        for (source, id) in contributions.iter().zip(["point", "line", "area"]) {
            assert_eq!(source["source_id"], id);
        }
        let energy: f64 = contributions
            .iter()
            .map(|s| 10_f64.powf(s["octave_db"][4].as_f64().unwrap() / 10.))
            .sum();
        assert!((r["octave_db"][4].as_f64().unwrap() - 10. * energy.log10()).abs() < 1e-10);
    }
}

fn library(input: &Value) -> Value {
    iso_rust::calculate(input, &AtomicBool::new(false), &|_, _| {}).unwrap()
}
/// An artifact of a calculation, which acalc-engine serves under `href`.
fn artifact(calculation: &Calculation, href: &str) -> Vec<u8> {
    let name = href
        .strip_prefix(BASE)
        .expect("artifact links use the job's base");
    let (_, media_type, bytes) = calculation
        .artifacts
        .iter()
        .find(|(n, _, _)| n == name)
        .expect("no such artifact");
    assert_eq!(media_type, "image/tiff");
    bytes.clone()
}
/// Read one pixel of the engine's uncompressed separate-plane Float32 layout.
fn pixel(tiff: &[u8], columns: usize, rows: usize, band: usize, row: usize, column: usize) -> f32 {
    let at = 8 + 4 * ((band * rows + row) * columns + column);
    f32::from_le_bytes(tiff[at..at + 4].try_into().unwrap())
}

#[test]
fn area_totals_omit_contributions_without_changing_any_totals() {
    // Output-format parity only, not an independent acoustic reference.
    for cutoff in [Value::Null, json!(1000)] {
        let mut request = mixed_sources();
        request["settings"]["minimum_contribution_db"] = cutoff;
        request["area_receivers"] = json!([{
            "id":"grid", "boundary":[[100,0],[101,0],[101,1],[100,1]],
            "spacing_m":1, "height_m":2
        }]);
        // Generated area points report totals only unless detail is requested.
        let default = library(&request);
        request["output"] = json!({"area_source_contributions":true});
        let mut detailed = library(&request);
        request["output"] = json!({"area_source_contributions":false});
        let compact = library(&request);
        assert_eq!(default, compact);
        let mut areas = 0;
        for receiver in detailed["receivers"].as_array_mut().unwrap() {
            if receiver.get("area_receiver_id").is_some() {
                assert_eq!(receiver["sources"].as_array().unwrap().len(), 3);
                receiver.as_object_mut().unwrap().remove("sources");
                areas += 1;
            }
        }
        assert_eq!(areas, 4);
        assert_eq!(
            compact["receivers"][0]["sources"].as_array().unwrap().len(),
            3
        );
        assert_eq!(compact["receivers"], detailed["receivers"]);
    }
}

#[test]
fn submissions_reject_per_sample_area_contributions_and_invalid_input() {
    let engine = Engine::start(1);
    let mut request = point_request(json!([90, 90, 90, 90, 90, 90, 90, 90]));
    request["output"] = json!({"area_source_contributions":true});
    let error = submit(&engine, &request).err().expect("rejected");
    assert_eq!(error["code"], "unsupported");
    assert_eq!(error["field"], "output.area_source_contributions");
    let mut invalid = point_request(json!([90, 90, 90, 90, 90, 90, 90, 90]));
    invalid["atmosphere"] = json!({"temperature_c":51});
    assert_eq!(
        submit(&engine, &invalid).err().unwrap()["code"],
        "invalid_input"
    );
    let call = engine.call(
        json!({"op":"calculate","artifact_base":BASE}),
        b"{\"sources\":[],\"sources\":[]}",
    );
    assert_eq!(call.next().header["error"]["code"], "invalid_json");
    let mut unadvertised = point_request(json!([90, 90, 90, 90, 90, 90, 90, 90]));
    unadvertised["profile_id"] = json!("not_a_profile");
    assert_eq!(
        submit(&engine, &unadvertised).err().unwrap()["field"],
        "profile_id"
    );
}

#[test]
fn empty_receiver_area_completes_without_fabricated_points() {
    let engine = Engine::start(2);
    let mut request = point_request(json!([90, 90, 90, 90, 90, 90, 90, 90]));
    request.as_object_mut().unwrap().remove("receivers");
    request["area_receivers"] = json!([{"id":"empty","boundary":[[10,10],[12,10],[12,12],[10,12]],"spacing_m":1,"exclude_polygons":[[[10,10],[12,10],[12,12],[10,12]]]}]);
    let calculation = calculate(&engine, &request);
    assert_eq!(calculation.final_progress()["completed_task_points"], 2.);
    let result = &calculation.result;
    assert_eq!(result["receivers"], json!([]));
    let area = &result["area_receivers"][0];
    assert_eq!(area["area_receiver_id"], "empty");
    assert_eq!(area["receiver_count"], 0);
    assert_eq!(
        (
            area["raster"]["columns"].as_u64(),
            area["raster"]["rows"].as_u64()
        ),
        (Some(3), Some(3))
    );
    let tiff = artifact(&calculation, area["raster"]["href"].as_str().unwrap());
    for band in 0..9 {
        for i in 0..9 {
            assert_eq!(pixel(&tiff, 3, 3, band, i / 3, i % 3), f32::MIN);
        }
    }
}

#[test]
fn receiver_grid_ids_and_row_order_survive_exclusions() {
    let mut request = point_request(json!([null, null, null, null, 90, null, null, null]));
    request.as_object_mut().unwrap().remove("receivers");
    request["area_receivers"] =
        json!([{"id":"grid","boundary":[[100,0],[102,0],[102,1],[100,1]],"spacing_m":1}]);
    let full = library(&request);
    request["area_receivers"][0]["exclude_polygons"] = json!([[
        [100.75, -0.25],
        [101.25, -0.25],
        [101.25, 0.25],
        [100.75, 0.25]
    ]]);
    let masked = library(&request);
    assert_eq!(
        full["area_receivers"][0]["grid"],
        json!({"origin":[100.,0.],"columns":3,"rows":2})
    );
    let full = full["receivers"].as_array().unwrap();
    let kept = masked["receivers"].as_array().unwrap();
    assert_eq!(full.len(), 6);
    assert_eq!(kept.len(), 5);
    for (sample, original_index) in kept.iter().zip([0, 2, 3, 4, 5]) {
        assert_eq!(sample["receiver_id"], full[original_index]["receiver_id"]);
        assert_eq!(sample["area_receiver_id"], "grid");
        assert_eq!(
            sample["grid_row"].as_u64().unwrap(),
            (original_index / 3) as u64
        );
        assert_eq!(
            sample["grid_column"].as_u64().unwrap(),
            (original_index % 3) as u64
        );
        let x = sample["position"][0].as_f64().unwrap();
        let y = sample["position"][1].as_f64().unwrap();
        let expected = format!("area:Z3JpZA:{:016x}:{:016x}", x.to_bits(), y.to_bits());
        assert_eq!(sample["receiver_id"], expected);
        assert_eq!(sample["position"][2].as_f64().unwrap(), 1.5);
    }
}

#[test]
fn area_samples_are_served_as_a_linked_geotiff_not_result_json() {
    let engine = Engine::start(2);
    let mut request = point_request(json!([null, null, null, null, 90, null, null, null]));
    request["area_receivers"] = json!([{"id":"grid","boundary":[[100,0],[102,0],[102,1],[100,1]],"spacing_m":1,
        "exclude_polygons":[[[100.75,-0.25],[101.25,-0.25],[101.25,0.25],[100.75,0.25]]]}]);
    let expected = library(&request);
    let calculation = calculate(&engine, &request);
    assert_eq!(
        calculation.final_progress()["model_summary"],
        json!({"source_count":1,"receiver_count":6})
    );
    let result = &calculation.result;
    // Only the explicit receiver remains in the document.
    assert_eq!(result["receivers"].as_array().unwrap().len(), 1);
    assert_eq!(result["receivers"][0]["receiver_id"], "receiver-β");
    let area = &result["area_receivers"][0];
    let raster = &area["raster"];
    let href = raster["href"].as_str().unwrap();
    assert_eq!(href, format!("{BASE}area-0.tif"));
    assert_eq!(calculation.artifacts.len(), 1);
    assert_eq!(calculation.artifacts[0].0, "area-0.tif");
    assert_eq!(raster["geotransform"], json!([99.5, 1., 0., 1.5, 0., -1.]));
    assert_eq!(area["sample_warnings"]["samples_with_warnings"], 0);
    let tiff = artifact(&calculation, href);
    assert_eq!(raster["bytes"], tiff.len());
    use sha2::Digest;
    assert_eq!(
        raster["sha256"],
        format!("{:x}", sha2::Sha256::digest(&tiff))
    );
    let mut seen = 0;
    for sample in expected["receivers"].as_array().unwrap() {
        if sample.get("area_receiver_id").is_none() {
            continue;
        }
        seen += 1;
        let (column, row) = (
            sample["grid_column"].as_u64().unwrap() as usize,
            sample["grid_row"].as_u64().unwrap() as usize,
        );
        let raster_row = 1 - row;
        assert_eq!(
            pixel(&tiff, 3, 2, 0, raster_row, column),
            sample["a_weighted_db"].as_f64().unwrap() as f32
        );
        assert_eq!(
            pixel(&tiff, 3, 2, 5, raster_row, column),
            sample["octave_db"][4].as_f64().unwrap() as f32
        );
        assert_eq!(pixel(&tiff, 3, 2, 1, raster_row, column), f32::MIN);
    }
    assert_eq!(seen, 5);
    // The excluded centre-bottom candidate is NoData in every band.
    assert_eq!(pixel(&tiff, 3, 2, 0, 1, 1), f32::MIN);
}

#[test]
fn discovery_identifies_the_rust_implementation_and_contract() {
    let engine = Engine::start(1);
    let hello = engine.request(json!({"op":"hello"}), b"").header["ok"].clone();
    assert_eq!(hello["protocol"], 1);
    assert_eq!(hello["method"], "ISO 9613-2:2024");
    assert_eq!(hello["divisible"], true);
    assert_eq!(hello["workers"], 1);
    assert_eq!(
        hello["build_revision"],
        iso_rust::capabilities::build_revision()
    );
    let capabilities = engine.request(json!({"op":"capabilities"}), b"").header["ok"].clone();
    assert_eq!(capabilities["api_version"], "1.2");
    assert_eq!(capabilities["implementation"]["name"], "iso_rust");
    assert_eq!(capabilities["verified_conformity"], false);
    let frequencies: Vec<f64> = capabilities["frequencies_hz"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert_eq!(
        frequencies,
        [63., 125., 250., 500., 1000., 2000., 4000., 8000.]
    );
    for kind in ["point", "line", "area"] {
        assert!(
            capabilities["source_kinds"]
                .as_array()
                .unwrap()
                .contains(&json!(kind))
        );
    }
    // Transport limits are acalc-engine's, which adds them.
    assert!(capabilities.get("transport").is_none());
    let unknown = engine.request(json!({"op":"upload"}), b"");
    assert_eq!(unknown.header["error"]["code"], "unknown_operation");
}

#[test]
fn cancellation_of_an_expensive_job_never_exposes_partial_results() {
    let engine = Engine::start(2);
    let mut request = point_request(json!([90, 90, 90, 90, 90, 90, 90, 90]));
    request["receivers"] = Value::Array(
        (0..10_000)
            .map(|i| json!({"id":format!("r{i}"),"position":[100.+i as f64*0.1,1.,1.5]}))
            .collect(),
    );
    let call = engine.call(
        json!({"op":"calculate","artifact_base":BASE}),
        request.to_string().as_bytes(),
    );
    let accepted = call.next();
    assert_eq!(accepted.header["event"], "accepted");
    engine.cancel(call.id);
    let calculation = follow(call, accepted.header["progress"].clone());
    // Completion may win the race. Either winner is final, and a cancelled
    // calculation never sends a result or artifacts.
    match &calculation.outcome {
        Err(error) => {
            assert_eq!(error["code"], "cancelled");
            assert!(calculation.result.is_null() && calculation.artifacts.is_empty());
        }
        Ok(_) => assert_eq!(
            calculation.result["receivers"].as_array().unwrap().len(),
            10_000
        ),
    }
    // The engine keeps serving.
    assert_eq!(
        engine.request(json!({"op":"hello"}), b"").header["ok"]["protocol"],
        1
    );
}

#[test]
fn surveyed_receiver_elevation_survives_submission_and_result_import() {
    let engine = Engine::start(2);
    let ground = 200.55258156292842_f64;
    let height = 1.6764000000000001_f64;
    let input = json!({
        "terrain":{"construction_profile":"acalc_schema11"},
        "sources":[{"id":"s","kind":"point","position":[600900,300000,203],"spectrum":[90,90,90,90,90,90,90,90]}],
        "receivers":[{"id":"surveyed-receiver","ground_ref":{"position":[600949.4591373267,300044.18026459473,ground]},"height_m":height}],
        "settings":{"minimum_contribution_db":null}
    });
    let result = self::result(&engine, &input);
    let receiver = &result["receivers"][0];
    assert_eq!(receiver["receiver_id"], "surveyed-receiver");
    let expected = [600949.4591373267f64, 300044.18026459473, ground + height];
    for (i, coordinate) in expected.into_iter().enumerate() {
        assert_eq!(
            receiver["position"][i].as_f64().unwrap().to_bits(),
            coordinate.to_bits()
        );
    }
}

#[test]
fn path_counts_cover_points_extended_sources_and_full_cutoffs() {
    let engine = Engine::start(4);
    for cutoff in [json!(null), json!(1000)] {
        let mut input = mixed_sources();
        input["settings"]["minimum_contribution_db"] = cutoff.clone();
        let calculation = calculate(&engine, &input);
        let result = &calculation.result;
        // One direct calculation per source and receiver.
        let expected = json!({"checked":6,"pruned":if cutoff.is_null(){0}else{6}});
        assert_eq!(calculation.final_progress()["path_counts"], expected);
        assert_eq!(result["diagnostics"]["path_counts"], expected);
    }
    for spectrum in [
        json!([null, null, null, null, null, null, null, null]),
        json!([0, 90, null, null, null, null, null, null]),
    ] {
        let mut input = point_request(spectrum);
        input["settings"]["minimum_contribution_db"] = json!(0);
        let calculation = calculate(&engine, &input);
        let result = &calculation.result;
        // Exact silence and a partly cut family are checked, but not fully pruned.
        assert_eq!(
            calculation.final_progress()["path_counts"],
            json!({"checked":1,"pruned":0})
        );
        assert_eq!(
            result["diagnostics"]["path_counts"],
            calculation.final_progress()["path_counts"]
        );
    }
}

#[test]
fn reusable_source_transfers_use_the_normal_calculation_lifecycle() {
    let engine = Engine::start(2);
    let capabilities = engine.request(json!({"op":"capabilities"}), b"").header["ok"].clone();
    assert!(
        capabilities["features"]
            .as_array()
            .unwrap()
            .contains(&json!("source_receiver_transfers"))
    );
    let mut request = point_request(json!([100, 100, 100, 100, 100, 100, 100, 100]));
    request["settings"]["area_absolute_tolerance"] = json!(0);
    request["output"] = json!({"source_power_transfers":true});
    let result = self::result(&engine, &request);
    assert_eq!(
        result["source_power_transfers"],
        json!({
        "reference_db":100., "power_basis":"per_source_input", "pruning":"disabled"})
    );
    assert_eq!(
        result["receivers"][0]["sources"][0]["source_id"],
        "source-α"
    );
    assert_eq!(result["warning_summary"]["warnings"], 0);
    for b in 0..8 {
        assert_eq!(
            result["receivers"][0]["sources"][0]["omitted_energy"][b],
            0.
        );
    }
}
