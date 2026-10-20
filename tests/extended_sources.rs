//! Supplementary extended-source convergence checks, not published ISO cases.
//! Each integrated line/area source is compared with an independent dense
//! point-source discretization of the same emission in the same scene.
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

fn lerp(a: &[f64], b: &[f64], t: f64) -> Vec<f64> {
    a.iter().zip(b).map(|(a, b)| a + t * (b - a)).collect()
}

fn point(id: usize, position: Vec<f64>, spectrum: &[f64], share: f64) -> Value {
    json!({"id":format!("q{id}"),"kind":"point","position":position,
        "spectrum":spectrum.iter().map(|l| l + 10. * share.log10()).collect::<Vec<_>>()})
}

/// Midpoint rule on `step`-metre pieces; power per metre times piece length.
fn line_as_points(source: &Value, step: f64) -> Vec<Value> {
    let spectrum: Vec<f64> = serde_json::from_value(source["spectrum"].clone()).unwrap();
    let vertices: Vec<Vec<f64>> = serde_json::from_value(source["vertices"].clone()).unwrap();
    let mut out = vec![];
    for w in vertices.windows(2) {
        let length = w[0]
            .iter()
            .zip(&w[1])
            .map(|(a, b)| (b - a).powi(2))
            .sum::<f64>()
            .sqrt();
        let n = (length / step).ceil() as usize;
        for i in 0..n {
            let p = lerp(&w[0], &w[1], (i as f64 + 0.5) / n as f64);
            out.push(point(out.len(), p, &spectrum, length / n as f64));
        }
    }
    out
}

/// Centroids of an n x n barycentric subdivision of each triangle.
fn area_as_points(source: &Value, step: f64) -> Vec<Value> {
    let spectrum: Vec<f64> = serde_json::from_value(source["spectrum"].clone()).unwrap();
    let triangles: Vec<[Vec<f64>; 3]> =
        serde_json::from_value(source["triangles"].clone()).unwrap();
    let area = |t: &[Vec<f64>; 3]| {
        let u: Vec<f64> = (0..3).map(|k| t[1][k] - t[0][k]).collect();
        let v: Vec<f64> = (0..3).map(|k| t[2][k] - t[0][k]).collect();
        0.5 * ((u[1] * v[2] - u[2] * v[1]).powi(2)
            + (u[2] * v[0] - u[0] * v[2]).powi(2)
            + (u[0] * v[1] - u[1] * v[0]).powi(2))
        .sqrt()
    };
    let total: f64 = triangles.iter().map(area).sum();
    let mut out = vec![];
    for t in &triangles {
        let a = area(t);
        let n = ((2. * a).sqrt() / step).ceil() as usize;
        let share = a / total / (n * n) as f64;
        for i in 0..n {
            for j in 0..n - i {
                let mut centroids = vec![[
                    (i as f64 + 1. / 3.) / n as f64,
                    (j as f64 + 1. / 3.) / n as f64,
                ]];
                if j + 1 < n - i {
                    centroids.push([
                        (i as f64 + 2. / 3.) / n as f64,
                        (j as f64 + 2. / 3.) / n as f64,
                    ]);
                }
                for [x, y] in centroids {
                    let p = (0..3)
                        .map(|k| t[0][k] + x * (t[1][k] - t[0][k]) + y * (t[2][k] - t[0][k]))
                        .collect();
                    out.push(point(out.len(), p, &spectrum, share));
                }
            }
        }
    }
    out
}

fn assert_matches_points(request: Value, points: Vec<Value>) {
    let integrated = calculate(&request);
    let mut discrete = request.clone();
    discrete["sources"] = Value::Array(points);
    let discrete = calculate(&discrete);
    for (a, b) in integrated["receivers"]
        .as_array()
        .unwrap()
        .iter()
        .zip(discrete["receivers"].as_array().unwrap())
    {
        let la = a["a_weighted_db"].as_f64().unwrap();
        let lb = b["a_weighted_db"].as_f64().unwrap();
        assert!(
            (la - lb).abs() <= 0.05,
            "{}: {la} vs {lb}",
            a["receiver_id"]
        );
        for (x, y) in a["octave_db"]
            .as_array()
            .unwrap()
            .iter()
            .zip(b["octave_db"].as_array().unwrap())
        {
            let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
            if x.max(y) >= 10. {
                assert!((x - y).abs() <= 0.15, "{}: {x} vs {y}", a["receiver_id"]);
            }
        }
    }
}

