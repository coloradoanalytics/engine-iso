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
        // Direct paths have no weighted cutoff: they are ranked, and with
        // one worker every receiver is ranked from all of its leaders.
        assert_eq!(orders[0]["tasks_without_cutoff"], 0);
        assert!(orders[0]["weighted_cutoff_db"].is_null());
        let pairs = &pruning["direct"];
        assert_eq!(pairs["unranked_pairs"], 0);
        let count = |key: &str| pairs[key].as_u64().unwrap();
        assert_eq!(
            count("leading_pairs") + count("evaluated_pairs") + count("omitted_pairs"),
            12 * 3
        );
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
        // Reflections only: ranked direct paths have their own rule.
        if record["reflection_faces"].as_array().unwrap().is_empty() {
            continue;
        }
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

/// Three strong sources among the receivers and twenty weak ones far away,
/// one building, direct paths only.
fn ranked_scene() -> Value {
    let mut sources = vec![];
    for i in 0..3 {
        sources.push(json!({"id":format!("strong{i}"),"kind":"point","position":[10. + 40. * i as f64, 0., 2.],
            "spectrum":[95,98,100,102,105,102,98,94]}));
    }
    for i in 0..20 {
        let y = 400. * (0.3 * i as f64).sin();
        sources.push(
            json!({"id":format!("weak{i}"),"kind":"point","position":[600. + 30. * i as f64, y, 2.],
            "spectrum":[55,58,60,62,65,62,58,54]}),
        );
    }
    json!({
        "sources":sources,
        "receivers":[{"id":"a","position":[20,15,1.5]},{"id":"b","position":[60,-20,1.5]},
                     {"id":"c","position":[100,30,1.5]},{"id":"d","position":[700,50,1.5]}],
        "buildings":[{"id":"hall","footprint":[[30,5,0],[50,5,0],[50,10,0],[30,10,0]],"height_m":8}],
        "settings":{"max_reflect_order":0,"minimum_contribution_db":null}
    })
}

/// Direct paths are ranked at each receiver. What is omitted is reported by
/// its bound, the bounds sum in every band to at most 10^(-c/10) of the energy
/// computed there, so the loss of each band and of the weighted level stays
/// within 10 lg(1 + 10^(-c/10)), and the result does not depend on the number of workers.
#[test]
fn ranked_direct_paths_omit_only_a_bounded_share_at_each_receiver() {
    let uncut = calculate(&ranked_scene(), 4);
    for (strategy, weights, constant) in [("dba", A, 20.), ("dbc", C, 25.)] {
        let mut v = ranked_scene();
        v["settings"]["pruning_strategy"] = json!(strategy);
        let result = calculate(&v, 1);
        let share = 10f64.powf(-constant / 10.);
        let pairs = &result["diagnostics"]["pruning"]["direct"];
        let count = |key: &str| pairs[key].as_u64().unwrap();
        assert!(count("omitted_pairs") > 0, "{strategy}: {pairs}");
        assert!(count("leading_pairs") >= 4, "{strategy}: {pairs}");
        assert_eq!(count("unranked_pairs"), 0);
        assert_eq!(
            count("leading_pairs") + count("evaluated_pairs") + count("omitted_pairs"),
            23 * 4
        );
        let levels = weighted_levels(&result, weights);
        let exact = weighted_levels(&uncut, weights);
        for (r, receiver) in result["receivers"].as_array().unwrap().iter().enumerate() {
            let weighted = |bands: &Value, level: bool| -> f64 {
                (0..8)
                    .map(|b| {
                        let e = if level {
                            energy(&bands[b])
                        } else {
                            bands[b].as_f64().unwrap()
                        };
                        e * 10f64.powf(weights[b] / 10.)
                    })
                    .sum()
            };
            let computed = weighted(&receiver["octave_db"], true);
            let omitted = weighted(&receiver["omitted_energy"], false);
            assert!(
                omitted <= share * computed * (1. + 1e-9),
                "{strategy} receiver {r}: omitted {omitted}, computed {computed}"
            );
            for b in 0..8 {
                let (kept, all) = (
                    energy(&receiver["octave_db"][b]),
                    energy(&uncut["receivers"][r]["octave_db"][b]),
                );
                let bound = receiver["omitted_energy"][b].as_f64().unwrap();
                assert!(bound <= share * kept * (1. + 1e-9), "{strategy} {r} {b}");
                assert!(
                    all <= kept * (1. + share) * (1. + 1e-9),
                    "{strategy} {r} {b}"
                );
            }
            // Pruning only removes energy, and no more than the bound.
            let loss = exact[r] - levels[r];
            assert!(
                (-1e-9..=10. * (1. + share).log10() + 1e-9).contains(&loss),
                "{strategy} receiver {r}: loss {loss}"
            );
            let mut omitted_sources = 0;
            for source in receiver["sources"].as_array().unwrap() {
                let bound: f64 = (0..8)
                    .map(|b| source["omitted_energy"][b].as_f64().unwrap())
                    .sum();
                if bound > 0. {
                    // Not evaluated: it has no level, only its bound.
                    assert!((0..8).all(|b| source["octave_db"][b].is_null()));
                    assert!(
                        source["source_id"].as_str().unwrap().starts_with("weak"),
                        "{strategy}: {}",
                        source["source_id"]
                    );
                    omitted_sources += 1;
                }
            }
            // The strong sources lead at the receivers among them.
            if r < 3 {
                assert!(omitted_sources > 0, "{strategy} receiver {r}");
            }
        }
        // The same at any worker count: every receiver is ranked from all
        // of its leaders before anything is omitted.
        let again = calculate(&v, 4);
        assert_eq!(result["receivers"], again["receivers"], "{strategy}");
        assert_eq!(
            result["diagnostics"]["pruning"], again["diagnostics"]["pruning"],
            "{strategy}"
        );
    }
}

/// A source that adds next to nothing to the weighted level but carries one
/// octave band is not ranked out: every band keeps its own share.
#[test]
fn a_band_carried_by_a_weak_source_is_kept() {
    let v = json!({
        "sources":[{"id":"hum","kind":"point","position":[0,0,2],"spectrum":[60,70,90,100,105,60,0,0]},
                   {"id":"hiss","kind":"point","position":[0,10,2],"spectrum":[0,0,0,0,0,0,60,0]}],
        "receivers":[{"id":"r","position":[60,5,1.5]}],
        "settings":{"max_reflect_order":0,"minimum_contribution_db":null}
    });
    let uncut = calculate(&v, 1);
    for strategy in ["dba", "dbc"] {
        let mut cut = v.clone();
        cut["settings"]["pruning_strategy"] = json!(strategy);
        let result = calculate(&cut, 1);
        assert_eq!(
            result["diagnostics"]["pruning"]["direct"]["leading_pairs"],
            1
        );
        assert_eq!(
            result["receivers"][0]["octave_db"], uncut["receivers"][0]["octave_db"],
            "{strategy}"
        );
    }
}
