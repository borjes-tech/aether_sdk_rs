//! Events: live updates for the people using the app, and announcements other plugins can react to.
//!
//! An event is transient: it is pushed to the organization's connected members and is not
//! stored. Use [`notify`](crate::notify) for something people should still see later.
//!
//! [`emit`] tells the organization's connected members **and** every plugin that listens to the event.
//! A listener is a function that runs as a background job (retried, at least once) under its own
//! plugin's permissions. The event's full name is `<emitting plugin>.<event>`; to listen to another
//! plugin's events, list it under `dependencies` and declare the listener in `plugin.toml`:
//!
//! ```toml
//! [[events]]
//! name = "chat.message_posted"     # <emitting plugin>.<event>
//! direction = "listen"
//! handler = "on_message"           # a function taking `events::Received`
//! ```
//!
//! or add one while running with [`subscribe`]. A handler that emits an event of its own is passed
//! on at most four handlers deep, so plugins that answer each other cannot loop for ever.
//!
//! Needs the capability `events::emit` to emit and `events::subscribe` to listen.
//!
//! ```no_run
//! use aether_sdk::{events, Result};
//!
//! fn on_message(received: events::Received) -> Result<()> {
//!     let _channel = received.payload["channel"].as_str();
//!     Ok(())
//! }
//! ```

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// What a handler function is given when an event it listens to is emitted.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Received {
    /// The full name, `<plugin>.<event>`.
    pub event: String,
    /// The plugin that emitted it.
    pub source: String,
    /// What it carried.
    pub payload: Value,
    /// The user or visitor whose action caused it, when there was one.
    pub emitted_by: Option<String>,
    /// How many handlers in a row led here (1 for an event emitted by a request).
    pub depth: u32,
}

use crate::error::Result;
use crate::transport::call;

/// Tell the organization's connected members, and the plugins that listen, that `event` happened,
/// with `payload`. `event` is a short name of lower-case letters, digits, `_` and `-` (at most 128
/// bytes) to be heard by plugins; `payload` at most 64 KiB.
pub fn emit(event: &str, payload: &impl Serialize) -> Result<()> {
    call("events::emit", &json!({ "event": event, "payload": serde_json::to_value(payload)? }))?;
    Ok(())
}

/// Run the function `function` of this plugin whenever `event` (a full name, `chat.message_posted`)
/// is emitted. The emitting plugin must be listed under `dependencies`. A listener declared in
/// `plugin.toml` cannot be replaced this way.
pub fn subscribe(event: &str, function: &str) -> Result<()> {
    call("events::subscribe", &json!({ "event": event, "function": function }))?;
    Ok(())
}

/// Remove a listener added with [`subscribe`]. Returns whether there was one.
pub fn unsubscribe(event: &str) -> Result<bool> {
    Ok(call("events::unsubscribe", &json!({ "event": event }))?["data"]["removed"].as_bool().unwrap_or(false))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::testing::MockKernel;

    #[test]
    fn listening_is_added_and_removed_by_full_name() {
        let kernel = MockKernel::new()
            .reply("events::subscribe", |_| json!({ "ok": true, "data": null }))
            .reply("events::unsubscribe", |_| json!({ "ok": true, "data": { "removed": true } }));
        let _installed = kernel.install();
        subscribe("chat.message_posted", "on_message").unwrap();
        assert!(unsubscribe("chat.message_posted").unwrap());
        assert_eq!(kernel.calls()[0].1, json!({ "event": "chat.message_posted", "function": "on_message" }));
    }

    #[test]
    fn a_handler_input_reads_as_received() {
        let received: Received = serde_json::from_value(json!({
            "event": "chat.message_posted", "source": "chat", "payload": { "channel": "general" }, "emitted_by": "users:u1", "depth": 1
        }))
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!((received.source.as_str(), received.depth, received.payload["channel"].as_str()), ("chat", 1, Some("general")));
    }

    #[test]
    fn emit_sends_the_event_and_payload() {
        let kernel = MockKernel::new().reply("events::emit", |_| json!({ "ok": true }));
        let _installed = kernel.install();
        emit("typing", &json!({ "who": "ann" })).unwrap();
        assert_eq!(kernel.calls()[0], ("events::emit".to_string(), json!({ "event": "typing", "payload": { "who": "ann" } })));
    }
}
