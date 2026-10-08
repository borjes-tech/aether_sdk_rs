//! What [`handler!`](crate::handler) expands to. Not part of the public API.

use serde::{de::DeserializeOwned, Serialize};

use crate::error::Result;

/// Run one plugin function: read the JSON input the kernel passed, call `f`, write its
/// JSON result back. An error becomes the call's failure. Returns the status code Extism
/// expects (0 for success).
#[cfg(target_arch = "wasm32")]
pub fn run<I: DeserializeOwned, O: Serialize>(f: impl FnOnce(I) -> Result<O>) -> i32 {
    use crate::error::Error;

    let outcome = (|| -> Result<String> {
        let raw: String = extism_pdk::input().map_err(|error| Error::Host(error.to_string()))?;
        // A call with no body has no input; a function taking `()` or an `Option` accepts that.
        let input: I = if raw.trim().is_empty() {
            serde_json::from_value(serde_json::Value::Null)?
        } else {
            serde_json::from_str(&raw)?
        };
        Ok(serde_json::to_string(&f(input)?)?)
    })();

    match outcome {
        Ok(output) => match extism_pdk::output(output.as_str()) {
            Ok(()) => 0,
            Err(_) => 1,
        },
        Err(error) => {
            // An error the plugin wrote on purpose (`Error::msg`) is for the caller; the
            // marker tells the kernel so. Anything else is internal and stays in the log.
            let text = match &error {
                Error::Message(message) => format!("{}{message}", crate::USER_ERROR_PREFIX),
                other => other.to_string(),
            };
            if let Ok(memory) = extism_pdk::Memory::from_bytes(text) {
                // SAFETY: `error_set` takes the offset of a block in plugin memory, which
                // `memory` is, and the block stays alive for the call.
                unsafe { extism_pdk::extism::error_set(memory.offset()) };
            }
            1
        }
    }
}

/// Handlers only run inside the kernel's WebAssembly runtime. Natively there is no kernel to
/// give them input, so a handler is tested through the function it calls.
#[cfg(not(target_arch = "wasm32"))]
pub fn run<I: DeserializeOwned, O: Serialize>(_f: impl FnOnce(I) -> Result<O>) -> i32 {
    1
}
