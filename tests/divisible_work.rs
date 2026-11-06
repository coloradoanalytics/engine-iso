//! Divisible work through the engine protocol, as an acalc-engine farm uses
//! it: items run on separate engine instances, in any order, and merged in a
//! session give the local engine's result bit for bit. acalc-engine's own
//! tests cover node selection, failures and silence.
mod common;
use common::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Point and line sources with second-order reflections, explicit
/// receivers, a generated grid and path diagnostics.
fn request() -> Value {
    json!({
        "sources":[
            {"id":"p","kind":"point","position":[-12,-6,2],"spectrum":[80,82,84,86,88,86,84,82]},
            {"id":"l","kind":"line","vertices":[[-20,30,1],[-17,31,1]],"power_basis":"power_per_m","spectrum":[70,72,74,76,78,76,74,72]}
        ],
        "receivers":[{"id":"r1","position":[-10,15,2]},{"id":"r2","position":[22,18,4]},{"id":"r3","position":[5,-15,1.5]}],
        "area_receivers":[{"id":"grid","boundary":[[-30,-30],[40,-30],[40,45],[-30,45]],"spacing_m":15,"height_m":1.5}],
        "buildings":[
            {"id":"b1","footprint":[[0,0,0],[10,0,0],[10,10,0],[0,10,0]],"height_m":10},
            {"id":"b2","footprint":[[15,5,0],[20,5,0],[20,25,0],[15,25,0]],"height_m":8}
        ],
        "ground":{"factor":0.5},
        "settings":{"max_reflect_order":2,"minimum_contribution_db":null},
        "output":{"path_diagnostics":"summary"}
    })
}
fn key(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Run `request` as a farm would: a merge session on `coordinator`, items
/// spread over `nodes` in the order given.
fn farm(coordinator: &Engine, nodes: &[&Engine], request: &Value, order: &[usize]) -> Calculation {
    let bytes = request.to_string().into_bytes();
    let key = key(&bytes);
    let session = coordinator.call(
        json!({"op": "merge", "key": key, "artifact_base": BASE}),
        &bytes,
    );
    let accepted = session.next();
    assert_eq!(accepted.header["event"], "accepted", "{}", accepted.header);
    let planned = loop {
        let frame = session.next();
        if frame.header["event"] == "planned" {
            break frame;
        }
    };
    let items = planned.header["items"].as_u64().unwrap() as usize;
    assert_eq!(order.len(), items);
    for node in nodes {
        let plan = node.request(json!({"op": "plan", "key": key}), &bytes);
        assert_eq!(plan.header["ok"]["items"], items, "{}", plan.header);
    }
    // Four items in flight per node, as farm slots keep them; each slot
    // takes the next item of `order`.
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for node in nodes {
            for _ in 0..4 {
                scope.spawn(|| {
                    loop {
                        let n = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(&item) = order.get(n) else { break };
                        let input = coordinator.request(
                            json!({"op": "item_input", "session": session.id, "item": item}),
                            b"",
                        );
                        let output = node.request(
                            json!({"op": "run_item", "key": key, "item": item}),
                            &input.payload,
                        );
                        assert!(output.header["ok"]["seconds"].is_f64(), "{}", output.header);
                        let merged = coordinator.request(
                            json!({"op": "item_output", "session": session.id, "item": item}),
                            &output.payload,
                        );
                        assert!(merged.header["ok"].is_null(), "{}", merged.header);
                    }
                });
            }
        }
    });
    let finished = coordinator.request(json!({"op": "finish", "session": session.id}), b"");
    assert!(finished.header["ok"].is_null());
    follow(session, accepted.header["progress"].clone())
}

