//! Supplementary accounting checks for the absolute contribution cutoff, not
//! published ISO cases. The cutoff prunes only families whose conservative
//! upper bound is below it, before path construction; computed contributions
//! are always kept. References are the same requests with the cutoff disabled.
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;

fn calculate(v: &Value) -> Value {
    rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap()
        .install(|| iso_rust::calculate(v, &AtomicBool::new(false), &|_, _| {}))
        .unwrap_or_else(|e| panic!("{e:?}"))
}
fn energy(level: &Value) -> f64 {
    level.as_f64().map_or(0., |l| 10f64.powf(l / 10.))
}
fn db(e: f64) -> f64 {
    10. * e.log10()
}
/// Cut energy plus its reported omission brackets the uncut energy per band.
fn assert_bracketed(cut: &Value, uncut: &Value) {
    for (c, u) in cut["receivers"]
        .as_array()
        .unwrap()
        .iter()
        .zip(uncut["receivers"].as_array().unwrap())
    {
        for b in 0..8 {
            let (computed, reference) = (energy(&c["octave_db"][b]), energy(&u["octave_db"][b]));
            let omitted = c["omitted_energy"][b].as_f64().unwrap();
            assert!(computed <= reference * (1. + 1e-12), "band {b}");
            assert!(
                reference - computed <= omitted * (1. + 1e-12) + 1e-300,
                "band {b}"
            );
        }
    }
}

fn screened_point() -> Value {
    json!({
        "sources":[{"id":"s","kind":"point","position":[-10,5,2],"spectrum":[null,null,null,null,100,null,null,null]}],
        "receivers":[{"id":"r","position":[20,5,2]}],
        "buildings":[{"id":"b","footprint":[[0,0,0],[10,0,0],[10,10,0],[0,10,0]],"height_m":10}],
        "settings":{"max_reflect_order":0,"minimum_contribution_db":null},
        "output":{"path_diagnostics":"summary"}
    })
}

#[test]
fn computed_point_contribution_below_the_cutoff_is_kept() {
    let mut v = screened_point();
    let uncut = calculate(&v);
    let screened = db(energy(&uncut["receivers"][0]["octave_db"][4]));
    let ledger = uncut["path_diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["band"] == 4)
        .unwrap();
    // The unscreened family cap, at the source's 100 dB band power.
    let cap = db(ledger["unscreened_unit_energy"].as_f64().unwrap()) + 100.;
    assert!(cap > screened + 1.);
    v["settings"]["minimum_contribution_db"] = json!((cap + screened) / 2.);
    let cut = calculate(&v);
    assert_eq!(cut["receivers"], uncut["receivers"]);
    assert_eq!(cut["receivers"][0]["state"], "calculated");
    assert_eq!(cut["diagnostics"]["path_search"]["bounded_pruned_bands"], 0);
}

fn extended(kind: &str, x: f64) -> Value {
    let source = if kind == "line" {
        json!({"id":"e","kind":"line","vertices":[[x,0,2],[x+20.,0,2],[x+20.,10,2]],
            "spectrum":[70,75,80,85,90,85,80,75]})
    } else {
        json!({"id":"e","kind":"area","triangles":[
            [[x,0,2],[x+20.,0,2],[x+20.,10,2]],[[x,0,2],[x+20.,10,2],[x,10,2]]],
            "spectrum":[70,75,80,85,90,85,80,75]})
    };
    json!({
        "sources":[source,
            {"id":"near","kind":"point","position":[-20,0,2],"spectrum":[60,60,60,60,60,60,60,60]}],
        "receivers":[{"id":"r","position":[0,0,2]}],
        "settings":{"max_reflect_order":0,"minimum_contribution_db":null}
    })
}

#[test]
fn distant_extended_family_is_skipped_on_its_bound_before_integration() {
    for kind in ["line", "area"] {
        let mut v = extended(kind, 2000.);
        let uncut = calculate(&v);
        let reports = |r: &Value| {
            r["diagnostics"]["integration_summary"]["zone_integrations"]
                .as_u64()
                .unwrap()
        };
        assert!(reports(&uncut) > 0);
        // Above every band of the distant source, below the near point source.
        v["settings"]["minimum_contribution_db"] = json!(20);
        let cut = calculate(&v);
        assert_eq!(reports(&cut), 0, "{kind}: integration ran");
        assert!(cut["operations"].as_u64() < uncut["operations"].as_u64());
        let source = &cut["receivers"][0]["sources"][0];
        assert_eq!(source["state"], "cutoff_limited");
        for b in 0..8 {
            let bound = source["omitted_energy"][b].as_f64().unwrap();
            assert!(bound > 0. && db(bound) < 20.);
            assert!(bound >= energy(&uncut["receivers"][0]["sources"][0]["octave_db"][b]));
        }
        assert_eq!(
            cut["receivers"][0]["sources"][1],
            uncut["receivers"][0]["sources"][1]
        );
        assert_bracketed(&cut, &uncut);
    }
}

#[test]
fn extended_family_below_the_cutoff_but_not_its_bound_is_kept() {
    for kind in ["line", "area"] {
        let mut v = extended(kind, 200.);
        let uncut = calculate(&v);
        let level = energy(&uncut["receivers"][0]["sources"][0]["octave_db"][0]);
        // Just above the computed 63 Hz level, so only a late cut would drop it.
        let cutoff = db(level) + 0.01;
        v["settings"]["minimum_contribution_db"] = json!(cutoff);
        let cut = calculate(&v);
        let (c, u) = (
            &cut["receivers"][0]["sources"][0],
            &uncut["receivers"][0]["sources"][0],
        );
        assert_eq!(c["octave_db"][0], u["octave_db"][0], "{kind}");
        for b in 0..8 {
            // Quieter bands may still be skipped, but only on a bound below it.
            if c["octave_db"][b] != u["octave_db"][b] {
                let bound = c["omitted_energy"][b].as_f64().unwrap();
                assert!(
                    c["octave_db"][b].is_null() && db(bound) < cutoff,
                    "{kind} band {b}"
                );
            }
        }
        assert_bracketed(&cut, &uncut);
    }
}

#[test]
fn partly_pruned_extended_family_integrates_only_surviving_bands() {
    for kind in ["line", "area"] {
        let mut v = extended(kind, 400.);
        let uncut = calculate(&v);
        let source = |r: &Value| r["receivers"][0]["sources"][0].clone();
        let levels: Vec<f64> = (0..8)
            .map(|b| db(energy(&source(&uncut)["octave_db"][b])))
            .collect();
        // Between the quietest band's bound and the loudest band's level.
        v["settings"]["minimum_contribution_db"] = json!(levels[0] + 3.);
        let cut = calculate(&v);
        let (c, u) = (source(&cut), source(&uncut));
        let mut kept = 0;
        for b in 0..8 {
            if c["octave_db"][b].is_null() {
                assert!(c["omitted_energy"][b].as_f64().unwrap() >= energy(&u["octave_db"][b]));
            } else {
                kept += 1;
                assert_eq!(c["octave_db"][b], u["octave_db"][b], "{kind} band {b}");
            }
        }
        assert!(c["octave_db"][0].is_null() && kept > 0, "{kind}");
        assert_bracketed(&cut, &uncut);
    }
}
