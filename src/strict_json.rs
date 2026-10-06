//! Shared strict JSON syntax for delivery adapters, independent of wire framing.

use std::{
    collections::{BTreeMap, HashSet},
    fmt,
};

use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, SeqAccess, Visitor},
};
use serde_json::{Number, Value};

/// Parse exactly one value, rejecting duplicate object keys before normalization.
pub(crate) fn parse(input: &[u8]) -> Result<Value, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    let value = StrictValue::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value.0)
}

/// A JSON value deserializer that rejects duplicate keys at every depth.
struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictValueVisitor)
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .map(StrictValue)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictValue>()? {
            values.push(value.0);
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = HashSet::new();
        let mut values = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(serde::de::Error::custom("duplicate object key"));
            }
            values.insert(key, map.next_value::<StrictValue>()?.0);
        }
        Ok(StrictValue(Value::Object(values.into_iter().collect())))
    }
}

#[cfg(test)]
mod tests {
    use super::parse;
    use serde_json::json;

    #[test]
    fn rejects_duplicate_keys_at_every_depth_and_trailing_values() {
        for input in [
            br#"{"id":1,"id":2}"#.as_slice(),
            br#"{"params":{"arguments":{"project":"a","project":"b"}}}"#.as_slice(),
            br#"{"a":1,"\u0061":2}"#.as_slice(),
            br#"[{"key":1,"key":2}]"#.as_slice(),
            br#"{} {}"#.as_slice(),
            b"{\"key\":\"\xff\"}".as_slice(),
        ] {
            assert!(parse(input).is_err(), "{input:?}");
        }
    }

    #[test]
    fn preserves_full_width_numbers_and_complete_json_values() {
        let input = br#"{"id":18446744073709551615,"values":[null,true,-1,1.25,"text"]}"#;
        assert_eq!(
            parse(input).unwrap(),
            json!({"id": u64::MAX, "values": [null, true, -1, 1.25, "text"]})
        );
    }
}
