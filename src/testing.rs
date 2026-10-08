//! Testing a plugin's logic natively, without a kernel.
//!
//! Put the work of each function in an ordinary Rust function and call it from the
//! [`handler!`](crate::handler); then test that function against a [`MockKernel`] that
//! answers the SDK's calls. Only available when building for the host (not for WebAssembly).
//!
//! ```
//! use aether_sdk::{db, testing::MockKernel};
//! use serde_json::json;
//!
//! let kernel = MockKernel::new().reply("db::find", |_| json!({ "ok": true, "data": [{ "id": "m:1" }] }));
//! let _installed = kernel.install();
//!
//! let rows: Vec<serde_json::Value> = db::find("message").all().unwrap();
//! assert_eq!(rows.len(), 1);
//! assert_eq!(kernel.calls()[0].0, "db::find");
//! ```

use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use serde_json::{json, Value};

use crate::error::{Error, Result};

type Responder = Box<dyn Fn(&Value) -> Value + Send + Sync>;

#[derive(Default)]
struct State {
    responders: HashMap<String, Responder>,
    calls: Vec<(String, Value)>,
}

fn locked(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

thread_local! {
    static INSTALLED: RefCell<Option<Arc<Mutex<State>>>> = const { RefCell::new(None) };
}

/// A stand-in for the kernel. Say how each command should answer, install it, and run the
/// code under test.
#[derive(Clone, Default)]
pub struct MockKernel {
    state: Arc<Mutex<State>>,
}

impl MockKernel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer `command` with whatever `respond` returns for its payload. The reply is the
    /// kernel's own JSON, such as `{ "ok": true, "data": [...] }`.
    pub fn reply(self, command: &str, respond: impl Fn(&Value) -> Value + Send + Sync + 'static) -> Self {
        locked(&self.state).responders.insert(command.to_string(), Box::new(respond));
        self
    }

    /// Answer `command` with a refusal of this `kind` (`capability`, `model`,
    /// `invalid_payload`, `db`, ...), as the kernel would.
    pub fn fail(self, command: &str, kind: &str, message: &str) -> Self {
        let (kind, message) = (kind.to_string(), message.to_string());
        self.reply(command, move |_| json!({ "error": message, "kind": kind }))
    }

    /// Route this thread's SDK calls here until the returned guard is dropped.
    pub fn install(&self) -> Installed {
        INSTALLED.with(|slot| *slot.borrow_mut() = Some(self.state.clone()));
        Installed(())
    }

    /// Every call made so far, in order: the command and its payload.
    pub fn calls(&self) -> Vec<(String, Value)> {
        locked(&self.state).calls.clone()
    }
}

/// Keeps a [`MockKernel`] installed; dropping it uninstalls.
#[must_use = "the mock kernel is uninstalled as soon as this is dropped"]
pub struct Installed(());

impl Drop for Installed {
    fn drop(&mut self) {
        INSTALLED.with(|slot| *slot.borrow_mut() = None);
    }
}

/// The native end of [`transport::call`](crate::transport::call).
pub(crate) fn dispatch(request: &str) -> Result<String> {
    let request: Value = serde_json::from_str(request)?;
    let command = request["command"].as_str().unwrap_or_default().to_string();
    let payload = request["payload"].clone();
    let state = INSTALLED
        .with(|slot| slot.borrow().clone())
        .ok_or_else(|| Error::Host("no kernel here: install a MockKernel in your test".into()))?;
    let mut state = locked(&state);
    state.calls.push((command.clone(), payload.clone()));
    let reply = match state.responders.get(&command) {
        Some(respond) => respond(&payload),
        None => json!({ "error": format!("unknown host command `{command}`"), "kind": "unknown_command" }),
    };
    Ok(reply.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::call;

    #[test]
    fn without_a_mock_installed_calls_say_so() {
        assert!(matches!(call("db::get", &json!({})), Err(Error::Host(_))));
    }

    #[test]
    fn unanswered_commands_are_unknown_and_calls_are_recorded() {
        let kernel = MockKernel::new();
        let _installed = kernel.install();
        assert!(matches!(call("nope", &json!({ "a": 1 })), Err(Error::UnknownCommand(_))));
        assert_eq!(kernel.calls(), [("nope".to_string(), json!({ "a": 1 }))]);
    }

    #[test]
    fn dropping_the_guard_uninstalls() {
        let kernel = MockKernel::new().reply("ping", |_| json!({ "ok": true }));
        drop(kernel.install());
        assert!(matches!(call("ping", &json!({})), Err(Error::Host(_))));
    }
}
