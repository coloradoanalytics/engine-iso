//! Supplementary analytic mixed-path geometry, not published ISO acceptance cases.
use serde_json::json;
use std::sync::atomic::AtomicBool;

#[test]
fn reflected_screen_path_solves_heights_without_fixed_point_drift() {
    for offset in [[0.; 3], [600000., 300000., 200.]] {
        let shift = |p: [f64; 3]| std::array::from_fn::<_, 3, _>(|k| p[k] + offset[k]);
        let request = json!({
            "sources":[{"id":"s","kind":"point","position":shift([10.,-10.,6.]),"spectrum":[null,null,null,null,100,null,null,null]}],
            "receivers":[{"id":"r","position":shift([10.,10.,1.])}],
            "reflectors":[{"id":"wall","vertices":[shift([0.,-100.,0.]),shift([0.,100.,0.]),shift([0.,100.,30.]),shift([0.,-100.,30.])]}],
            "barriers":[{"id":"screen","start":shift([2.,0.,0.]),"end":shift([2.,8.,0.]),"height_m":20,"closed":true,"surface_density_kg_m2":10}],
            "settings":{"max_reflect_order":1,"minimum_contribution_db":null},
            "output":{"path_diagnostics":"full"}
        });
        let result = iso_rust::calculate(&request, &AtomicBool::new(false), &|_, _| {}).unwrap();
        // Mirroring (10,-10) to (-10,-10) and connecting to screen corner (2,0)
        // fixes the reflection at (0,-5/3). The corner's projected section
        // fraction is 71/425. Thus h=6/6+5/6*((354/425)*h+71/425),
        // giving h=581/156 and corner height 85/26. Ordinary iteration fails
        // the original 32-step limit on this constructed scene.
        let routes: Vec<_> = result["path_diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| {
                p.get("physical_route").is_some() && p["reflection_faces"] == json!(["wall/face"])
            })
            .collect();
        let route = routes
            .iter()
            .find(|p| {
                let points = p["physical_route"].as_array().unwrap();
                points.len() == 4
                    && (points[1][1].as_f64().unwrap() - offset[1] + 5. / 3.).abs() < 1e-8
            })
            .expect("the valid reflected lateral route must be retained");
        // The independent affine update contracts by (5/6)*(354/425).
        // A 1 cm update criterion bounds its remaining height tail below 3.4 cm.
        let points = route["physical_route"].as_array().unwrap();
        for (k, (actual, expected)) in points[1]
            .as_array()
            .unwrap()
            .iter()
            .zip(shift([0., -5. / 3., 581. / 156.]))
            .enumerate()
        {
            assert!(
                (actual.as_f64().unwrap() - expected).abs() < if k == 2 { 0.034 } else { 1e-8 },
                "actual={}, expected={expected}, error={}",
                actual,
                (actual.as_f64().unwrap() - expected).abs()
            );
        }
        for (k, (actual, expected)) in points[2]
            .as_array()
            .unwrap()
            .iter()
            .zip(shift([2., 0., 85. / 26.]))
            .enumerate()
        {
            assert!(
                (actual.as_f64().unwrap() - expected).abs() < if k == 2 { 0.034 } else { 1e-8 },
                "actual={}, expected={expected}, error={}",
                actual,
                (actual.as_f64().unwrap() - expected).abs()
            );
        }
        assert!(route["unit_route_energy"].as_f64().unwrap() > 0.);
    }
}

