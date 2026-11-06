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

/// Weighted level of every receiver, in request order.
fn weighted_levels(result: &Value, weights: [f64; 8]) -> Vec<f64> {
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
        .collect()
}
fn lowest(levels: &[f64]) -> f64 {
    levels.iter().copied().fold(f64::INFINITY, f64::min)
}
fn highest(levels: &[f64]) -> f64 {
    levels.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

#[test]
fn live_thresholds_follow_each_receivers_own_level() {
    for (strategy, weights, constant) in [("dba", A, 20.), ("dbc", C, 25.)] {
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
        // Contributions only add energy, so every receiver's threshold lies
        // between its direct-path level and its final uncut level, each less
        // the margin. The reported range is the lowest and highest threshold
        // applied: the quietest and the loudest receiver bound them.
        let first = weighted_levels(&calculate(&direct, 4), weights);
        let last = weighted_levels(&uncut, weights);
        for order in &orders[1..] {
            assert_eq!(order["tasks_without_cutoff"], 0, "{strategy}");
            let range = order["weighted_cutoff_db"].as_array().unwrap();
            let (lo, hi) = (range[0].as_f64().unwrap(), range[1].as_f64().unwrap());
            assert!(
                lowest(&first) - margin - 1e-9 <= lo && lo <= lowest(&last) - margin + 1e-9,
                "{strategy}: lowest {lo}"
            );
            assert!(
                highest(&first) - margin - 1e-9 <= hi && hi <= highest(&last) - margin + 1e-9,
                "{strategy}: highest {hi}"
            );
            // The receivers differ by tens of decibels, and so do their
            // thresholds: the loud one is not held to the quiet one's.
            assert!(hi > lowest(&last) - margin + 20., "{strategy}: {lo} {hi}");
        }
        // Later orders see more completed energy.
        let low = |k: usize| orders[k]["weighted_cutoff_db"][0].as_f64().unwrap();
        assert!(low(2) >= low(1), "{strategy}");
    }
}

/// Every skipped band is below the threshold of the receiver it would have
/// reached, and the loud receiver skips contributions that the quietest
/// receiver's threshold would have kept.
#[test]
fn each_receiver_is_pruned_against_its_own_level() {
    let mut v = scene(2);
    let uncut = weighted_levels(&calculate(&v, 4), A);
    v["settings"]["pruning_strategy"] = json!("dba");
    v["output"] = json!({"path_diagnostics":"summary"});
    let result = calculate(&v, 1);
    let margin = 10. * 12f64.log10() + 20.;
    let ids = ["near", "mid", "far"];
    let mut skipped = [0usize; 3];
    let mut beyond_quietest = [0usize; 3];
    for record in result["path_diagnostics"].as_array().unwrap() {
        let Some(omitted) = record.get("omitted_energy") else {
            continue;
        };
        let r = ids
            .iter()
            .position(|id| record["receiver_id"] == *id)
            .unwrap();
        for b in 0..8 {
            let bound = omitted[b].as_f64().unwrap();
            if bound == 0. {
                continue;
            }
            let weighted = 10. * bound.log10() + A[b];
            // A threshold never exceeds the receiver's final level less the margin.
            assert!(
                weighted < uncut[r] - margin + 1e-9,
                "{}: band {b} bound {weighted} above its own threshold",
                ids[r]
            );
            skipped[r] += 1;
            if weighted >= lowest(&uncut) - margin {
                beyond_quietest[r] += 1;
            }
        }
    }
    assert!(skipped.iter().all(|n| *n > 0), "{skipped:?}");
    // near is the loudest receiver, far the quietest.
    assert!(uncut[0] > uncut[2] + 20.);
    assert!(beyond_quietest[0] > 0, "{beyond_quietest:?}");
    assert_eq!(beyond_quietest[2], 0, "{beyond_quietest:?}");
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
