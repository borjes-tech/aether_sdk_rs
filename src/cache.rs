//! A small key/value cache for the plugin's own use.
//!
//! Entries are private to your plugin in the organization the call is for: no other plugin
//! can read or clear them. Values are anything that serializes to JSON. The cache may drop
//! an entry at any time (memory pressure, a restart), so keep only what you can recompute,
//! never the only copy of data.
//!
//! Needs the capabilities `cache::get`, `cache::set`, `cache::invalidate` and `cache::clear`
//! for the calls that use them.
//!
//! ```no_run
//! use aether_sdk::{cache, Result};
//!
//! fn rate() -> Result<f64> {
//!     if let Some(rate) = cache::get::<f64>("usd_eur")? {
//!         return Ok(rate);
//!     }
//!     let rate = 0.92; // ... fetched from somewhere slow ...
//!     cache::set("usd_eur", &rate, Some(3600))?;
//!     Ok(rate)
//! }
//! ```

use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

use crate::error::Result;
use crate::transport::call;

/// The value stored under `key`, or `None` when there is none (never set, expired, evicted).
pub fn get<T: DeserializeOwned>(key: &str) -> Result<Option<T>> {
    match call("cache::get", &json!({ "key": key }))?.get("data") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(serde_json::from_value(value.clone())?)),
    }
}

/// Store `value` under `key`. `ttl_secs` is how long it should live (1 second to 30 days);
/// `None` uses the cache's default. Keys are at most 256 bytes.
pub fn set(key: &str, value: &impl Serialize, ttl_secs: Option<u64>) -> Result<()> {
    let mut request = json!({ "key": key, "value": serde_json::to_value(value)? });
    if let Some(ttl) = ttl_secs {
        request["ttl_secs"] = json!(ttl);
    }
    call("cache::set", &request)?;
    Ok(())
}

/// Remove `key`. Returns whether there was anything to remove.
pub fn invalidate(key: &str) -> Result<bool> {
    Ok(removed(&call("cache::invalidate", &json!({ "key": key }))?) > 0)
}

/// Remove every entry whose key starts with `prefix`. Returns how many were removed.
pub fn invalidate_prefix(prefix: &str) -> Result<usize> {
    Ok(removed(&call("cache::invalidate", &json!({ "prefix": prefix }))?))
}

/// Remove all of this plugin's entries in this organization. Returns how many were removed.
pub fn clear() -> Result<usize> {
    Ok(removed(&call("cache::clear", &json!({}))?))
}

fn removed(reply: &Value) -> usize {
    reply["data"]["removed"].as_u64().unwrap_or(0) as usize
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::testing::MockKernel;
    use crate::Error;

    #[test]
    fn set_sends_the_value_and_ttl_and_get_reads_it_back() {
        let kernel = MockKernel::new()
            .reply("cache::set", |_| json!({ "ok": true, "data": null }))
            .reply("cache::get", |_| json!({ "ok": true, "data": { "n": 1 } }));
        let _installed = kernel.install();
        set("k", &json!({ "n": 1 }), Some(60)).unwrap();
        set("plain", &5, None).unwrap();
        let back: Option<Value> = get("k").unwrap();
        assert_eq!(back, Some(json!({ "n": 1 })));
        let calls = kernel.calls();
        assert_eq!(calls[0].1, json!({ "key": "k", "value": { "n": 1 }, "ttl_secs": 60 }));
        assert_eq!(calls[1].1, json!({ "key": "plain", "value": 5 }));
    }

    #[test]
    fn a_missing_entry_is_none() {
        let kernel = MockKernel::new().reply("cache::get", |_| json!({ "ok": true, "data": null }));
        let _installed = kernel.install();
        assert_eq!(get::<String>("nope").unwrap(), None);
    }

    #[test]
    fn invalidating_and_clearing_report_how_many_went() {
        let kernel = MockKernel::new()
            .reply("cache::invalidate", |request| {
                json!({ "ok": true, "data": { "removed": if request.get("prefix").is_some() { 3 } else { 1 } } })
            })
            .reply("cache::clear", |_| json!({ "ok": true, "data": { "removed": 7 } }));
        let _installed = kernel.install();
        assert!(invalidate("k").unwrap());
        assert_eq!(invalidate_prefix("user:").unwrap(), 3);
        assert_eq!(clear().unwrap(), 7);
        assert_eq!(kernel.calls()[1].1, json!({ "prefix": "user:" }));
    }

    #[test]
    fn a_missing_capability_is_reported_as_one() {
        let kernel = MockKernel::new().fail("cache::get", "capability", "plugin does not hold `cache::get`");
        let _installed = kernel.install();
        assert!(matches!(get::<Value>("k"), Err(Error::Capability(_))));
    }
}
