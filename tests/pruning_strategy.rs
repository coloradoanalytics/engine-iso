//! Supplementary checks of automatic live pruning, not published ISO cases.
//! References are the same requests calculated without pruning.
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;

fn calculate(v: &Value, threads: usize) -> Value {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap()
        .install(|| iso_rust::calculate(v, &AtomicBool::new(false), &|_, _| {}))
        .unwrap_or_else(|e| panic!("{e:?}"))
}
fn energy(level: &Value) -> f64 {
    level.as_f64().map_or(0., |l| 10f64.powf(l / 10.))
}
const A: [f64; 8] = [-26.2, -16.1, -8.6, -3.2, 0.0, 1.2, 1.0, -1.1];
const C: [f64; 8] = [-0.8, -0.2, 0.0, 0.0, 0.0, -0.2, -0.8, -3.0];

/// A courtyard of buildings with sources and receivers spread across it.
fn scene(order: usize) -> Value {
    let mut buildings = vec![];
    for (i, (x, y)) in [(0., 0.), (60., 0.), (0., 60.), (60., 60.), (30., 120.)]
        .iter()
        .enumerate()
    {
        buildings.push(json!({"id":format!("b{i}"),"footprint":[[x,y,0],[x+20.,*y,0],[x+20.,y+20.,0],[*x,y+20.,0]],"height_m":12}));
    }
    let mut sources = vec![];
    for i in 0..12 {
        let (x, y) = (5. + 7. * i as f64, 40. + (i % 3) as f64 * 5.);
        sources.push(
            json!({"id":format!("s{i}"),"kind":"point","position":[x,y,2],
            "spectrum":[85,88,90,92,95,92,88,84]}),
        );
    }
    json!({
        "sources":sources,
        "receivers":[{"id":"near","position":[40,30,1.5]},{"id":"mid","position":[150,45,1.5]},
                     {"id":"far","position":[400,-200,1.5]}],
        "buildings":buildings,
        "settings":{"max_reflect_order":order,"minimum_contribution_db":null}
    })
}

#[test]
fn strategy_is_validated_and_exclusive_with_a_fixed_cutoff() {
    let mut v = scene(0);
    for strategy in ["dba", "dbc"] {
        v["settings"]["pruning_strategy"] = json!(strategy);
        assert!(iso_rust::validation::validate(&v).is_ok());
    }
    v["settings"]["pruning_strategy"] = json!("octave");
    assert!(iso_rust::validation::validate(&v).is_err());
    v["settings"]["pruning_strategy"] = json!("dba");
    v["settings"]["minimum_contribution_db"] = json!(0);
    assert!(iso_rust::validation::validate(&v).is_err());
    v["settings"]["pruning_strategy"] = Value::Null;
    assert!(iso_rust::validation::validate(&v).is_ok());
}

fn quietest(result: &Value, weights: [f64; 8]) -> f64 {
    result["receivers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            let e: f64 = (0..8)
                .map(|b| energy(&r["octave_db"][b]) * 10f64.powf(weights[b] / 10.))
                .sum();
            10. * e.log10()
        })
        .fold(f64::INFINITY, f64::min)
}

#[test]
fn live_cutoffs_lie_between_the_direct_and_final_quietest_levels() {
    for (strategy, weights, constant) in [("dba", A, 15.), ("dbc", C, 20.)] {
        let mut v = scene(2);
        let uncut = calculate(&v, 4);
        v["settings"]["pruning_strategy"] = json!(strategy);
        let mut direct = v.clone();
        direct["settings"]["max_reflect_order"] = json!(0);
        // One worker runs tasks in order, so every reflection task starts
        // after all direct paths have completed.
        let result = calculate(&v, 1);
        let pruning = &result["diagnostics"]["pruning"];
        assert_eq!(pruning["strategy"], strategy);
        let margin = 10. * 12f64.log10() + constant;
        assert!((pruning["margin_db"].as_f64().unwrap() - margin).abs() < 1e-12);
        let orders = pruning["stages"].as_array().unwrap();
        assert_eq!(orders.len(), 3);
        // Direct paths are never pruned.
        assert_eq!(orders[0]["tasks_without_cutoff"], 12);
        assert!(orders[0]["weighted_cutoff_db"].is_null());
        // Contributions only add energy, so each weighted cutoff lies
        // between the direct-path quietest level and the final uncut quietest
        // level, each less the margin.
        let low = quietest(&calculate(&direct, 4), weights) - margin;
        let high = quietest(&uncut, weights) - margin;
        for order in &orders[1..] {
            assert_eq!(order["tasks_without_cutoff"], 0, "{strategy}");
            let range = order["weighted_cutoff_db"].as_array().unwrap();
            let (lo, hi) = (range[0].as_f64().unwrap(), range[1].as_f64().unwrap());
            assert!(
                low - 1e-9 <= lo && lo <= hi && hi <= high + 1e-9,
                "{strategy}: {lo} {hi} not in [{low}, {high}]"
            );
        }
        // Later orders see more completed energy.
        let lowest = |k: usize| orders[k]["weighted_cutoff_db"][0].as_f64().unwrap();
        assert!(lowest(2) >= lowest(1), "{strategy}");
    }
}

