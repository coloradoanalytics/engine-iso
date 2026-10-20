//! Published scenes adapted to 2024, with independent analytic references.
//! Expected geometry/terms come from a separate verification script, which
//! neither imports nor calls this engine. These are not ISO-issued 2024 values.
use serde_json::Value;
use std::sync::atomic::AtomicBool;

fn close_points(a: &Value, b: &Value) -> bool {
    let a = a.as_array().unwrap();
    let b = b.as_array().unwrap();
    a.len() == b.len()
        && a.iter().zip(b).all(|(p, q)| {
            (0..3)
                .map(|k| (p[k].as_f64().unwrap() - q[k].as_f64().unwrap()).powi(2))
                .sum::<f64>()
                .sqrt()
                <= 1e-7
        })
}

fn check(request_text: &str, id: &str) {
    let references: Value = serde_json::from_str(include_str!(
        "fixtures/iso17534/2024-adapted-reference.json"
    ))
    .unwrap();
    let expected = references["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["case_id"] == id)
        .unwrap();
    let request: Value = serde_json::from_str(request_text).unwrap();
    iso_rust::validation::validate(&request).unwrap();
    let result = iso_rust::calculate(&request, &AtomicBool::new(false), &|_, _| {}).unwrap();
    let receiver = &result["receivers"][0];
    let tolerance = references["tolerance_db"].as_f64().unwrap();
    let close = |a: &Value, b: &Value| {
        let delta = a.as_f64().expect("finite calculated result") - b.as_f64().unwrap();
        assert!(
            delta.abs() <= tolerance,
            "{id}: actual {a}, independent {b}, delta {delta}"
        );
    };
    assert_eq!(receiver["state"], "calculated", "{id}");
    close(&receiver["a_weighted_db"], &expected["a_weighted_db"]);
    for band in 0..8 {
        close(&receiver["octave_db"][band], &expected["octave_db"][band]);
    }

    // Also anchor every scene to reported reference intervals. Only the
    // independently calculated ground/Dz/Kmet edition changes move them;
    // engine output must never supply these offsets or widen the intervals.
    let intervals: Value = serde_json::from_str(include_str!(
        "fixtures/iso17534/2024-acceptance-reference.json"
    ))
    .unwrap();
    let reported = intervals["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["case_id"] == id)
        .unwrap();
    assert_eq!(reported["values"].as_array().unwrap().len(), 9);
    for value in reported["values"].as_array().unwrap() {
        let actual = if let Some(frequency) = value["frequency_hz"].as_f64() {
            let band = iso_rust::acoustics::FREQUENCIES
                .iter()
                .position(|f| *f == frequency)
                .unwrap();
            receiver["octave_db"][band].as_f64().unwrap() + iso_rust::acoustics::A_WEIGHTING[band]
        } else {
            receiver["a_weighted_db"].as_f64().unwrap()
        };
        assert!(
            (value["lower_db"].as_f64().unwrap()..=value["upper_db"].as_f64().unwrap())
                .contains(&actual),
            "{id}, {} Hz: {actual} dB outside edition-adjusted reported interval {}..{}",
            value["frequency_hz"],
            value["lower_db"],
            value["upper_db"]
        );
    }

    let actual: Vec<_> = result["path_diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p.get("physical_route").is_some())
        .collect();
    let mut count = 0;
    for family in expected["families"].as_array().unwrap() {
        for band in 0..8 {
            if !family["eligible"][band].as_bool().unwrap() {
                continue;
            }
            for route in family["routes"].as_array().unwrap() {
                count += 1;
                let matches: Vec<_> = actual
                    .iter()
                    .filter(|p| {
                        p["band"] == band
                            && close_points(&p["physical_route"], &route["points"])
                            && close_points(&p["ground_reference"], &family["reference"])
                    })
                    .collect();
                assert_eq!(
                    matches.len(),
                    1,
                    "{id}, band {band}, route {}",
                    route["points"]
                );
                let ledger = matches[0];
                for term in ["A_atm", "A_gr", "A_refl"] {
                    close(&ledger[term], &family[term][band]);
                }
                close(&ledger["A_bar"], &route["A_bar"][band]);
                close(&ledger["A_div"], &family["A_div"]);
                close(&ledger["D_omega"], &family["D_omega"]);
                close(
                    &ledger["reference_distance_m"],
                    &family["reference_length_m"],
                );
            }
        }
    }
    assert_eq!(actual.len(), count, "{id}: unexpected or missing route");
}

macro_rules! cases {
    ($($name:ident => $id:literal),+ $(,)?) => {
        $(#[test] fn $name() {
            check(include_str!(concat!("fixtures/iso17534/", $id, ".json")), $id);
        })+
    };
}
cases! {
    t01 => "T01", t02 => "T02", t03 => "T03", t04 => "T04", t05 => "T05",
    t06 => "T06", t07 => "T07", t08 => "T08", t09 => "T09", t10 => "T10",
    t11 => "T11", t12 => "T12", t13 => "T13", t14 => "T14", t15 => "T15",
    t16 => "T16", t17 => "T17", t18 => "T18", t19 => "T19",
}

#[test]
fn t19_published_independent_band_selection_and_reported_correction() {
    // Independent evidence: Theers et al., DAGA 2026, p. 722 explicitly
    // excludes only 63/125 Hz for 2024. Wölfel's July 2025 declaration,
    // p. 10, reports the corrected legacy 250 Hz interval 23.27..23.37.
    // These published checks must not be replaced by a fitted Table 67 mask.
    // Agreement across editions for this one scene is not 2024 conformity.
    let evidence: Value =
        serde_json::from_str(include_str!("fixtures/iso17534/published-corrections.json")).unwrap();
    let request: Value = serde_json::from_str(include_str!("fixtures/iso17534/T19.json")).unwrap();
    let result = iso_rust::calculate(&request, &AtomicBool::new(false), &|_, _| {}).unwrap();
    let excluded = evidence["corroborating_sources"][0]["t19_excluded_frequencies_2024_hz"]
        .as_array()
        .unwrap();
    let paths = result["path_diagnostics"].as_array().unwrap();
    for (band, frequency) in [63, 125, 250, 500, 1000, 2000, 4000, 8000]
        .into_iter()
        .enumerate()
    {
        let reflections = paths
            .iter()
            .filter(|p| {
                p.get("physical_route").is_some()
                    && p["band"] == band
                    && !p["reflection_faces"].as_array().unwrap().is_empty()
            })
            .count();
        assert_eq!(
            reflections,
            usize::from(!excluded.iter().any(|f| f == frequency)),
            "T19: published 2024 reflection eligibility at {frequency} Hz"
        );
    }
    let bounds = &evidence["overrides"]["T19"]["band_overrides"]["250"];
    let actual_a = result["receivers"][0]["octave_db"][2].as_f64().unwrap() - 8.6;
    assert!(
        (bounds["lower_db"].as_f64().unwrap()..=bounds["upper_db"].as_f64().unwrap())
            .contains(&actual_a),
        "T19 at 250 Hz: {actual_a} dB outside reported corrected interval"
    );
}
