//! Work that happens later or on a schedule.
//!
//! * [`enqueue`] runs one of your plugin's own functions **in the background**, now or after a
//!   delay, with automatic retries. It is the equivalent of Celery's `.delay()`.
//! * [`register`] adds a **recurring task**: a cron expression or an interval. Tasks that always
//!   exist belong in `plugin.toml` instead:
//!
//! ```toml
//! [[schedule]]
//! name = "nightly-report"
//! function = "send_report"
//! cron = "0 2 * * *"            # or every = "15m"
//! timezone = "Africa/Lagos"      # optional; UTC otherwise
//! payload = { kind = "daily" }  # optional input for the function
//! ```
//!
//! A job runs as the kernel (not as the person who started it) with your plugin's own
//! capabilities and model grants. It is delivered **at least once**: if a worker dies halfway the
//! job runs again, so write the function so that repeating it is harmless, or give the job a
//! [`unique_key`](Job::unique_key) so the same work is never queued twice. Failures are retried
//! with a growing delay up to the administrator's limit, then recorded as failed.
//!
//! Needs `scheduler::enqueue` (jobs) and `scheduler::register` / `scheduler::cancel` (tasks).
//!
//! ```no_run
//! use aether_sdk::{json, scheduler, Result};
//!
//! fn after_signup(user: &str) -> Result<()> {
//!     scheduler::enqueue("send_onboarding")
//!         .payload(&json!({ "user": user }))
//!         .delay_secs(3600)
//!         .unique_key(&format!("onboarding-{user}"))
//!         .send()?;
//!     Ok(())
//! }
//! ```

use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Map, Value};

use crate::error::Result;
use crate::transport::call;

/// A queued job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enqueued {
    pub id: String,
    /// False when a job with the same `unique_key` was already waiting or running; `id` is then
    /// that job's.
    pub created: bool,
}

/// Start building a job that calls `function` of this plugin.
pub fn enqueue(function: &str) -> Job {
    Job { request: Map::from_iter([("function".to_string(), json!(function))]) }
}

/// A background job under construction. Nothing happens until [`Job::send`].
#[derive(Debug, Clone)]
pub struct Job {
    request: Map<String, Value>,
}

impl Job {
    /// The function's input (a JSON object).
    #[must_use]
    pub fn payload(mut self, payload: &impl Serialize) -> Self {
        self.request.insert("payload".into(), serde_json::to_value(payload).unwrap_or(Value::Null));
        self
    }

    /// Wait this long before it may start (up to a year).
    #[must_use]
    pub fn delay_secs(mut self, seconds: u64) -> Self {
        self.request.insert("delay_secs".into(), json!(seconds));
        self
    }

    /// A named queue, so heavy work can be given to its own workers. Default: `default`.
    #[must_use]
    pub fn queue(mut self, queue: &str) -> Self {
        self.request.insert("queue".into(), json!(queue));
        self
    }

    /// How many times to try it in all (1 to 20). Default: the administrator's setting.
    #[must_use]
    pub fn max_attempts(mut self, attempts: u32) -> Self {
        self.request.insert("max_attempts".into(), json!(attempts));
        self
    }

    /// Queue the job only if no job of this plugin with the same key is waiting or running.
    #[must_use]
    pub fn unique_key(mut self, key: &str) -> Self {
        self.request.insert("unique_key".into(), json!(key));
        self
    }

    /// Put it on the queue.
    pub fn send(self) -> Result<Enqueued> {
        let reply = call("scheduler::enqueue", &Value::Object(self.request))?;
        Ok(Enqueued {
            id: reply["data"]["id"].as_str().unwrap_or_default().to_string(),
            created: reply["data"]["created"].as_bool().unwrap_or(true),
        })
    }
}

/// How one of your jobs is doing, or `None` if there is no such job of yours.
pub fn job<T: DeserializeOwned>(id: &str) -> Result<Option<T>> {
    match call("scheduler::job", &json!({ "id": id }))?.get("data") {
        None | Some(Value::Null) => Ok(None),
        Some(status) => Ok(Some(serde_json::from_value(status.clone())?)),
    }
}

/// Cancel a job that has not started. Returns whether it was cancelled (false if it already
/// started, finished or is not yours).
pub fn cancel_job(id: &str) -> Result<bool> {
    Ok(call("scheduler::cancel_job", &json!({ "id": id }))?["data"]["cancelled"].as_bool().unwrap_or(false))
}

/// Start building a recurring task called `name` that calls `function`.
pub fn register(name: &str, function: &str) -> Task {
    Task { request: Map::from_iter([("name".to_string(), json!(name)), ("function".to_string(), json!(function))]) }
}

/// A recurring task under construction. Give it exactly one of [`cron`](Task::cron) and
/// [`every`](Task::every).
#[derive(Debug, Clone)]
pub struct Task {
    request: Map<String, Value>,
}

