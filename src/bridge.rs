//! Calling a bridge: an integration with an outside service that the kernel has set up.
//!
//! A bridge is a first-party integration such as Paystack (payments) or OpenStreetMap (maps). The
//! administrator fills in its settings once, globally or per organization; the plugin only names the
//! bridge and the action and **never sees a key**. The plugin must list the bridge in `plugin.toml`:
//!
//! ```toml
//! bridges = ["paystack"]
//! ```
//!
//! The call waits for the answer (at most 45 seconds) and returns what the bridge gives back. An error
//! carries the provider's own message. Calls are logged with the plugin and the action, never the
//! parameters.
//!
//! | Bridge | Actions |
//! |---|---|
//! | `paystack` | `initialize_transaction { email, amount, currency?, reference?, callback_url?, metadata? }`, `verify_transaction { reference }` |
//! | `open_street_map` | `geocode { query, limit?, country_codes? }`, `reverse { lat, lon }` |
//!
//! (Email and SMS are not here: use [`communication`](crate::communication).)
//!
//! Needs the capability `bridge::call`.
//!
//! ```no_run
//! use aether_sdk::{bridge, json, Result};
//!
//! fn start_payment(email: &str, kobo: u64) -> Result<String> {
//!     let started: serde_json::Value = bridge::call(
//!         "paystack",
//!         "initialize_transaction",
//!         &json!({ "email": email, "amount": kobo, "currency": "NGN" }),
//!     )?;
//!     Ok(started["authorization_url"].as_str().unwrap_or_default().to_string())
//! }
//! ```

use serde::{de::DeserializeOwned, Serialize};
use serde_json::json;

use crate::error::Result;
use crate::transport::call as kernel;

/// Call `action` of `bridge` with `params` (a JSON object) and read the answer as `T`.
pub fn call<T: DeserializeOwned>(bridge: &str, action: &str, params: &impl Serialize) -> Result<T> {
    let reply = kernel("bridge::call", &json!({ "bridge": bridge, "action": action, "params": serde_json::to_value(params)? }))?;
    Ok(serde_json::from_value(reply["data"].clone())?)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::testing::MockKernel;
    use crate::Error;
    use serde_json::Value;

    #[test]
    fn the_bridge_action_and_parameters_are_sent_and_the_answer_is_read() {
        let kernel = MockKernel::new()
            .reply("bridge::call", |_| json!({ "ok": true, "data": { "authorization_url": "https://pay.example/x" } }));
        let _installed = kernel.install();
        let answer: Value = call("paystack", "initialize_transaction", &json!({ "email": "a@b.co", "amount": 100 })).unwrap();
        assert_eq!(answer["authorization_url"], "https://pay.example/x");
        assert_eq!(
            kernel.calls()[0].1,
            json!({ "bridge": "paystack", "action": "initialize_transaction", "params": { "email": "a@b.co", "amount": 100 } })
        );
    }

    #[test]
    fn an_unlisted_bridge_and_a_provider_refusal_are_errors() {
        let kernel = MockKernel::new()
            .fail("bridge::call", "error", "this plugin may not call bridge `stripe`; list it under `bridges` in plugin.toml");
        let _installed = kernel.install();
        assert!(matches!(call::<Value>("stripe", "charge", &json!({})), Err(Error::Kernel(m)) if m.contains("bridges")));
    }
}