#[test]
fn sliding_roof_contact_keeps_the_two_reflection_family() {
    // Three-building reduction in local coordinates, with extended wall
    // bottoms and flat ground. This is an adapted engineering regression,
    // not a published ISO reference case.
    let base: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/sliding-roof-contact.json")).unwrap();
    for offset in [[0.; 3], [600680., 300580., 195.]] {
        let mut request = base.clone();
        for key in ["sources", "receivers"] {
            for item in request[key].as_array_mut().unwrap() {
                for k in 0..3 {
                    item["position"][k] = json!(item["position"][k].as_f64().unwrap() + offset[k]);
                }
            }
        }
        for building in request["buildings"].as_array_mut().unwrap() {
            for point in building["footprint"].as_array_mut().unwrap() {
                for k in 0..3 {
                    point[k] = json!(point[k].as_f64().unwrap() + offset[k]);
                }
            }
        }
        let result = iso_rust::calculate(&request, &AtomicBool::new(false), &|_, _| {}).unwrap();
        // Independent 45-digit decimal line mirrors/intersections give these
        // plan contacts. If f is the corner's projection on S->B1, and a,b
        // are the two mirror interpolation weights from plane distances:
        // h1=a*((1-f)*Sz+f*h1)+(1-a)*h2; h2=b*h1+(1-b)*Rz.
        // Solve those scalar equations, then intersect the resulting lateral
        // plane with the host's first roof edge. No engine output supplies
        // these supplementary expectations.
        let expected = [
            [5.80964653029545, -0.7275004007885892, 21.10320000000001],
            [2.668768583214842, 26.97218561306363, 20.267221333247743],
            [1.85574133796442, 36.0818244803314, 19.993575494595774],
            [10.813235402097547, 37.92839797751583, 19.719929655950462],
        ];
        for band in 5..8 {
            let route = result["path_diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| {
                    p["band"] == band
                        && p["physical_route"].as_array().is_some_and(|p| p.len() == 6)
                        && p["reflection_faces"] == json!(["first/wall:1:0", "second/wall:3:2"])
                })
                .expect("the feasible lateral route in the reported family must survive");
            assert!(route["unit_route_energy"].as_f64().unwrap() > 0.);
            for (point, expected) in route["physical_route"].as_array().unwrap()[1..5]
                .iter()
                .zip(expected)
            {
                for k in 0..3 {
                    assert!((point[k].as_f64().unwrap() - offset[k] - expected[k]).abs() < 1e-8);
                }
            }
        }
    }
}

#[test]
fn sliding_roof_neighbour_converges_for_point_and_area_receivers() {
    // Two-building reduction in local coordinates, with extended wall
    // bottoms, flat ground and explicit source/receiver elevations. This is an
    // adapted engineering regression, not an ISO reference case.
    let base: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/roof-neighbour-contact.json")).unwrap();
    // Independent decimal solution: C is the intersection of mirrored S->P
    // with x=263.7648114950862, while P lies on y=180.7606436291244,z=H.
    // Solve (Pxy-Cxy).(Rxy-Cxy)*(Rz-Cz)=(H-Cz)*|Rxy-Cxy|^2
    // inside the finite roof edge, then back-substitute in the mirror line.
    // A 1 cm update threshold is not an absolute coordinate-error bound.
    // This case has contraction below .7; the remaining geometric tail is
    // below 3 cm. Retain the independently derived exact reference.
    let expected = [
        [263.7648114950862, 137.43505494070233, 23.585255080811525],
        [163.89500432655357, 180.7606436291244, 20.82530743532908],
    ];
    for offset in [[0.; 3], [601000., 300600., 180.]] {
        for area in [false, true] {
            let mut request = base.clone();
            for key in ["sources", "receivers"] {
                for item in request[key].as_array_mut().unwrap() {
                    for k in 0..3 {
                        item["position"][k] =
                            json!(item["position"][k].as_f64().unwrap() + offset[k]);
                    }
                }
            }
            for building in request["buildings"].as_array_mut().unwrap() {
                for point in building["footprint"].as_array_mut().unwrap() {
                    for k in 0..3 {
                        point[k] = json!(point[k].as_f64().unwrap() + offset[k]);
                    }
                }
            }
            if area {
                let p = &request["receivers"][0]["position"];
                let x = p[0].as_f64().unwrap();
                let y = p[1].as_f64().unwrap();
                let h = p[2].as_f64().unwrap() - offset[2];
                request["area_receivers"] = json!([{"id":"grid","boundary":[[x,y],[x+0.01,y],[x+0.01,y+0.01],[x,y+0.01]],"spacing_m":1.,"height_m":h}]);
                request["receivers"] = json!([]);
            }
            let result =
                iso_rust::calculate(&request, &AtomicBool::new(false), &|_, _| {}).unwrap();
            for band in 5..8 {
                let route=result["path_diagnostics"].as_array().unwrap().iter().find(|p| {
                    p["band"]==band && p["reflection_faces"]==json!(["mirror/wall:0:1"])
                        && p["physical_route"].as_array().is_some_and(|ps|ps.len()==4 &&
                            (ps[2][0].as_f64().unwrap()-offset[0]-expected[1][0]).abs()<0.03)
                }).expect("the reflected lateral route with a sliding adjacent roof support must survive");
                for (p, q) in route["physical_route"].as_array().unwrap()[1..3]
                    .iter()
                    .zip(expected)
                {
                    for k in 0..3 {
                        assert!((p[k].as_f64().unwrap() - offset[k] - q[k]).abs() < 0.03);
                    }
                }
                assert!(route["unit_route_energy"].as_f64().unwrap() > 0.);
            }
        }
    }
}