impl Task {
    /// A five-field cron expression: `"0 9 * * 1-5"` is 09:00 on weekdays.
    #[must_use]
    pub fn cron(mut self, expression: &str) -> Self {
        self.request.insert("cron".into(), json!(expression));
        self
    }

    /// A fixed interval, at least 10 seconds: `"30s"`, `"15m"`, `"6h"`, `"1d"`.
    #[must_use]
    pub fn every(mut self, interval: &str) -> Self {
        self.request.insert("every".into(), json!(interval));
        self
    }

    /// The time zone a cron expression is read in, such as `"Africa/Lagos"`. Default UTC.
    #[must_use]
    pub fn timezone(mut self, zone: &str) -> Self {
        self.request.insert("timezone".into(), json!(zone));
        self
    }

    /// The function's input (a JSON object).
    #[must_use]
    pub fn payload(mut self, payload: &impl Serialize) -> Self {
        self.request.insert("payload".into(), serde_json::to_value(payload).unwrap_or(Value::Null));
        self
    }

    /// A named queue for the jobs this task makes.
    #[must_use]
    pub fn queue(mut self, queue: &str) -> Self {
        self.request.insert("queue".into(), json!(queue));
        self
    }

    /// After downtime, wait for the next slot instead of running once for what was missed.
    #[must_use]
    pub fn skip_missed(mut self) -> Self {
        self.request.insert("catch_up".into(), json!("skip"));
        self
    }

    /// Create the task, or update it if you registered one with this name before. A task
    /// declared in `plugin.toml` cannot be replaced this way.
    pub fn save(self) -> Result<()> {
        call("scheduler::register", &Value::Object(self.request))?;
        Ok(())
    }
}

/// Remove a task you registered with [`register`]. Returns whether there was one.
pub fn cancel(name: &str) -> Result<bool> {
    Ok(call("scheduler::cancel", &json!({ "name": name }))?["data"]["removed"].as_bool().unwrap_or(false))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::testing::MockKernel;
    use crate::Error;

    #[test]
    fn a_job_is_described_to_the_kernel() {
        let kernel = MockKernel::new()
            .reply("scheduler::enqueue", |_| json!({ "ok": true, "data": { "id": "j1", "created": true } }));
        let _installed = kernel.install();
        let queued = enqueue("send_onboarding")
            .payload(&json!({ "user": "u1" }))
            .delay_secs(3600)
            .queue("slow")
            .max_attempts(5)
            .unique_key("onboarding-u1")
            .send()
            .unwrap();
        assert_eq!(queued, Enqueued { id: "j1".into(), created: true });
        assert_eq!(
            kernel.calls()[0].1,
            json!({
                "function": "send_onboarding", "payload": { "user": "u1" }, "delay_secs": 3600,
                "queue": "slow", "max_attempts": 5, "unique_key": "onboarding-u1"
            })
        );
    }

    #[test]
    fn a_duplicate_is_reported_not_hidden() {
        let kernel = MockKernel::new()
            .reply("scheduler::enqueue", |_| json!({ "ok": true, "data": { "id": "j1", "created": false } }));
        let _installed = kernel.install();
        assert!(!enqueue("f").unique_key("k").send().unwrap().created);
    }

    #[test]
    fn jobs_can_be_checked_and_cancelled() {
        let kernel = MockKernel::new()
            .reply("scheduler::job", |request| {
                if request["id"] == "gone" { json!({ "ok": true, "data": null }) } else { json!({ "ok": true, "data": { "state": "queued" } }) }
            })
            .reply("scheduler::cancel_job", |_| json!({ "ok": true, "data": { "cancelled": true } }));
        let _installed = kernel.install();
        assert_eq!(job::<Value>("j1").unwrap(), Some(json!({ "state": "queued" })));
        assert_eq!(job::<Value>("gone").unwrap(), None);
        assert!(cancel_job("j1").unwrap());
    }

    #[test]
    fn a_task_is_registered_and_removed_by_name() {
        let kernel = MockKernel::new()
            .reply("scheduler::register", |_| json!({ "ok": true, "data": null }))
            .reply("scheduler::cancel", |_| json!({ "ok": true, "data": { "removed": true } }));
        let _installed = kernel.install();
        register("sweep", "sweep_stale").cron("0 * * * *").timezone("Africa/Lagos").queue("slow").skip_missed().save().unwrap();
        assert!(cancel("sweep").unwrap());
        assert_eq!(
            kernel.calls()[0].1,
            json!({ "name": "sweep", "function": "sweep_stale", "cron": "0 * * * *", "timezone": "Africa/Lagos", "queue": "slow", "catch_up": "skip" })
        );
    }

    #[test]
    fn a_bad_schedule_is_an_error() {
        let kernel = MockKernel::new().fail("scheduler::register", "invalid_payload", "`soon` is not an interval");
        let _installed = kernel.install();
        assert!(matches!(register("t", "f").every("soon").save(), Err(Error::InvalidRequest(_))));
    }
}
