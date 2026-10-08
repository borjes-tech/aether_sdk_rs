//! The one door to the kernel.
//!
//! Every SDK function ends up in [`call`]: one JSON request out, one JSON reply back. In a
//! plugin that is the kernel's `aether::command` host function; natively (unit tests) it is a
//! [`MockKernel`](crate::testing::MockKernel).

use serde_json::{json, Value};

use crate::error::{check_reply, Error, Result};

/// How many times in all a write that the database turned back for a conflict is sent.
const CONFLICT_ATTEMPTS: usize = 5;

/// The commands that change data: the ones a conflict can be retried for.
fn is_write(command: &str) -> bool {
    matches!(command, "db::create" | "db::update" | "db::delete" | "db::increment" | "db::relate" | "db::unrelate" | "db::mutate" | "db::transaction")
}

/// SurrealDB refuses a commit when another transaction changed the same record first, and says the
/// transaction "can be retried": nothing of it was applied.
fn is_conflict(error: &Error) -> bool {
    matches!(error, Error::Database(message) if message.contains("Transaction conflict") || message.contains("can be retried"))
}

/// Send `command` with `payload` to the kernel and return its reply, with a failure
/// reply turned into an [`Error`](crate::Error).
///
/// Everything in this crate is built on it. It is public for kernel commands the SDK has
/// no function for yet.
///
/// A write that the database turned back for a **conflict** (two requests changing the same record at
/// once, such as a shared counter) is sent again, up to five times in all: it was not applied, so
/// sending it again cannot do it twice. A plugin cannot sleep, so there is no pause between the tries;
/// by the time the answer came back the other transaction has already committed.
pub fn call(command: &str, payload: &Value) -> Result<Value> {
    let request = json!({ "command": command, "payload": payload }).to_string();
    let attempts = if is_write(command) { CONFLICT_ATTEMPTS } else { 1 };
    let mut last = None;
    for _ in 0..attempts {
        let reply = send(&request)?;
        match check_reply(serde_json::from_str(&reply)?) {
            Err(error) if is_conflict(&error) => last = Some(error),
            other => return other,
        }
    }
    Err(last.unwrap_or_else(|| Error::Host("no answer".into())))
}

#[cfg(target_arch = "wasm32")]
mod host {
    use extism_pdk::host_fn;

    #[host_fn("aether")]
    extern "ExtismHost" {
        pub fn command(request: String) -> String;
    }
}

#[cfg(target_arch = "wasm32")]
fn send(request: &str) -> Result<String> {
    // SAFETY: `command` is the kernel's host function, registered for every plugin; the
    // generated wrapper copies the request into and the reply out of plugin memory.
    unsafe { host::command(request.to_string()) }.map_err(|error| crate::Error::Host(error.to_string()))
}

#[cfg(not(target_arch = "wasm32"))]
fn send(request: &str) -> Result<String> {
    crate::testing::dispatch(request)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::testing::MockKernel;

    type Test = std::result::Result<(), Box<dyn std::error::Error>>;

    fn conflicting(times: usize, tries: Arc<AtomicUsize>) -> MockKernel {
        let answer = |command: &str| {
            let _ = command;
            json!({ "ok": true, "data": { "id": "x" } })
        };
        MockKernel::new().reply("db::create", move |_| {
            if tries.fetch_add(1, Ordering::SeqCst) < times {
                json!({ "error": "Cannot COMMIT: Transaction conflict: Write conflict, retry the transaction. This transaction can be retried", "kind": "db" })
            } else {
                answer("db::create")
            }
        })
    }

    #[test]
    fn a_conflicted_write_is_sent_again() -> Test {
        let tries = Arc::new(AtomicUsize::new(0));
        let kernel = conflicting(2, tries.clone());
        let _installed = kernel.install();
        let reply = call("db::create", &json!({ "model": "m", "data": { "a": 1 } }))?;
        assert_eq!(reply["data"]["id"], "x");
        assert_eq!(tries.load(Ordering::SeqCst), 3);
        Ok(())
    }

    #[test]
    fn it_gives_up_after_five_tries_and_says_why() -> Test {
        let tries = Arc::new(AtomicUsize::new(0));
        let kernel = conflicting(100, tries.clone());
        let _installed = kernel.install();
        let error = call("db::create", &json!({})).err().ok_or("it should fail")?;
        assert!(matches!(error, Error::Database(ref m) if m.contains("conflict")));
        assert_eq!(tries.load(Ordering::SeqCst), 5);
        Ok(())
    }

    #[test]
    fn a_read_is_never_sent_twice_and_other_errors_are_not_retried() -> Test {
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = reads.clone();
        let kernel = MockKernel::new()
            .reply("db::find", move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                json!({ "error": "Transaction conflict: can be retried", "kind": "db" })
            })
            .reply("db::update", |_| json!({ "error": "the field is required", "kind": "invalid_payload" }));
        let _installed = kernel.install();
        assert!(call("db::find", &json!({})).is_err());
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert!(call("db::update", &json!({})).is_err());
        assert_eq!(kernel.calls().iter().filter(|(c, _)| c == "db::update").count(), 1, "a real refusal is not a conflict");
        Ok(())
    }
}