#[test]
fn items_run_anywhere_in_any_order_merge_to_the_local_result() {
    let request = request();
    let local_engine = Engine::start(2);
    let local = calculate(&local_engine, &request);
    assert!(local.result["path_diagnostics"].as_array().unwrap().len() > 20);
    let (coordinator, a, b) = (Engine::start(2), Engine::start(1), Engine::start(3));
    // Learn the item count, then run items backwards and interleaved.
    let bytes = request.to_string().into_bytes();
    let items = coordinator
        .request(json!({"op": "plan", "key": key(&bytes)}), &bytes)
        .header["ok"]["items"]
        .as_u64()
        .unwrap() as usize;
    assert!(items > 1);
    let order: Vec<usize> = (0..items).rev().collect();
    let farmed = farm(&coordinator, &[&a, &b], &request, &order);
    assert_eq!(
        serde_json::to_string(&farmed.result).unwrap(),
        serde_json::to_string(&local.result).unwrap()
    );
    assert_eq!(farmed.artifacts, local.artifacts);
    assert_eq!(
        farmed.final_progress()["path_counts"],
        local.final_progress()["path_counts"]
    );
    // The coordinator keeps the session's plan, so its own engine can run
    // items without a separate `plan`.
    let order: Vec<usize> = (0..items).collect();
    let again = farm(&coordinator, &[&coordinator], &request, &order);
    assert_eq!(again.result, local.result);
}

/// A pruning strategy through the protocol: every item's input is taken
/// from the merge session as bytes and passed unchanged to `run_item` on
/// another engine. With one item at a time on one-worker engines, each item
/// starts from the same completed contributions as in the one-worker local
/// run, so the results are identical. (Items in flight together would differ:
/// thresholds are live.)
#[test]
fn strategy_item_inputs_carry_receiver_thresholds_through_the_protocol() {
    for strategy in ["dba", "dbc"] {
        let mut request = request();
        request["settings"]["pruning_strategy"] = json!(strategy);
        let local = calculate(&Engine::start(1), &request);
        let (coordinator, node) = (Engine::start(1), Engine::start(1));
        let bytes = request.to_string().into_bytes();
        let key = key(&bytes);
        let session = coordinator.call(
            json!({"op": "merge", "key": key, "artifact_base": BASE}),
            &bytes,
        );
        let accepted = session.next();
        assert_eq!(accepted.header["event"], "accepted", "{}", accepted.header);
        let planned = loop {
            let frame = session.next();
            if frame.header["event"] == "planned" {
                break frame;
            }
        };
        let items = planned.header["items"].as_u64().unwrap() as usize;
        let plan = node.request(json!({"op": "plan", "key": key}), &bytes);
        assert_eq!(plan.header["ok"]["items"], items, "{}", plan.header);
        // Four stages of equally many items: the leading and the remaining
        // sources' direct paths, then reflection orders 1 and 2.
        let per_stage = items / 4;
        let direct = 2 * per_stage;
        let run = |payload: &[u8], item: usize| {
            node.request(json!({"op": "run_item", "key": key, "item": item}), payload)
        };
        let mut with_thresholds = 0;
        let mut last_list = None;
        for item in 0..items {
            let input = coordinator.request(
                json!({"op": "item_input", "session": session.id, "item": item}),
                b"",
            );
            let fields = input.json();
            assert_eq!(
                fields["cutoffs"],
                json!(vec![Value::Null; 8]),
                "{strategy} item {item}"
            );
            // Direct items carry one action per receiver, nothing else.
            match fields.get("receiver_actions") {
                Some(list) => {
                    assert!(item < direct, "{strategy} item {item}");
                    let list = list.as_array().unwrap();
                    let limit = if item < per_stage { 1 } else { 2 };
                    assert!(list.iter().all(|a| a.as_u64().unwrap() <= limit));
                }
                None => assert!(item >= direct, "{strategy} item {item}"),
            }
            match fields.get("receiver_thresholds") {
                None => assert!(item < direct, "{strategy} item {item}"),
                Some(list) => {
                    assert!(item >= direct, "{strategy} item {item}");
                    let list = list.as_array().unwrap();
                    // Bit patterns of finite thresholds, or null.
                    assert!(list.iter().any(|t| t.is_u64()), "{strategy} item {item}");
                    assert!(list.iter().all(|t| t.is_u64() || t.is_null()));
                    with_thresholds += 1;
                    last_list = Some((item, fields.clone()));
                }
            }
            let output = run(&input.payload, item);
            assert!(output.header["ok"]["seconds"].is_f64(), "{}", output.header);
            let merged = coordinator.request(
                json!({"op": "item_output", "session": session.id, "item": item}),
                &output.payload,
            );
            assert!(merged.header["ok"].is_null(), "{}", merged.header);
        }
        assert_eq!(with_thresholds, items - direct, "{strategy}");
        // Inputs that do not fit the item are refused by the node; they are
        // not merged, so they leave the calculation unchanged.
        let (item, fields) = last_list.unwrap();
        let list = fields["receiver_thresholds"].as_array().unwrap().clone();
        let refused = |fields: &Value, item: usize| {
            let output = run(fields.to_string().as_bytes(), item);
            assert_eq!(
                output.header["error"]["code"], "invalid_input",
                "{strategy}: {fields}"
            );
        };
        let mut short = fields.clone();
        short["receiver_thresholds"] = json!(list[1..]);
        refused(&short, item);
        let mut infinite = fields.clone();
        infinite["receiver_thresholds"][0] = json!(f64::INFINITY.to_bits());
        refused(&infinite, item);
        let mut both = fields.clone();
        both["cutoffs"] = json!(vec![30f64.to_bits(); 8]);
        refused(&both, item);
        // A direct item takes no thresholds, and needs its actions.
        refused(&fields, 0);
        refused(&json!({"cutoffs": vec![Value::Null; 8]}), 0);
        refused(&json!({"cutoffs": vec![Value::Null; 8]}), per_stage);
        // The form of earlier builds, without a list, still runs.
        let legacy = run(
            br#"{"cutoffs":[null,null,null,null,null,null,null,null]}"#,
            item,
        );
        assert!(legacy.header["ok"]["seconds"].is_f64(), "{}", legacy.header);
        let finished = coordinator.request(json!({"op": "finish", "session": session.id}), b"");
        assert!(finished.header["ok"].is_null());
        let farmed = follow(session, accepted.header["progress"].clone());
        assert_eq!(
            serde_json::to_string(&farmed.result).unwrap(),
            serde_json::to_string(&local.result).unwrap(),
            "{strategy}"
        );
        assert_eq!(farmed.artifacts, local.artifacts, "{strategy}");
        let stages = farmed.result["diagnostics"]["pruning"]["stages"]
            .as_array()
            .unwrap();
        assert!(stages[0]["weighted_cutoff_db"].is_null());
        assert!(stages[1]["weighted_cutoff_db"].is_array(), "{strategy}");
    }
}