#[test]
fn oscillating_roof_supports_resolve_on_a_physical_branch() {
    // Reductions with flat ground, extended wall bottoms and explicit
    // acoustic positions. Supplementary geometry evidence, not an ISO-issued
    // case.
    // Independent 60-digit Decimal equations place each host roof exit in
    // the S->C lateral plane. Cxy follows the mirror line through the adjacent
    // support, Qxy is C->R at screen y=Y. The four-building case solves a
    // finite-edge scalar residual; the five-building case solves the affine
    // corner-height equation before intersecting the host roof edge.
    // No engine output supplies these supplementary expectations.
    for (fixture, expected) in [
        (
            include_str!("fixtures/oscillating-roof-contact.json"),
            vec![
                [-18.73041734849517, -235.44082022730078, 36.103200000000015],
                [263.7648114950862, 94.00015097057343, 24.01010518546991],
                [189.36779542558838, 180.7606436291244, 20.82530743532908],
            ],
        ),
        (
            include_str!("fixtures/three-state-contact.json"),
            vec![
                [-80.22461185876425, -190.49721804446355, 36.103200000000015],
                [-54.83173039078247, -188.2467900812626, 35.53612348420096],
                [263.7648114950862, 112.70491141879715, 23.53841092907376],
                [191.71896217890722, 180.7606436291244, 20.82530743532908],
            ],
        ),
        (
            include_str!("fixtures/finite-roof-contact-cycle.json"),
            vec![
                [-56.803628925361274, -169.18588696326356, 36.103200000000015],
                [-53.04468661977444, -164.1292305754614, 35.83671126475671],
                [263.7648114950862, 122.48280531652537, 17.48088615660068],
            ],
        ),
    ] {
        let base: serde_json::Value = serde_json::from_str(fixture).unwrap();
        for offset in [[0.; 3], [601000., 300600., 180.]] {
            for area in [false, true] {
                let mut request = base.clone();
                for key in ["sources", "receivers"] {
                    for item in request[key].as_array_mut().unwrap() {
                        for k in 0..3 {
                            item["position"][k] =
                                json!(item["position"][k].as_f64().unwrap() + offset[k]);
                        }
                    }
                }
                for building in request["buildings"].as_array_mut().unwrap() {
                    for point in building["footprint"].as_array_mut().unwrap() {
                        for k in 0..3 {
                            point[k] = json!(point[k].as_f64().unwrap() + offset[k]);
                        }
                    }
                }
                if area {
                    let p = &request["receivers"][0]["position"];
                    let x = p[0].as_f64().unwrap();
                    let y = p[1].as_f64().unwrap();
                    let h = p[2].as_f64().unwrap() - offset[2];
                    request["area_receivers"] = json!([{"id":"grid","boundary":[[x,y],[x+0.01,y],[x+0.01,y+0.01],[x,y+0.01]],"spacing_m":1.,"height_m":h}]);
                    request["receivers"] = json!([]);
                }
                let result =
                    iso_rust::calculate(&request, &AtomicBool::new(false), &|_, _| {}).unwrap();
                for band in 5..8 {
                    let route = result["path_diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|p| {
                        p["band"] == band
                            && p["reflection_faces"] == json!(["mirror/wall:0:1"])
                            && p["physical_route"].as_array().is_some_and(|ps| {
                                ps.len() == expected.len() + 2
                                    && (ps[1][0].as_f64().unwrap() - offset[0] - expected[0][0])
                                        .abs()
                                        < 0.001
                            })
                    })
                    .expect(
                        "the valid roof-support branch must survive the local shortest-chain cycle",
                    );
                    for (point, expected) in route["physical_route"].as_array().unwrap()
                        [1..1 + expected.len()]
                        .iter()
                        .zip(&expected)
                    {
                        for k in 0..3 {
                            assert!(
                                (point[k].as_f64().unwrap() - offset[k] - expected[k]).abs()
                                    < 0.001
                            );
                        }
                    }
                    assert!(route["unit_route_energy"].as_f64().unwrap() > 0.);
                }
            }
        }
    }
}

