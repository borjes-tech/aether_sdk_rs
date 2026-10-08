//! Helpers for plugins that handle records as JSON ([`Value`]) rather than as one struct per model.
//!
//! Plenty of business code is "read a record, check a few fields, write a change". These keep that
//! short: read a text field, require a record to exist, pick the fields a caller may set, and ask
//! what day it is.

use serde_json::{Map, Value};

use crate::error::{Error, Result};

/// A record as the kernel returns it.
pub type Record = Value;

/// A text field of a record; empty text counts as no value.
pub fn text<'a>(record: &'a Record, field: &str) -> Option<&'a str> {
    record.get(field).and_then(Value::as_str).filter(|text| !text.is_empty())
}

/// The record's `id`.
pub fn id_of(record: &Record) -> Result<&str> {
    text(record, "id").ok_or_else(|| Error::msg("the record has no id"))
}

/// The record `id` of `model`, or an error naming what is missing (`what` is how a person calls
/// it: "employee").
pub fn require(model: &str, id: &str, what: &str) -> Result<Record> {
    crate::db::get::<Record>(model, id)?.ok_or_else(|| Error::msg(format!("there is no {what} `{id}`")))
}

/// Only the fields of `input` named in `names`: what a caller is allowed to set.
pub fn pick(input: &Record, names: &[&str]) -> Record {
    let mut out = Map::new();
    for name in names {
        if let Some(value) = input.get(*name) {
            out.insert((*name).to_string(), value.clone());
        }
    }
    Value::Object(out)
}

/// Today's date in UTC as `YYYY-MM-DD`.
pub fn today() -> Result<String> {
    let context = crate::context::current()?;
    let today = context.today();
    if today.is_empty() {
        return Err(Error::msg("the kernel did not say what day it is"));
    }
    Ok(today.to_string())
}

/// The next number of a series, such as `EMP-00042` for `next_number("employee", "EMP-", 5)`.
///
/// The plugin needs a model named `counter` with a unique string field `key` and an int field
/// `next` (default 0), granted for read and write. Counting is done by the database, so two
/// calls at once never get the same number.
pub fn next_number(series: &str, prefix: &str, width: usize) -> Result<String> {
    use serde_json::json;

    let counter = match crate::db::find::<Record>("counter").filter("key", series).first()? {
        Some(counter) => counter,
        None => match crate::db::create::<Record>("counter", &json!({ "key": series, "next": 0 })) {
            Ok(counter) => counter,
            // Another call created it between our look and our write: use theirs.
            Err(_) => crate::db::find::<Record>("counter")
                .filter("key", series)
                .first()?
                .ok_or_else(|| Error::msg("could not start the number series"))?,
        },
    };
    let bumped = crate::db::increment::<Record>("counter", id_of(&counter)?, "next", 1.0)?
        .ok_or_else(|| Error::msg("could not count"))?;
    let number = bumped.get("next").and_then(Value::as_i64).unwrap_or(1);
    Ok(crate::dates::numbered(prefix, number, width))
}

/// Whether a boolean field is explicitly `false` (an absent field is not).
pub fn is_off(record: &Record, field: &str) -> bool {
    record.get(field) == Some(&Value::Bool(false))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn text_ignores_empty_values_and_other_types() {
        let record = json!({ "a": "x", "b": "", "c": 3 });
        assert_eq!(text(&record, "a"), Some("x"));
        assert_eq!(text(&record, "b"), None);
        assert_eq!(text(&record, "c"), None);
        assert_eq!(text(&record, "d"), None);
    }

    #[test]
    fn pick_keeps_only_the_named_fields() {
        let picked = pick(&json!({ "name": "A", "state": "approved", "x": 1 }), &["name", "missing"]);
        assert_eq!(picked, json!({ "name": "A" }));
    }

    #[test]
    fn is_off_only_for_an_explicit_false() {
        let record = json!({ "on": true, "off": false });
        assert!(is_off(&record, "off"));
        assert!(!is_off(&record, "on"));
        assert!(!is_off(&record, "absent"));
    }
}
