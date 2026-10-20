//! Supplementary analytical geometry/accounting checks, not published ISO cases.
use iso_rust::beam::{self, SearchIndex};
use iso_rust::{path::*, scene::*, types::Sum};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize};

fn request() -> Value {
    json!({
        "sources":[{"id":"s","kind":"point","position":[-10,-5,2],"spectrum":[null,null,null,null,100,null,null,null]}],
        "receivers":[{"id":"r","position":[-10,15,2]}],
        "buildings":[{"id":"b","footprint":[[0,0,0],[10,0,0],[10,10,0],[0,10,0]],"height_m":10}],
        "settings":{"max_reflect_order":1,"minimum_contribution_db":null},
        "output":{"path_diagnostics":"full"}
    })
}
fn calculate(v: &Value, threads: usize) -> Value {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap()
        .install(|| iso_rust::calculate(v, &AtomicBool::new(false), &|_, _| {}).unwrap())
}

#[test]
fn all_exterior_faces_are_candidates_without_floors_or_second_bounces() {
    let cancel = AtomicBool::new(false);
    for reverse in [false, true] {
        let mut v = request();
        if reverse {
            v["buildings"][0]["footprint"]
                .as_array_mut()
                .unwrap()
                .reverse();
        }
        let scene = Scene::prepare(&v, &cancel).unwrap();
        assert_eq!(scene.faces.len(), 5); // four walls and one reflecting roof; no floor
        let fs = families(&scene, &cancel).unwrap();
        assert_eq!(fs.len(), 6);
        assert_eq!(fs.iter().collect::<std::collections::HashSet<_>>().len(), 6);
        assert!(fs.iter().all(|f| f.len() <= 1));
        assert!(
            fs.iter()
                .flatten()
                .all(|i| scene.faces[*i].id.starts_with("wall:") || scene.faces[*i].id == "roof")
        );
        let source_faces = source_face_visibility(&scene, &scene.sources[0]);
        for (i, face) in scene.faces.iter().enumerate() {
            if face.id.starts_with("wall:") {
                // Source is southwest: only the south and west outward walls face it.
                assert_eq!(
                    source_faces[i],
                    face.normal[0] < -0.5 || face.normal[1] < -0.5
                );
            }
        }
        let result = calculate(&v, 4);
        // Calculations: the direct path and one west-wall reflection.
        assert_eq!(
            result["diagnostics"]["path_counts"],
            json!({"checked":2,"pruned":0})
        );
        assert_eq!(result["diagnostics"]["path_search"]["empty_beams"], 1);
        assert_eq!(
            result["diagnostics"]["path_search"]["receiver_connections"],
            2
        );
        let routes: Vec<_> = result["path_diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p.get("physical_route").is_some())
            .collect();
        assert_eq!(routes.len(), 2); // direct, plus exactly one west-wall reflection
        assert!(
            routes
                .iter()
                .all(|p| p["reflection_faces"].as_array().unwrap().len() <= 1)
        );
        assert_eq!(
            routes
                .iter()
                .find(|p| p["physical_route"].as_array().unwrap().len() == 3)
                .unwrap()["physical_route"][1],
            json!([0., 5., 2.])
        );
    }
}

#[test]
fn admitted_work_matches_exhaustive_uncut_point_family_evaluation() {
    let v = request();
    let cancel = AtomicBool::new(false);
    let scene = Scene::prepare(&v, &cancel).unwrap();
    let s = &scene.sources[0];
    let z = &s.zones[0];
    let operations = AtomicUsize::new(0);
    let work = Work {
        cancel: &cancel,
        operations: &operations,
    };
    let mut sum = Sum::default();
    // Comparison is a filter-equivalence regression, not an independent ISO reference.
    for f in families(&scene, &cancel).unwrap() {
        sum.add(
            transfer(
                &scene,
                s,
                z,
                z.cells[0].centroid(),
                &scene.receivers[0],
                &f,
                &work,
            )
            .unwrap()
            .energy[4]
                * z.power[4],
        );
    }
    let result = calculate(&v, 2);
    assert_eq!(
        result["receivers"][0]["octave_db"][4].as_f64().unwrap(),
        10. * sum.total().log10()
    );
}

#[test]
fn extended_source_filter_uses_whole_domain_not_centroid() {
    let scene = Scene::prepare(&request(), &AtomicBool::new(false)).unwrap();
    let west = scene.faces.iter().position(|f| f.normal[0] < -0.5).unwrap();
    let mut source = scene.sources[0].clone();
    source.kind = "line".into();
    source.zones[0].cells = vec![Cell::Line([[-1., -5., 2.], [10., -5., 2.]])];
    assert!(source_face_visibility(&scene, &source)[west]);
    source.zones[0].cells = vec![Cell::Triangle([
        [-1., -5., 2.],
        [10., -5., 2.],
        [10., -4., 2.],
    ])];
    assert!(source_face_visibility(&scene, &source)[west]);
    source.zones[0].cells = vec![Cell::Line([[1., -5., 2.], [10., -5., 2.]])];
    assert!(!source_face_visibility(&scene, &source)[west]);
}

#[test]
fn absolute_cutoff_is_strict_and_can_be_disabled() {
    let mut settings = Settings::from(&json!({"minimum_contribution_db":0}));
    assert!(!below_cutoff(&settings.cutoffs, 0, 1.));
    assert!(below_cutoff(&settings.cutoffs, 7, 0.99));
    settings.cutoffs = [None; 8];
    assert!(!below_cutoff(&settings.cutoffs, 0, 0.01));
    assert!(!below_cutoff(&settings.cutoffs, 0, 0.));
}

#[test]
fn legacy_receiver_margin_is_ignored_independently_of_worker_count() {
    let mut v = request();
    v["buildings"] = json!([]);
    v["settings"]["max_reflect_order"] = json!(0);
    let mut quiet = v["sources"][0].clone();
    quiet["id"] = json!("quiet");
    quiet["spectrum"][4] = json!(60);
    v["sources"].as_array_mut().unwrap().push(quiet);
    let uncut = calculate(&v, 1);
    for margin in [json!(null), json!(1), json!(30)] {
        v["settings"]["receiver_margin_db"] = margin;
        for workers in [1, 4] {
            let actual = calculate(&v, workers);
            assert_eq!(actual, uncut);
            assert!(actual["settings"].get("receiver_margin_db").is_none());
            assert_eq!(actual["receivers"][0]["sources"][1]["state"], "calculated");
        }
    }
}

#[test]
fn point_cap_pruning_bounds_screened_energy_and_skips_diffraction() {
    let mut v = request();
    v["sources"][0]["position"] = json!([-10, 5, 2]);
    v["receivers"][0]["position"] = json!([20, 5, 2]);
    v["settings"]["max_reflect_order"] = json!(0);
    let uncut = calculate(&v, 1);
    v["settings"]["minimum_contribution_db"] = json!(1000);
    let cut = calculate(&v, 1);
    let actual = 10f64.powf(uncut["receivers"][0]["octave_db"][4].as_f64().unwrap() / 10.);
    assert!(cut["receivers"][0]["omitted_energy"][4].as_f64().unwrap() >= actual);
    assert_eq!(
        cut["diagnostics"]["path_counts"],
        json!({"checked":1,"pruned":1})
    );
    assert!(cut["operations"].as_u64().unwrap() < uncut["operations"].as_u64().unwrap());
}

#[test]
fn receiver_population_does_not_multiply_stubs_or_reflector_searches() {
    let cancel = AtomicBool::new(false);
    let mut v = request();
    let scene = Scene::prepare(&v, &cancel).unwrap();
    let baseline = beam::search(
        &scene,
        &scene.sources[0],
        &SearchIndex::new(&scene),
        &cancel,
    )
    .unwrap();
    for i in 0..1000 {
        // Far northeast, outside both southwest-facing outgoing beams.
        v["receivers"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":format!("far-{i}"),"position":[1000+i,1000+i,2]}));
    }
    let scene = Scene::prepare(&v, &cancel).unwrap();
    let many = beam::search(
        &scene,
        &scene.sources[0],
        &SearchIndex::new(&scene),
        &cancel,
    )
    .unwrap();
    assert_eq!(many.stats.checked, baseline.stats.checked);
    assert_eq!(many.stats.reflector_candidates, 5); // five exterior surfaces, never a second query
    assert_eq!(
        many.stats.reflector_candidates,
        baseline.stats.reflector_candidates
    );
    // Only the direct beam gains receiver tests. No off-beam receiver reaches
    // exact reflected-path construction, even when very far away.
    assert_eq!(
        many.stats.receiver_candidates,
        baseline.stats.receiver_candidates + 1000
    );
    assert_eq!(
        many.stubs
            .iter()
            .filter(|s| !s.family.is_empty())
            .map(|s| s.receiver_stubs.len())
            .sum::<usize>(),
        1
    );
}

#[test]
fn octaves_and_partly_pruned_receiver_connections_do_not_inflate_stub_counts() {
    let mut v = request();
    let one_band = calculate(&v, 1);
    v["sources"][0]["spectrum"] = json!([100, 100, 100, 100, 100, 100, 100, 100]);
    let all_bands = calculate(&v, 4);
    assert_eq!(
        one_band["diagnostics"]["path_counts"],
        all_bands["diagnostics"]["path_counts"]
    );
    v["buildings"] = json!([]);
    v["settings"]["max_reflect_order"] = json!(0);
    v["settings"]["minimum_contribution_db"] = json!(40);
    v["receivers"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"far","position":[5000,5000,2]}));
    let result = calculate(&v, 4);
    assert_eq!(result["receivers"][0]["state"], "calculated");
    assert_eq!(result["receivers"][1]["state"], "cutoff_limited");
    // One calculation per receiver; only the far receiver's is pruned.
    assert_eq!(
        result["diagnostics"]["path_counts"],
        json!({"checked":2,"pruned":1})
    );
    assert_eq!(
        result["diagnostics"]["path_search"]["bounded_pruned_bands"],
        8
    );
}