#[test]
fn live_pruning_stays_close_to_the_uncut_result_at_any_worker_count() {
    let v = scene(3);
    let uncut = calculate(&v, 4);
    // Without pruning, results do not depend on scheduling.
    assert_eq!(calculate(&v, 1)["receivers"], uncut["receivers"]);
    for strategy in ["dba", "dbc"] {
        let mut cut = v.clone();
        cut["settings"]["pruning_strategy"] = json!(strategy);
        // Live cutoffs depend on completion order, so pruned results may
        // differ slightly between runs; each must stay close to the uncut one.
        for threads in [1, 4] {
            let result = calculate(&cut, threads);
            for (c, u) in result["receivers"]
                .as_array()
                .unwrap()
                .iter()
                .zip(uncut["receivers"].as_array().unwrap())
            {
                let (cd, ud) = (
                    c["a_weighted_db"].as_f64().unwrap(),
                    u["a_weighted_db"].as_f64().unwrap(),
                );
                // Pruning only removes energy; the margin keeps the loss small.
                assert!(
                    cd <= ud + 1e-9 && ud - cd < 0.05,
                    "{strategy}/{threads}: {cd} vs {ud}"
                );
                // The per-source breakdown still sums to the receiver total.
                for b in 0..8 {
                    let sum: f64 = c["sources"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|s| energy(&s["octave_db"][b]))
                        .sum();
                    let total = energy(&c["octave_db"][b]);
                    assert!(
                        (sum - total).abs() <= 1e-9 * total.max(1e-300),
                        "{strategy} band {b}"
                    );
                }
            }
        }
    }
}

#[test]
fn prefix_pruning_drops_only_families_below_their_cutoff() {
    // A wall behind the sources reflects towards one distant receiver.
    let mut v = json!({
        "sources":[{"id":"a","kind":"point","position":[8,8,1],"spectrum":[85,85,85,85,85,85,85,85]},
                   {"id":"b","kind":"point","position":[20,8,1],"spectrum":[85,85,85,85,85,85,85,85]}],
        "receivers":[{"id":"r","position":[300,8.5,1.5]}],
        "buildings":[{"id":"wall","footprint":[[-6,4,0],[-2,4,0],[-2,12,0],[-6,12,0]],"height_m":4}],
        "settings":{"max_reflect_order":2,"minimum_contribution_db":null}
    });
    v["output"] = json!({"path_diagnostics":"summary"});
    let families = |r: &Value| -> std::collections::HashMap<String, Vec<f64>> {
        r["path_diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p.get("completed_family_energy").is_some())
            .map(|p| {
                (
                    format!(
                        "{}|{}|{}",
                        p["source_id"], p["receiver_id"], p["reflection_faces"]
                    ),
                    serde_json::from_value(p["completed_family_energy"].clone()).unwrap(),
                )
            })
            .collect()
    };
    let uncut = families(&calculate(&v, 4));
    for cutoff in [35., 45.] {
        v["settings"]["minimum_contribution_db"] = json!(cutoff);
        let result = calculate(&v, 4);
        let cut = families(&result);
        assert!(cut.keys().all(|k| uncut.contains_key(k)));
        let dropped: Vec<_> = uncut
            .iter()
            .filter(|(k, _)| !cut.contains_key(*k))
            .collect();
        assert!(
            result["diagnostics"]["path_search"]["prefix_pruned_beams"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert!(!dropped.is_empty(), "cutoff {cutoff}: nothing was pruned");
        for (key, bands) in dropped {
            for e in bands {
                assert!(*e == 0. || 10. * e.log10() < cutoff, "{key}: {e}");
            }
        }
    }
}
