//! JSON as Go's encoding/json handled it, which the unit and every stored
//! bucket are used to: every number is a float64, so 20 and 20.0 are the
//! same value (a merge of one over the other changes nothing) and both are
//! written 20. Objects are written with their keys sorted.

use serde_json::{Map, Number, Value};

/// A bucket's fields, or any JSON object.
pub type Fields = Map<String, Value>;

/// A float64 as Go wrote it: whole values without a fraction.
pub fn num(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 {
        Value::Number(Number::from(f as i64))
    } else {
        Number::from_f64(f).map_or(Value::Null, Value::Number)
    }
}

/// v with every number in num's form, so equal values compare equal.
pub fn canonical(v: Value) -> Value {
    match v {
        Value::Number(n) => n.as_f64().map_or(Value::Number(n), num),
        Value::Array(a) => Value::Array(a.into_iter().map(canonical).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, canonical(v))).collect()),
        v => v,
    }
}

/// The first JSON value of a request body, as json.Decoder read it: an
/// object, or None. What follows the value is ignored.
pub fn decode_object(body: &[u8]) -> Option<Fields> {
    let first = serde_json::Deserializer::from_slice(body).into_iter::<Value>().next()?.ok()?;
    match canonical(first) {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// A whole payload as json.Unmarshal into a map read it: Ok(None) for null.
pub fn unmarshal_object(payload: &[u8]) -> Result<Option<Fields>, serde_json::Error> {
    match canonical(serde_json::from_slice(payload)?) {
        Value::Object(m) => Ok(Some(m)),
        Value::Null => Ok(None),
        _ => Err(serde::de::Error::custom("not a JSON object")),
    }
}

/// fmt.Sprint of a decoded value, for the comparisons that used it.
pub fn sprint(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "<nil>".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => other.to_string(),
    }
}

/// A float64, when v is a number.
pub fn number(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
}

/// Whether a bucket field reads as set: true, a non-zero number, or a
/// string other than "", "off" and "false".
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty() && s != "off" && s != "false",
        _ => false,
    }
}

/// An object literal, for the many small ones the bridge builds.
#[macro_export]
macro_rules! fields {
    ($($k:expr => $v:expr),* $(,)?) => {{
        #[allow(unused_mut)]
        let mut m = $crate::json::Fields::new();
        $(m.insert(($k).to_string(), serde_json::Value::from($v));)*
        m
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_as_go_wrote_them() {
        assert_eq!(num(20.0).to_string(), "20");
        assert_eq!(num(20.5).to_string(), "20.5");
        assert_eq!(num(-3.4).to_string(), "-3.4");
        assert_eq!(num(1790679723.0).to_string(), "1790679723");
        assert_eq!(canonical(json!(20)), canonical(json!(20.0)));
        assert_eq!(canonical(json!({"a": [1, 2.0]})).to_string(), r#"{"a":[1,2]}"#);
    }

    #[test]
    fn decode_takes_the_first_value() {
        assert_eq!(decode_object(br#"{"a":1} trailing"#).unwrap()["a"], json!(1));
        assert!(decode_object(b"[1]").is_none());
        assert!(decode_object(b"nope").is_none());
        assert!(unmarshal_object(br#"{"a":1} x"#).is_err());
        assert_eq!(unmarshal_object(b"null").unwrap(), None);
    }

    #[test]
    fn truthy_and_sprint() {
        assert!(truthy(Some(&json!(true))) && truthy(Some(&json!(1))) && truthy(Some(&json!("on"))));
        assert!(!truthy(Some(&json!("off"))) && !truthy(Some(&json!(0))) && !truthy(None));
        assert_eq!(sprint(None), "<nil>");
        assert_eq!(sprint(Some(&json!("F"))), "F");
    }
}
