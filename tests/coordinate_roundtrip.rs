use serde_json::{Value, json};
#[test]
fn surveyed_receiver_coordinates_survive_json_roundtrip() {
    // Exact binary64 expectations from correctly rounded Rust decimal literals.
    // This is transport fidelity, not an acoustic reference case.
    let expected = [
        600949.4591373267_f64,
        300044.18026459473,
        200.55258156292842,
        1.6764000000000001,
    ];
    let wire = "[600949.4591373267,300044.18026459473,200.55258156292842,1.6764000000000001]";
    let values: Value = serde_json::from_str(wire).unwrap();
    for (i, expected) in expected.into_iter().enumerate() {
        assert_eq!(
            values[i].as_f64().unwrap().to_bits(),
            expected.to_bits(),
            "coordinate {i}: {} vs {expected}",
            values[i]
        );
        let decoded: f64 = serde_json::from_str(&json!(expected).to_string()).unwrap();
        assert_eq!(decoded.to_bits(), expected.to_bits());
    }
}