#[test]
fn item_errors_unknown_plans_and_cancellation_are_reported() {
    let engine = Engine::start(1);
    let missing = engine.request(
        json!({"op": "run_item", "key": "none", "item": 0}),
        br#"{"cutoffs":[null,null,null,null,null,null,null,null]}"#,
    );
    assert_eq!(missing.header["error"]["code"], "unknown_plan");
    // An area source's path diagnostics exceed the record limit: the item's
    // error is the calculation's.
    let mut request = request();
    request["sources"] = json!([{"id":"a","kind":"area","triangles":[[[25,-12,1],[27,-12,1],[25,-10,1]]],
        "power_basis":"total_power","spectrum":[85,85,85,85,85,85,85,85]}]);
    request["settings"]["max_reflect_order"] = json!(1);
    let bytes = request.to_string().into_bytes();
    let key = key(&bytes);
    let session = engine.call(
        json!({"op": "merge", "key": key, "artifact_base": BASE}),
        &bytes,
    );
    assert_eq!(session.next().header["event"], "accepted");
    let planned = session.next();
    assert_eq!(planned.header["event"], "planned");
    let items = planned.header["items"].as_u64().unwrap();
    let outside = engine.request(
        json!({"op": "item_input", "session": session.id, "item": items}),
        b"",
    );
    assert_eq!(outside.header["error"]["code"], "invalid_input");
    let mut errors = vec![];
    for item in 0..items {
        let input = engine.request(
            json!({"op": "item_input", "session": session.id, "item": item}),
            b"",
        );
        let output = engine.request(
            json!({"op": "run_item", "key": key, "item": item}),
            &input.payload,
        );
        if let Some(error) = output.header.get("error") {
            // A coordinator fails the calculation at the first item error.
            errors.push(error.clone());
            break;
        }
    }
    assert!(
        errors.iter().any(|e| e["message"]
            .as_str()
            .unwrap()
            .contains("exceed 100000 records")),
        "{errors:?}"
    );
    engine.cancel(session.id);
    let (_, last) = session.finish();
    assert_eq!(last.header["error"]["code"], "cancelled");
    let unknown = engine.request(json!({"op": "finish", "session": 999}), b"");
    assert_eq!(unknown.header["error"]["code"], "unknown_session");
}