#[test]
fn contact_cycle_height_trials_keep_the_valid_alternative() {
    // Four-building reduction with original finite wall bottoms and terrain.
    // The preceding release fails at large projected coordinates. This is supplementary
    // geometry/execution evidence, not an ISO acoustic acceptance case.
    let base: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/cycle-height-contact.json")).unwrap();
    for offset in [[0.; 3], [-601000., -300600., -175.]] {
        let mut request = base.clone();
        for key in ["sources", "receivers"] {
            for item in request[key].as_array_mut().unwrap() {
                for k in 0..3 {
                    item["position"][k] = json!(item["position"][k].as_f64().unwrap() + offset[k]);
                }
            }
        }
        for b in request["buildings"].as_array_mut().unwrap() {
            for v in b["ground_vertices"].as_array_mut().unwrap() {
                for k in 0..3 {
                    v["position"][k] = json!(v["position"][k].as_f64().unwrap() + offset[k]);
                }
            }
        }
        for line in request["terrain"]["breakline_definitions"]
            .as_array_mut()
            .unwrap()
        {
            for v in line["vertices"].as_array_mut().unwrap() {
                for k in 0..3 {
                    v["position"][k] = json!(v["position"][k].as_f64().unwrap() + offset[k]);
                }
            }
        }
        request["ground"]["elevation_m"] = json!(175. + offset[2]);
        let receiver = request["receivers"][0]["position"].clone();
        for area in [false, true] {
            let mut input = request.clone();
            if area {
                let x = receiver[0].as_f64().unwrap();
                let y = receiver[1].as_f64().unwrap();
                input["receivers"] = json!([]);
                input["area_receivers"] = json!([{
                    "id":"area", "boundary":[[x,y],[x+0.01,y],[x+0.01,y+0.01],[x,y+0.01]],
                    "height_m":1.6764000000000001,"spacing_m":1.
                }]);
            }
            let result = iso_rust::calculate(&input, &AtomicBool::new(false), &|_, _| {}).unwrap();
            // Independent 60-digit source-image plan, finite roof intersection,
            // and roof-to-receiver mirror-height interpolation supply these.
            let expected = [
                [600700.5047671543, 300609.25220668747, 216.1032],
                [601603.868148974, 300918.0526199907, 191.5412861364525],
            ];
            for band in 4..8 {
                let route = result["path_diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| {
                        v["band"] == band
                            && v["reflection_faces"] == json!(["mirror/wall:3:0"])
                            && v["physical_route"].as_array().is_some_and(|p| p.len() == 4)
                    })
                    .expect("the independently located valid alternative must survive");
                assert!(route["unit_route_energy"].as_f64().unwrap() > 0.);
                for (actual, expected) in route["physical_route"].as_array().unwrap()[1..3]
                    .iter()
                    .zip(expected)
                {
                    for k in 0..3 {
                        assert!(
                            (actual[k].as_f64().unwrap() - offset[k] - expected[k]).abs() < 0.001
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn bounded_roof_corner_keeps_the_sliding_route() {
    // Two-building reduction with original finite wall bottoms and terrain.
    // The preceding release fails on this reduction. This is supplementary
    // geometry/execution evidence, not an ISO acoustic acceptance case.
    let base: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/roof-corner-contact.json")).unwrap();
    for offset in [[0.; 3], [-601000., -300600., -180.]] {
        let mut request = base.clone();
        for key in ["sources", "receivers"] {
            for item in request[key].as_array_mut().unwrap() {
                for k in 0..3 {
                    item["position"][k] = json!(item["position"][k].as_f64().unwrap() + offset[k]);
                }
            }
        }
        for b in request["buildings"].as_array_mut().unwrap() {
            for v in b["ground_vertices"].as_array_mut().unwrap() {
                for k in 0..3 {
                    v["position"][k] = json!(v["position"][k].as_f64().unwrap() + offset[k]);
                }
            }
        }
        for line in request["terrain"]["breakline_definitions"]
            .as_array_mut()
            .unwrap()
        {
            for v in line["vertices"].as_array_mut().unwrap() {
                for k in 0..3 {
                    v["position"][k] = json!(v["position"][k].as_f64().unwrap() + offset[k]);
                }
            }
        }
        request["ground"]["elevation_m"] = json!(180. + offset[2]);
        let receiver = request["receivers"][0]["position"].clone();
        for area in [false, true] {
            let mut input = request.clone();
            if area {
                let x = receiver[0].as_f64().unwrap();
                let y = receiver[1].as_f64().unwrap();
                input["receivers"] = json!([]);
                input["area_receivers"] = json!([{
                    "id":"area", "boundary":[[x,y],[x+0.01,y],[x+0.01,y+0.01],[x,y+0.01]],
                    "height_m":1.6764000000000001,"spacing_m":1.
                }]);
            }
            let result = iso_rust::calculate(&input, &AtomicBool::new(false), &|_, _| {}).unwrap();
            // Independent 60-digit incoming roof-plane, mirror-plan and
            // outgoing fixed-corner affine-height equations supply these.
            let expected = [
                [601192.718738375, 300671.466321422, 200.82530743532906],
                [601263.7648114951, 300726.11348252563, 198.57210617865197],
                [601192.7187383752, 300780.7606436291, 196.3189049219817],
            ];
            for band in 6..8 {
                let route = result["path_diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| {
                        v["band"] == band
                            && v["reflection_faces"] == json!(["mirror/wall:0:1"])
                            && v["physical_route"].as_array().is_some_and(|p| {
                                p.len() == 5
                                    && (p[1][1].as_f64().unwrap() - offset[1] - expected[0][1])
                                        .abs()
                                        < 0.001
                            })
                    })
                    .expect("the independently located valid alternative must survive");
                assert!(route["unit_route_energy"].as_f64().unwrap() > 0.);
                for (actual, expected) in route["physical_route"].as_array().unwrap()[1..4]
                    .iter()
                    .zip(expected)
                {
                    for k in 0..3 {
                        assert!(
                            (actual[k].as_f64().unwrap() - offset[k] - expected[k]).abs() < 0.001
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn bounded_empty_roof_corner_keeps_the_vertical_alternative() {
    // Two-building reduction with original finite wall bottoms and terrain.
    // The preceding release fails on this reduction. This is supplementary
    // geometry/execution evidence, not an ISO acoustic acceptance case.
    let mut base: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/roof-corner-contact.json")).unwrap();
    base["sources"][0]["position"] =
        json!([600718.1069781118, 300600.60565214214, 213.05519390398783]);
    base["receivers"][0]["position"] =
        json!([600866.5155394394, 300852.55321920995, 188.20280633860233]);
    base["sources"][0]["spectrum"] = json!([null, null, null, null, null, 100, null, null]);
    for offset in [[0.; 3], [-601000., -300600., -180.]] {
        let mut request = base.clone();
        for key in ["sources", "receivers"] {
            for item in request[key].as_array_mut().unwrap() {
                for k in 0..3 {
                    item["position"][k] = json!(item["position"][k].as_f64().unwrap() + offset[k]);
                }
            }
        }
        for b in request["buildings"].as_array_mut().unwrap() {
            for v in b["ground_vertices"].as_array_mut().unwrap() {
                for k in 0..3 {
                    v["position"][k] = json!(v["position"][k].as_f64().unwrap() + offset[k]);
                }
            }
        }
        for line in request["terrain"]["breakline_definitions"]
            .as_array_mut()
            .unwrap()
        {
            for v in line["vertices"].as_array_mut().unwrap() {
                for k in 0..3 {
                    v["position"][k] = json!(v["position"][k].as_f64().unwrap() + offset[k]);
                }
            }
        }
        request["ground"]["elevation_m"] = json!(180. + offset[2]);
        let receiver = request["receivers"][0]["position"].clone();
        for area in [false, true] {
            let mut input = request.clone();
            if area {
                let x = receiver[0].as_f64().unwrap();
                let y = receiver[1].as_f64().unwrap();
                input["receivers"] = json!([]);
                input["area_receivers"] = json!([{
                    "id":"area", "boundary":[[x,y],[x+0.01,y],[x+0.01,y+0.01],[x,y+0.01]],
                    "height_m":1.6764000000000001,"spacing_m":1.
                }]);
            }
            let result = iso_rust::calculate(&input, &AtomicBool::new(false), &|_, _| {}).unwrap();
            // Independent unfolded source-image plan / finite north-roof-edge
            // intersection and mirror-height interpolation. The other lateral
            // roof/corner branch is excluded by exact-rational residual bounds.
            let expected = [
                [601263.7648114951, 300746.4070389165, 203.1574142615479],
                [601135.1973545519, 300780.7606436291, 200.82530743532906],
            ];
            for band in 5..6 {
                let route = result["path_diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| {
                        v["band"] == band
                            && v["reflection_faces"] == json!(["mirror/wall:0:1"])
                            && v["physical_route"].as_array().is_some_and(|p| {
                                p.len() == 4
                                    && (p[1][1].as_f64().unwrap() - offset[1] - expected[0][1])
                                        .abs()
                                        < 0.001
                            })
                    })
                    .expect("the independently located valid alternative must survive");
                assert!(route["unit_route_energy"].as_f64().unwrap() > 0.);
                for (actual, expected) in route["physical_route"].as_array().unwrap()[1..3]
                    .iter()
                    .zip(expected)
                {
                    for k in 0..3 {
                        assert!(
                            (actual[k].as_f64().unwrap() - offset[k] - expected[k]).abs() < 0.001
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn bounded_near_turning_roof_corner_keeps_the_vertical_alternative() {
    // Two-building reduction with original finite wall bottoms and terrain.
    // The preceding release fails on this reduction. This is supplementary
    // geometry/execution evidence, not an ISO acoustic acceptance case.
    let mut base: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/roof-corner-contact.json")).unwrap();
    base["sources"][0]["position"] =
        json!([600687.5439853733, 300623.32511685917, 213.05519390398783]);
    base["receivers"][0]["position"] =
        json!([600836.0354784793, 300852.55321920995, 188.50414026033107]);
    base["sources"][0]["spectrum"] = json!([null, null, null, null, null, 100, null, null]);
    for offset in [[0.; 3], [-601000., -300600., -180.]] {
        let mut request = base.clone();
        for key in ["sources", "receivers"] {
            for item in request[key].as_array_mut().unwrap() {
                for k in 0..3 {
                    item["position"][k] = json!(item["position"][k].as_f64().unwrap() + offset[k]);
                }
            }
        }
        for b in request["buildings"].as_array_mut().unwrap() {
            for v in b["ground_vertices"].as_array_mut().unwrap() {
                for k in 0..3 {
                    v["position"][k] = json!(v["position"][k].as_f64().unwrap() + offset[k]);
                }
            }
        }
        for line in request["terrain"]["breakline_definitions"]
            .as_array_mut()
            .unwrap()
        {
            for v in line["vertices"].as_array_mut().unwrap() {
                for k in 0..3 {
                    v["position"][k] = json!(v["position"][k].as_f64().unwrap() + offset[k]);
                }
            }
        }
        request["ground"]["elevation_m"] = json!(180. + offset[2]);
        let receiver = request["receivers"][0]["position"].clone();
        for area in [false, true] {
            let mut input = request.clone();
            if area {
                let x = receiver[0].as_f64().unwrap();
                let y = receiver[1].as_f64().unwrap();
                input["receivers"] = json!([]);
                input["area_receivers"] = json!([{
                    "id":"area", "boundary":[[x,y],[x+0.01,y],[x+0.01,y+0.01],[x,y+0.01]],
                    "height_m":1.6764000000000001,"spacing_m":1.
                }]);
            }
            let result = iso_rust::calculate(&input, &AtomicBool::new(false), &|_, _| {}).unwrap();
            // Independent unfolded source-image plan / finite north-roof-edge
            // intersection and mirror-height interpolation. The other lateral
            // roof/corner branch is excluded by exact-rational residual bounds.
            let expected = [
                [601263.7648114951, 300754.8914155491, 202.83487750468575],
                [601150.4653714119, 300780.7606436291, 200.82530743532906],
            ];
            for band in 5..6 {
                let route = result["path_diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| {
                        v["band"] == band
                            && v["reflection_faces"] == json!(["mirror/wall:0:1"])
                            && v["physical_route"].as_array().is_some_and(|p| {
                                p.len() == 4
                                    && (p[1][1].as_f64().unwrap() - offset[1] - expected[0][1])
                                        .abs()
                                        < 0.001
                            })
                    })
                    .expect("the independently located valid alternative must survive");
                assert!(route["unit_route_energy"].as_f64().unwrap() > 0.);
                for (actual, expected) in route["physical_route"].as_array().unwrap()[1..3]
                    .iter()
                    .zip(expected)
                {
                    for k in 0..3 {
                        assert!(
                            (actual[k].as_f64().unwrap() - offset[k] - expected[k]).abs() < 0.001
                        );
                    }
                }
            }
        }
    }
}

// Translate authored XYZ, retaining relative heights and acoustic parameters.
fn translated_contact_fixture(text: &str, offset: [f64; 3]) -> serde_json::Value {
    fn positions(value: &mut serde_json::Value, offset: [f64; 3]) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(p) = map.get_mut("position") {
                    for k in 0..3 {
                        p[k] = json!(p[k].as_f64().unwrap() + offset[k]);
                    }
                }
                for (key, v) in map {
                    if key != "position" {
                        positions(v, offset);
                    }
                }
            }
            serde_json::Value::Array(values) => {
                for v in values {
                    positions(v, offset);
                }
            }
            _ => {}
        }
    }
    let mut value: serde_json::Value = serde_json::from_str(text).unwrap();
    positions(&mut value, offset);
    for p in value["terrain"]["samples"].as_array_mut().unwrap() {
        for k in 0..3 {
            p[k] = json!(p[k].as_f64().unwrap() + offset[k]);
        }
    }
    value["ground"]["elevation_m"] =
        json!(value["ground"]["elevation_m"].as_f64().unwrap() + offset[2]);
    value
}

fn check_fixed_support_fixture(
    fixture: &str,
    face: &str,
    expected: &[[f64; 3]],
    bands: std::ops::Range<usize>,
    include_area: bool,
) {
    for offset in [[0.; 3], [-601000., -300600., -180.]] {
        let request = translated_contact_fixture(fixture, offset);
        for area in [false, true]
            .into_iter()
            .take(if include_area { 2 } else { 1 })
        {
            let mut input = request.clone();
            if area {
                let x = input["receivers"][0]["position"][0].as_f64().unwrap();
                let y = input["receivers"][0]["position"][1].as_f64().unwrap();
                input["receivers"] = json!([]);
                input["area_receivers"] = json!([{
                    "id":"area", "boundary":[[x,y],[x+0.01,y],[x+0.01,y+0.01],[x,y+0.01]],
                    "height_m":1.6764000000000001,"spacing_m":1.
                }]);
            }
            let result = iso_rust::calculate(&input, &AtomicBool::new(false), &|_, _| {}).unwrap();
            for band in bands.clone() {
                let route = result["path_diagnostics"].as_array().unwrap().iter().find(|v| {
                    v["band"] == band && v["reflection_faces"] == json!([face])
                        && v["physical_route"].as_array().is_some_and(|p| {
                            p.len() == expected.len() + 2
                                && p[1..p.len()-1].iter().zip(expected).all(|(p, e)| {
                                    (0..3).all(|k| (p[k].as_f64().unwrap()-offset[k]-e[k]).abs() < 0.001)
                                })
                        })
                }).unwrap_or_else(|| panic!("independently located route missing: band {band}, area {area}, offset {offset:?}"));
                assert!(route["unit_route_energy"].as_f64().unwrap() > 0.);
            }
        }
    }
}

#[test]
fn empty_long_roof_chain_retains_independent_vertical_alternative() {
    // Original three-footprint reduction. Exact-rational Bernstein bounds
    // exclude the constrained lateral branch; unfolded geometry supplies this
    // positive alternative. Neither is an ISO acoustic reference.
    check_fixed_support_fixture(
        include_str!("fixtures/roof-chain-corner-contact.json"),
        "mirror/wall:0:1",
        &[
            [601263.7648114951, 300743.2066879931, 203.6779050830636],
            [601131.758616455, 300769.414068062, 200.82530743532906],
        ],
        5..8,
        true,
    );
}

#[test]
fn longer_roof_chain_reaches_independent_joint_contact() {
    // Construct a remote flat roof through a second intersection of the
    // independently solved lateral plane: E=S+.15*(C-S)+.02*(Dy,-Dx,0).
    check_fixed_support_fixture(
        include_str!("fixtures/roof-chain-corner-valid.json"),
        "mirror/wall:0:1",
        &[
            [600828.898742849, 300589.81838487723, 210.88273074518745],
            [601192.718738375, 300671.466321422, 200.82530743532906],
            [601263.7648114951, 300726.11348252563, 198.57210617865197],
            [601192.7187383752, 300780.7606436291, 196.3189049219817],
        ],
        6..8,
        false,
    );
}

#[test]
fn fixed_support_height_solve_retains_controlling_terrain_anchor() {
    // Adapted three-generator case with the controlling ground ridge authored
    // explicitly. Unfolded plan geometry and affine corner/mirror equations
    // give these contacts independently of the production terrain triangulation.
    check_fixed_support_fixture(
        include_str!("fixtures/fixed-support-height-contact.json"),
        "mirror/wall:1:0",
        &[
            [600843.9519099166, 300467.08706518204, 212.94408001261175],
            [600807.541915685, 300529.823772226, 212.07364724058586],
            [600826.8084221098, 300525.24062891095, 211.836],
        ],
        6..8,
        true,
    );
}

fn coplanar_receiver_request(order: u32) -> serde_json::Value {
    let absorption = [0.1; 8];
    json!({
        "sources":[{"id":"s","kind":"point","spectrum":[90,92,94,95,95,94,90,85],
            "position":[103.62648474661196,42.237768698860535,5]}],
        "receivers":[{"id":"in_plane","position":[160,40,1.5]},{"id":"open_side","position":[160,40.0-1e-6,1.5]}],
        "buildings":[
            {"id":"b6","footprint":[[130,40,0],[142,40,0],[142,46,0],[130,46,0]],"height_m":12,"absorption":absorption},
            {"id":"b8","footprint":[[175,40,0],[195,40,0],[195,43.5,0],[175,43.5,0]],"height_m":6,"absorption":absorption},
            {"id":"b11","footprint":[[220,40,0],[228,40,0],[228,43.5,0],[220,43.5,0]],"height_m":12,"absorption":absorption}],
        "ground":{"factor":0.3},
        "settings":{"max_reflect_order":order,"minimum_contribution_db":null,"ground_method":"general"}
    })
}
fn coplanar_levels(order: u32) -> Vec<f64> {
    let result = iso_rust::calculate(
        &coplanar_receiver_request(order),
        &AtomicBool::new(false),
        &|_, _| {},
    )
    .unwrap_or_else(|e| panic!("{e:?}"));
    result["receivers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["a_weighted_db"].as_f64().unwrap())
        .collect()
}

#[test]
fn receiver_in_wall_and_roof_edge_planes_resolves_endpoint_contacts() {
    // Supplementary degenerate-geometry regression. The receiver lies in the
    // y=40 plane shared by two building walls and a roof edge. The reflected
    // route's vertical section then contains the whole roof edge, whose
    // interior is a contact continuum: the interval search previously spent
    // its box budget (about 100 s) and failed the job.
    let reflected = coplanar_levels(1);
    assert!(reflected.iter().all(|l| l.is_finite()), "{reflected:?}");
}

#[test]
#[ignore = "known: ray_blocked reports a leg grazing a wall as blocked at the corner it touches"]
fn receiver_in_wall_plane_takes_the_open_side_limit() {
    let direct = coplanar_levels(0);
    assert!((direct[0] - direct[1]).abs() < 1e-4, "{direct:?}");
}

#[test]
fn parallel_gap_roof_edge_contact_search_resolves() {
    // Reduction: a roof standoff source 0.15 m above its roof edge
    // reflects between two parallel module walls 6.09 m apart, then from a
    // distant wall, to one area-grid cell (receiver at 1.6764 m). Only those
    // three walls reflect; all buildings and terrain still screen. Corner
    // heights eliminated from the joint contact equations were enclosed
    // ~1000x too wide over small boxes, so the cover spent its 20,000-box
    // budget (about 70 s) and left this family unresolved. Supplementary
    // numerical regression only: it requires a complete contact search and
    // claims no reference level.
    let request: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/parallel-gap-roof-edge-contact.json")).unwrap();
    let result = iso_rust::calculate(&request, &AtomicBool::new(false), &|_, _| {}).unwrap();
    let receiver = &result["receivers"][0];
    assert!(receiver["a_weighted_db"].as_f64().unwrap().is_finite());
    assert!(receiver.get("warnings").is_none(), "{receiver}");
    assert!(
        receiver["omitted_energy"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e.as_f64() == Some(0.)),
        "{receiver}"
    );
    assert!(
        result["diagnostics"]["integration"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r.get("unresolved_contact_allowance").is_none())
    );
}
