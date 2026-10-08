//! Calling another plugin's functions.
//!
//! The other plugin must be listed in your `plugin.toml`:
//!
//! ```toml
//! dependencies = ["currency"]
//! ```
//!
//! It runs as the same person as the request, under **its own** permissions: calling a plugin
//! never lends it your access to models, and it never lends you its own. An anonymous visitor
//! can only reach what the other plugin lists in `public_functions`. Calls nest at most four
//! deep and a function cannot be called while it is already running further up the chain.
//! It works the same whether the other plugin is WebAssembly or a Rhai script.
//!
//! Needs the capability `plugins::call`.
//!
//! ```no_run
//! use aether_sdk::{json, plugins, Result};
//!
//! fn price_in_euro(amount: f64) -> Result<f64> {
//!     let converted: serde_json::Value = plugins::call("currency", "convert", &json!({ "amount": amount, "to": "EUR" }))?;
//!     Ok(converted["amount"].as_f64().unwrap_or(0.0))
//! }
//! ```

use serde::{de::DeserializeOwned, Serialize};
use serde_json::json;

use crate::error::Result;
use crate::transport::call as kernel;

/// Call `function` of `plugin` with `input` and read its JSON answer as `T`. If that function
/// fails with a message meant for its caller, this returns it as an error carrying that message.
pub fn call<T: DeserializeOwned>(plugin: &str, function: &str, input: &impl Serialize) -> Result<T> {
    let reply = kernel(
        "plugins::call",
        &json!({ "plugin": plugin, "function": function, "input": serde_json::to_value(input)? }),
    )?;
    Ok(serde_json::from_value(reply["data"].clone())?)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::testing::MockKernel;
    use crate::Error;
    use serde_json::Value;

    #[test]
    fn calls_the_other_plugin_and_reads_its_answer() {
        let kernel = MockKernel::new()
            .reply("plugins::call", |_| json!({ "ok": true, "data": { "amount": 9.2 } }));
        let _installed = kernel.install();
        let answer: Value = call("currency", "convert", &json!({ "amount": 10 })).unwrap();
        assert_eq!(answer["amount"], 9.2);
        assert_eq!(
            kernel.calls()[0].1,
            json!({ "plugin": "currency", "function": "convert", "input": { "amount": 10 } })
        );
    }

    #[test]
    fn an_undeclared_dependency_is_refused() {
        let kernel = MockKernel::new().fail("plugins::call", "error", "`other` is not one of this plugin's dependencies");
        let _installed = kernel.install();
        assert!(matches!(call::<Value>("other", "f", &json!({})), Err(Error::Kernel(_))));
    }
}
