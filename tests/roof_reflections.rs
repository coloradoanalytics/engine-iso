//! Supplementary analytical roof geometry; no published ISO case claims.
use iso_rust::{
    beam::{SearchIndex, search},
    path::{families, prepare_reference},
    scene::Scene,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::atomic::AtomicBool};

fn request(sloped: bool) -> Value {
    json!({
        "buildings":[{"id":"b","footprint":[[0,0,0],[10,0,0],[10,10,0],[0,10,0]],
            "roof_elevations_m":if sloped {vec![5,10,10,5]} else {vec![5,5,5,5]}}],
        "sources":[{"id":"s","kind":"point","position":if sloped {vec![2,5,11]} else {vec![2,5,8]},
            "spectrum":[null,null,null,null,90,null,null,null]}],
        "receivers":[{"id":"r","position":if sloped {vec![8,5,14]} else {vec![8,5,8]}}],
        "settings":{"max_reflect_order":1,"minimum_contribution_db":null},
        "output":{"path_diagnostics":"full"}
    })
}

#[test]
fn flat_and_sloped_roofs_reflect_without_opt_in_and_have_analytic_bounces() {
    let cancel = AtomicBool::new(false);
    for sloped in [false, true] {
        for reverse in [false, true] {
            let mut v = request(sloped);
            if reverse {
                v["buildings"][0]["footprint"]
                    .as_array_mut()
                    .unwrap()
                    .reverse();
                v["buildings"][0]["roof_elevations_m"]
                    .as_array_mut()
                    .unwrap()
                    .reverse();
            }
            let scene = Scene::prepare(&v, &cancel).unwrap();
            let roof = scene.faces.iter().position(|f| f.id == "roof").unwrap();
            assert!(scene.faces[roof].reflects);
            assert!(scene.faces[roof].normal[2] > 0.);
            let found = search(
                &scene,
                &scene.sources[0],
                &SearchIndex::new(&scene),
                &cancel,
            )
            .unwrap();
            assert!(
                found
                    .stubs
                    .iter()
                    .any(|s| s.family == vec![roof] && s.receiver_stubs.len() == 1)
            );
            let result = iso_rust::calculate(&v, &cancel, &|_, _| {}).unwrap();
            let routes: Vec<_> = result["path_diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|p| {
                    p["reflection_faces"] == json!(["b/roof"]) && p.get("physical_route").is_some()
                })
                .collect();
            assert_eq!(routes.len(), 1);
            let expected = if sloped { [7., 5., 8.5] } else { [5., 5., 5.] };
            for (k, e) in expected.iter().enumerate() {
                assert!((routes[0]["physical_route"][1][k].as_f64().unwrap() - e).abs() < 1e-10);
            }
            assert!(routes[0]["unit_route_energy"].as_f64().unwrap() > 0.);
            // The same roof stays a potential reflector, but its back cannot
            // be reached from this source outside and below the building.
            v["sources"][0]["position"] = json!([-2, 5, 1]);
            let scene = Scene::prepare(&v, &cancel).unwrap();
            let found = search(
                &scene,
                &scene.sources[0],
                &SearchIndex::new(&scene),
                &cancel,
            )
            .unwrap();
            assert!(!found.stubs.iter().any(|s| s.family == vec![roof]));
            // A source above the roof cannot reflect to a receiver behind it.
            v["sources"][0]["position"] = request(sloped)["sources"][0]["position"].clone();
            v["receivers"][0]["position"] = json!([-2, 5, 1]);
            let scene = Scene::prepare(&v, &cancel).unwrap();
            let found = search(
                &scene,
                &scene.sources[0],
                &SearchIndex::new(&scene),
                &cancel,
            )
            .unwrap();
            assert!(!found.stubs.iter().any(|s| s.family == vec![roof]));
        }
    }
}

#[test]
fn every_nonplanar_roof_facet_is_a_candidate_with_its_own_orientation() {
    let cancel = AtomicBool::new(false);
    let mut v = request(false);
    v["buildings"][0]["roof_elevations_m"] = json!([5, 5, 9, 5]);
    v["buildings"][0]["roof_triangles"] = json!([[0, 1, 2], [0, 2, 3]]);
    v["sources"][0]["position"] = json!([-2, 5, 6]);
    // At P=(1,5,5.4), reflection on z=5+0.4x maps incoming
    // (3,0,-0.6) to (51/29,0,363/145).
    v["receivers"][0]["position"] = json!([1. + 51. / 29., 5., 5.4 + 363. / 145.]);
    let scene = Scene::prepare(&v, &cancel).unwrap();
    let roofs: Vec<_> = scene
        .faces
        .iter()
        .enumerate()
        .filter(|(_, f)| f.id.starts_with("roof:"))
        .collect();
    assert_eq!(roofs.len(), 2);
    assert!(roofs.iter().all(|(_, f)| f.reflects && f.normal[2] > 0.));
    let found = search(
        &scene,
        &scene.sources[0],
        &SearchIndex::new(&scene),
        &cancel,
    )
    .unwrap();
    for (i, face) in roofs {
        let connected = found.stubs.iter().any(|s| s.family == vec![i]);
        assert_eq!(connected, face.id == "roof:0:2:3");
    }
}

#[test]
fn explicit_face_overrides_keep_their_meaning() {
    let cancel = AtomicBool::new(false);
    let mut v = request(false);
    for selected in [json!([]), json!(["wall:0:1"]), json!(["roof"])] {
        v["buildings"][0]["reflecting_faces"] = selected.clone();
        let scene = Scene::prepare(&v, &cancel).unwrap();
        let actual: Vec<_> = scene
            .faces
            .iter()
            .filter(|f| f.reflects)
            .map(|f| f.id.clone())
            .collect();
        assert_eq!(json!(actual), selected);
    }
}

#[test]
fn later_roof_bounces_match_exhaustive_images_and_stay_inside_incoming_beams() {
    let cancel = AtomicBool::new(false);
    let mut v = request(false);
    v["settings"]["max_reflect_order"] = json!(2);
    v["sources"][0]["position"] = json!([2, 5, 12]);
    v["reflectors"] =
        json!([{"id":"wall","vertices":[[15,-20,0],[15,20,0],[15,20,30],[15,-20,30]]}]);
    v["receivers"] = json!([]);
    for x in [1, 3, 5, 7, 9] {
        for z in [8, 12, 20] {
            v["receivers"]
                .as_array_mut()
                .unwrap()
                .push(json!({"id":format!("r{x}-{z}"),"position":[x,5,z]}));
        }
    }
    let scene = Scene::prepare(&v, &cancel).unwrap();
    let roof = scene.faces.iter().position(|f| f.id == "roof").unwrap();
    let point = scene.sources[0].zones[0].cells[0].centroid();
    let mut expected = BTreeSet::new();
    for f in families(&scene, &cancel).unwrap() {
        for (ri, r) in scene.receivers.iter().enumerate() {
            if prepare_reference(&scene, point, r.point, &f)
                .unwrap()
                .is_some()
            {
                expected.insert((f.clone(), ri));
            }
        }
    }
    assert!(expected.iter().any(|(f, _)| f.len() == 2 && f[1] == roof));
    let found = search(
        &scene,
        &scene.sources[0],
        &SearchIndex::new(&scene),
        &cancel,
    )
    .unwrap();
    let actual: BTreeSet<_> = found
        .stubs
        .iter()
        .flat_map(|s| {
            s.receiver_stubs
                .iter()
                .map(|c| (s.family.clone(), c.receiver))
        })
        .collect();
    assert_eq!(actual, expected);
}