#[test]
fn obstructed_line_source_with_foliage_and_reflections_converges() {
    // Every receiver path crosses foliage, a barrier or a building, and the
    // 3 m panel is ineligible in low bands: previously each of these kept
    // cells unresolved until the depth limit failed the job.
    let request = json!({
        "sources":[{"id":"l1","kind":"line","vertices":[[5,200,0.5],[120,205,0.5],[250,215,0.5]],
            "power_basis":"power_per_m","spectrum":[70,72,74,75,74,72,68,62]}],
        "receivers":[{"id":"r1","position":[280,260,1.5]},{"id":"r3","position":[40,170,1.5]}],
        "reflectors":[{"id":"panel","start":[200,150,0],"end":[203,150,0],"height_m":5}],
        "barriers":[{"id":"wall","vertices":[[20,180,0],[60,190,0],[90,185,0]],"heights_m":[3,4,3],
            "role":"screen_and_reflector","opacity":{"kind":"declared_opaque","basis":"test"},"absorption":[0.2,0.2,0.2,0.2,0.2,0.2,0.2,0.2]}],
        "buildings":[{"id":"b0","footprint":[[130,225,0],[145,225,0],[145,235,0],[130,235,0]],"height_m":6,
            "absorption":[0.1,0.1,0.1,0.1,0.1,0.1,0.1,0.1]}],
        "misc_regions":[{"id":"trees","kind":"foliage","method_profile":"iso9613_2024_foliage_simplified_acalc_v1",
            "boundary":[[160,180],[230,180],[230,230],[160,230]],"canopy_height_m":10}],
        "ground":{"factor":0.3,"region_policy":"nonoverlapping_schema11",
            "regions":[{"id":"g2","boundary":[[150,150],[320,150],[300,280],[160,280]],"factor":0.8}]},
        "atmosphere":{"temperature_c":10,"relative_humidity_percent":70,"pressure_kpa":101.325},
        "settings":{"max_reflect_order":1,"minimum_contribution_db":null,"ground_method":"general",
            "area_relative_tolerance":0.05}
    });
    let points = line_as_points(&request["sources"][0], 0.5);
    assert_matches_points(request, points);
}

#[test]
fn area_source_over_nonplanar_terrain_is_integrated() {
    // Nonplanar terrain previously failed every extended source under the
    // general method, although point sources are evaluated on such terrain.
    let mut samples = vec![];
    for i in (0..=200).step_by(50) {
        for j in (0..=100).step_by(50) {
            let (x, y) = (f64::from(i), f64::from(j));
            samples.push(json!([
                x - 10.,
                y - 10.,
                -(1.6 + 0.8 * (x / 70.).sin() + 0.7 * (y / 50.).cos())
            ]));
        }
    }
    let request = json!({
        "sources":[{"id":"a1","kind":"area","power_basis":"total_power","spectrum":[88,88,88,88,88,88,88,88],
            "triangles":[[[100,5,1],[115,5,1],[115,20,1]],[[100,5,1],[115,20,1],[100,20,1]]]}],
        "receivers":[{"id":"r1","position":[160,40,1.5]},{"id":"r2","position":[60,60,1.5]}],
        "reflectors":[{"id":"panel","start":[200,150,0],"end":[203,150,0],"height_m":5}],
        "buildings":[{"id":"b0","footprint":[[125,10,0],[135,10,0],[135,20,0],[125,20,0]],"height_m":6,
            "absorption":[0.1,0.1,0.1,0.1,0.1,0.1,0.1,0.1]}],
        "terrain":{"samples":samples},
        "ground":{"factor":0.5},
        "atmosphere":{"temperature_c":10,"relative_humidity_percent":70,"pressure_kpa":101.325},
        "settings":{"max_reflect_order":1,"minimum_contribution_db":null,"ground_method":"general",
            "area_relative_tolerance":0.05}
    });
    let points = area_as_points(&request["sources"][0], 0.5);
    assert_matches_points(request, points);
}
