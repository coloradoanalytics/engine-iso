//! Decoding and checking submitted calculation requests.
use crate::{types::Diagnostic, validation};
use serde::de::{Deserialize, MapAccess, SeqAccess, Visitor};
use serde_json::{Value, json};

// serde_json::Value normally overwrites duplicate object keys. Acoustic
// requests must not silently lose a supplied setting or geometry field.
struct StrictValue(Value);
impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StrictVisitor;
        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = StrictValue;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON value with unique object keys")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Bool(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(StrictValue(json!(v)))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(StrictValue(json!(v)))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| StrictValue(Value::Number(n)))
                    .ok_or_else(|| E::custom("JSON numbers must be finite"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::String(v.into())))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::String(v)))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut result = Vec::new();
                while let Some(StrictValue(v)) = seq.next_element()? {
                    result.push(v);
                }
                Ok(StrictValue(Value::Array(result)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut result = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if result.contains_key(&key) {
                        return Err(serde::de::Error::custom(format!(
                            "Duplicate JSON field {key:?}"
                        )));
                    }
                    let StrictValue(v) = map.next_value()?;
                    result.insert(key, v);
                }
                Ok(StrictValue(Value::Object(result)))
            }
        }
        deserializer.deserialize_any(StrictVisitor)
    }
}

/// Decode request bytes, refusing duplicate keys, and validate them.
pub fn decode_and_validate(bytes: &[u8]) -> Result<Value, Diagnostic> {
    let StrictValue(value) = serde_json::from_slice(bytes)
        .map_err(|e| Diagnostic::new("invalid_json", format!("Invalid JSON: {e}")))?;
    validation::validate(&value)?;
    Ok(value)
}

/// Checks of a request submitted through acalc-engine beyond `validate`,
/// which library callers don't need.
pub fn check_submission(input: &Value) -> Result<(), Diagnostic> {
    // Submitted area samples are delivered as raster pixels, which cannot
    // carry per-sample source contributions. The library API still returns
    // them.
    if input.pointer("/output/area_source_contributions") == Some(&Value::Bool(true)) {
        return Err(Diagnostic::unsupported(
            "Area receivers are returned as rasters; per-sample source contributions are unavailable.",
        )
        .at("output.area_source_contributions"));
    }
    // Profile identities are checked against executable discovery, not merely
    // accepted as opaque metadata that could mislabel the calculation.
    if let Some(id) = input.get("profile_id").and_then(Value::as_str) {
        let advertised = crate::capabilities()
            .get("profiles")
            .and_then(Value::as_array)
            .is_some_and(|profiles| {
                profiles
                    .iter()
                    .any(|p| p.get("id").and_then(Value::as_str) == Some(id))
            });
        if !advertised {
            return Err(Diagnostic::unsupported(format!(
                "Calculation profile {id:?} is not advertised."
            ))
            .at("profile_id"));
        }
    }
    Ok(())
}

/// `decode_and_validate` and `check_submission`.
pub fn decode_submission(bytes: &[u8]) -> Result<Value, Diagnostic> {
    let value = decode_and_validate(bytes)?;
    check_submission(&value)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structural_errors_are_invalid_json() {
        for payload in [
            "{} {}",
            "{\"sources\":true}",
            "{\"extra\":0}",
            "{\"sources\":[],\"sources\":[]}",
        ] {
            assert_eq!(
                decode_and_validate(payload.as_bytes()).unwrap_err().code,
                "invalid_json",
                "{payload}"
            );
        }
    }
}