#[test]
fn quiet_extended_sources_are_retained_without_receiver_margin() {
    let mut v = request();
    v["buildings"] = json!([]);
    v["settings"]["max_reflect_order"] = json!(0);
    v["settings"]["receiver_margin_db"] = json!(30);
    v["sources"].as_array_mut().unwrap().push(json!({
        "id":"quiet-line","kind":"line","vertices":[[-10,-5,2],[-10,-4,2]],
        "spectrum":[null,null,null,null,50,null,null,null]
    }));
    let result = calculate(&v, 4);
    v["settings"]
        .as_object_mut()
        .unwrap()
        .remove("receiver_margin_db");
    assert_eq!(result, calculate(&v, 1));
    assert_eq!(result["receivers"][0]["sources"][1]["state"], "calculated");
    assert_eq!(
        result["diagnostics"]["path_counts"],
        json!({"checked":2,"pruned":0})
    );
    assert_eq!(
        result["diagnostics"]["path_search"]["bounded_pruned_bands"],
        0
    );
    assert!(
        result["diagnostics"]["integration_summary"]["zone_integrations"]
            .as_u64()
            .unwrap()
            > 0
    );
}

#[test]
fn beam_connections_match_exhaustive_images_through_order_three() {
    let cancel = AtomicBool::new(false);
    for origin in [0., 700_000.] {
        let mut v = json!({"settings":{"max_reflect_order":3},
            "sources":[{"id":"s","kind":"point","position":[origin+5.,origin+5.,2.],"spectrum":[100,100,100,100,100,100,100,100]}],
            "reflectors":[
                {"id":"left","vertices":[[origin,origin-20.,0.],[origin,origin+20.,0.],[origin,origin+20.,30.],[origin,origin-20.,30.]]},
                {"id":"right","vertices":[[origin+20.,origin-20.,0.],[origin+20.,origin+20.,0.],[origin+20.,origin+20.,30.],[origin+20.,origin-20.,30.]]},
                {"id":"bottom","vertices":[[origin-20.,origin,0.],[origin+30.,origin,0.],[origin+30.,origin,30.],[origin-20.,origin,30.]]}
            ],"receivers":[]});
        for x in [-40., 1., 7., 15., 25., 100.] {
            for y in [-30., 1., 9., 15., 40.] {
                for z in [1., 6., 40.] {
                    let n = v["receivers"].as_array().unwrap().len();
                    v["receivers"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"id":format!("r{n}"),"position":[origin+x,origin+y,z]}));
                }
            }
        }
        let scene = Scene::prepare(&v, &cancel).unwrap();
        let all = families(&scene, &cancel).unwrap();
        let s = scene.sources[0].zones[0].cells[0].centroid();
        let mut expected = std::collections::BTreeSet::new();
        for f in &all {
            for (ri, r) in scene.receivers.iter().enumerate() {
                if prepare_reference(&scene, s, r.point, f).unwrap().is_some() {
                    expected.insert((f.clone(), ri));
                }
            }
        }
        let search = beam::search(
            &scene,
            &scene.sources[0],
            &SearchIndex::new(&scene),
            &cancel,
        )
        .unwrap();
        let actual: std::collections::BTreeSet<_> = search
            .stubs
            .iter()
            .flat_map(|stub| {
                stub.receiver_stubs
                    .iter()
                    .map(|c| (stub.family.clone(), c.receiver))
            })
            .collect();
        assert!(expected.iter().any(|(f, _)| f.len() == 3));
        assert_eq!(actual, expected, "origin {origin}");
        assert_eq!(
            actual.len(),
            search
                .stubs
                .iter()
                .map(|s| s.receiver_stubs.len())
                .sum::<usize>()
        );

        // A source-domain envelope must keep every connection reached by the
        // vertices and interior of an extended emitting domain, including
        // connections invisible from its centre.
        let mut area = scene.sources[0].clone();
        area.kind = "area".into();
        area.zones[0].cells = vec![Cell::Triangle([
            [origin + 1., origin + 1., 2.],
            [origin + 9., origin + 1., 2.],
            [origin + 1., origin + 9., 2.],
        ])];
        let search = beam::search(&scene, &area, &SearchIndex::new(&scene), &cancel).unwrap();
        let actual: std::collections::BTreeSet<_> = search
            .stubs
            .iter()
            .flat_map(|stub| {
                stub.receiver_stubs
                    .iter()
                    .map(|c| (stub.family.clone(), c.receiver))
            })
            .collect();
        for p in [
            [1., 1., 2.],
            [9., 1., 2.],
            [1., 9., 2.],
            [3., 3., 2.],
            [5., 5., 2.],
        ] {
            let p = [origin + p[0], origin + p[1], p[2]];
            for f in &all {
                for (ri, r) in scene.receivers.iter().enumerate() {
                    if prepare_reference(&scene, p, r.point, f).unwrap().is_some() {
                        assert!(
                            actual.contains(&(f.clone(), ri)),
                            "extended domain lost {f:?} / {ri} at {p:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn area_receiver_hits_spawn_distinct_terminal_stubs_from_one_reflection_beam() {
    let mut v = request();
    v["buildings"] = json!([]);
    v["receivers"] = json!([]);
    v["reflectors"] = json!([{"id":"wall","vertices":[[0,-20,0],[0,20,0],[0,20,20],[0,-20,20]]}]);
    v["area_receivers"] = json!([{"id":"grid","boundary":[[-20,0],[-10,0],[-10,10],[-20,10]],"spacing_m":1,"height_m":2}]);
    let cancel = AtomicBool::new(false);
    let scene = Scene::prepare(&v, &cancel).unwrap();
    let search = beam::search(
        &scene,
        &scene.sources[0],
        &SearchIndex::new(&scene),
        &cancel,
    )
    .unwrap();
    assert_eq!(scene.receivers.len(), 121);
    assert_eq!(search.stats.checked, 2); // source beam + one reflected beam
    assert_eq!(search.stats.reflector_candidates, 1); // no reflector queries after order one
    for beam in &search.stubs {
        assert_eq!(beam.receiver_stubs.len(), 121);
        let unique: std::collections::HashSet<_> =
            beam.receiver_stubs.iter().map(|s| s.receiver).collect();
        assert_eq!(unique.len(), 121); // exactly one child per receiver, no duplicates
    }
    let result = calculate(&v, 1);
    let parallel = calculate(&v, 4);
    assert_eq!(result["receivers"], parallel["receivers"]);
    assert_eq!(result["path_diagnostics"], parallel["path_diagnostics"]);
    // One calculation per receiver hit of each beam.
    assert_eq!(
        result["diagnostics"]["path_counts"],
        json!({"checked":242,"pruned":0})
    );
    assert_eq!(
        result["diagnostics"]["path_search"]["receiver_stub_counts"],
        json!({"checked":242,"pruned":0})
    );
    let routes: Vec<_> = result["path_diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| {
            p.get("physical_route").is_some()
                && p["reflection_faces"].as_array().unwrap().len() == 1
        })
        .collect();
    assert_eq!(routes.len(), 121);
    for receiver in result["receivers"].as_array().unwrap() {
        let route = routes
            .iter()
            .find(|p| p["receiver_id"] == receiver["receiver_id"])
            .unwrap();
        let rx = receiver["position"][0].as_f64().unwrap();
        let ry = receiver["position"][1].as_f64().unwrap();
        // Independent mirror construction: image (10,-5,2) to (rx,ry,2), x=0.
        let expected_y = -5. + (ry + 5.) * 10. / (10. - rx);
        let contact = &route["physical_route"][1];
        assert!(contact[0].as_f64().unwrap().abs() < 1e-10);
        assert!((contact[1].as_f64().unwrap() - expected_y).abs() < 1e-10);
        assert!((contact[2].as_f64().unwrap() - 2.).abs() < 1e-10);
        assert_eq!(route["physical_route"][2], receiver["position"]);
        // Scheduling regression, not an independent acoustic reference: each grid
        // child must equal calculating that receiver alone through the same beam.
        let mut single = v.clone();
        single.as_object_mut().unwrap().remove("area_receivers");
        single["receivers"] = json!([{"id":"single","position":receiver["position"]}]);
        assert_eq!(
            calculate(&single, 1)["receivers"][0]["octave_db"],
            receiver["octave_db"]
        );
    }
    // Place a cutoff between the weakest/strongest reflected children to ensure
    // pruning one terminal stub does not prune its siblings or their shared beam.
    let completed = |r: &Value| -> Vec<Value> {
        r["path_diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| {
                p.get("completed_family_energy").is_some()
                    && p["reflection_faces"].as_array().unwrap().len() == 1
            })
            .cloned()
            .collect()
    };
    let original = completed(&result);
    let levels: Vec<_> = original
        .iter()
        .map(|p| 10. * p["completed_family_energy"][4].as_f64().unwrap().log10())
        .collect();
    let cutoff = (levels.iter().copied().fold(f64::INFINITY, f64::min)
        + levels.iter().copied().fold(f64::NEG_INFINITY, f64::max))
        / 2.;
    v["settings"]["minimum_contribution_db"] = json!(cutoff);
    let cut = calculate(&v, 4);
    let children = completed(&cut);
    assert_eq!(children.len(), 121);
    let mut pruned = 0;
    for (before, after) in original.iter().zip(&children) {
        assert_eq!(before["receiver_id"], after["receiver_id"]);
        let energy = before["completed_family_energy"][4].as_f64().unwrap();
        // Skipped only on a family bound below the cutoff; computed energy is kept.
        if after["completed_family_energy"][4] == 0. {
            pruned += 1;
            let bound = after["omitted_energy"][4].as_f64().unwrap();
            assert!(bound >= energy * (1. - 1e-12) && 10. * bound.log10() < cutoff);
        } else {
            assert_eq!(
                after["completed_family_energy"],
                before["completed_family_energy"]
            );
        }
    }
    assert!(pruned > 0 && pruned < 121);
    assert_eq!(
        cut["diagnostics"]["path_counts"],
        cut["diagnostics"]["path_search"]["receiver_stub_counts"]
    );
    let pruned_children = cut["path_diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| {
            p.get("completed_family_energy").is_some()
                && p["completed_family_energy"][4] == 0.
                && p["omitted_energy"][4].as_f64().unwrap() > 0.
        })
        .count();
    assert_eq!(
        cut["diagnostics"]["path_search"]["receiver_stub_counts"],
        json!({"checked":242,"pruned":pruned_children})
    );
}
