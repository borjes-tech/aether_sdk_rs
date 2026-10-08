//! Run one function over many calls at once: fan out, then wait for the result.
//!
//! A plugin's WebAssembly runs on one thread, so a single call cannot use more than one core. What the kernel
//! *can* do is run many calls at the same time, each in its own instance on its own thread, and that is
//! what [`fan_out`] asks for: it splits a list into chunks and queues one background job per chunk. The
//! jobs are ordinary scheduler jobs (retries, queues, `unique_key`), so they run up to the scheduler's
//! `concurrency` at a time. [`Join`] then waits for all of them without holding a call open.
//!
//! ```no_run
//! use aether_sdk::parallel::{self, Chunk, Join, Poll};
//! use aether_sdk::{json, Result};
//!
//! // 1. Start: one job per 200 employees. Ids are small, so they fit a job's 64 KiB payload.
//! fn start(run: &str, employees: &[String]) -> Result<()> {
//!     let launched = parallel::fan_out("calculate_batch")
//!         .group(&format!("payroll-{run}"))
//!         .queue("payroll")
//!         .shared(&json!({ "run": run }))
//!         .chunk_size(200)
//!         .send(employees)?;
//!     // 2. Ask to be told when they are all done.
//!     parallel::join("finish_run", &launched).payload(&json!({ "run": run })).send()?;
//!     Ok(())
//! }
//!
//! // Each job gets one chunk. It may run twice (at least once delivery): make it safe to repeat.
//! fn calculate_batch(chunk: Chunk<String>) -> Result<()> {
//!     for employee in &chunk.items {
//!         let _ = (employee, &chunk.shared["run"]);
//!     }
//!     Ok(())
//! }
//!
//! // 3. Runs now and then until the batches are finished (re-queues itself).
//! fn finish_run(join: Join) -> Result<()> {
//!     match join.poll()? {
//!         Poll::Finished(progress) if progress.all_succeeded() => { /* mark the run calculated */ }
//!         Poll::Finished(_) | Poll::GaveUp(_) => { /* some batches failed: say so */ }
//!         Poll::Waiting => {}
//!     }
//!     Ok(())
//! }
//! ```
//!
//! What to keep in mind:
//!
//! * A job's payload is at most 64 KiB, so a chunk carries *ids or keys*, not the data; the job reads
//!   what it needs. [`FanOut::send`] splits by size as well as by count and refuses an item that cannot fit.
//! * A job is cut off after the scheduler's lease (120 s by default): make a chunk small enough to finish
//!   well inside it.
//! * Jobs are delivered at least once and retried: a chunk must be safe to run twice.
//! * `group` plus the chunk number is the job's `unique_key`, so sending the same fan-out again while its
//!   jobs wait or run queues nothing twice. Put something that changes per run in the group name.
//! * Jobs run as the kernel, with the plugin's own capabilities and model grants, not as the caller.
//!
//! Needs `scheduler::enqueue` (the same capability as [`scheduler`](crate::scheduler)).

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::scheduler;

/// The kernel refuses a payload above 64 KiB; stay under it with room for the envelope.
pub const DEFAULT_MAX_BYTES: usize = 48_000;
/// A plugin may have 10 000 jobs waiting at once.
pub const MAX_CHUNKS: usize = 10_000;

/// What each job of a fan-out receives as its input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk<T> {
    /// The group this chunk belongs to.
    pub group: String,
    /// Its place, from 0.
    pub index: u32,
    /// How many chunks there are in all.
    pub count: u32,
    pub items: Vec<T>,
    /// Whatever the starter gave to [`FanOut::shared`]: the same for every chunk.
    #[serde(default)]
    pub shared: Value,
}

/// One queued job of a fan-out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRef {
    pub index: u32,
    pub id: String,
}

/// A fan-out that has been queued. Keep [`Launched::jobs`] (or hand it to [`join`]) to follow it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launched {
    pub group: String,
    pub jobs: Vec<JobRef>,
    /// False for chunks whose job already existed (a repeated fan-out): they are counted, not queued again.
    pub newly_queued: u32,
}

/// Split `items` into runs by count and by serialized size: no run has more than `max_items` or more
/// than `max_bytes` of JSON (counting `overhead` for what every chunk carries besides the items).
/// Returns the end of each run.
pub fn plan<T: Serialize>(items: &[T], max_items: usize, max_bytes: usize, overhead: usize) -> Result<Vec<usize>> {
    if max_items == 0 {
        return Err(Error::msg("a chunk holds at least one item"));
    }
    let budget = max_bytes.saturating_sub(overhead);
    let mut ends = Vec::new();
    let (mut count, mut bytes) = (0usize, 0usize);
    for (index, item) in items.iter().enumerate() {
        // +1 for the comma between items.
        let size = serde_json::to_vec(item)?.len() + 1;
        if size > budget {
            return Err(Error::msg(format!("item {index} is {size} bytes, more than a chunk may carry ({budget}): send an id, not the data")));
        }
        if count > 0 && (count == max_items || bytes + size > budget) {
            ends.push(index);
            count = 0;
            bytes = 0;
        }
        count += 1;
        bytes += size;
    }
    if count > 0 {
        ends.push(items.len());
    }
    Ok(ends)
}

/// Start describing a fan-out of `function` (one of this plugin's own functions, taking a [`Chunk`]).
pub fn fan_out(function: &str) -> FanOut {
    FanOut { function: function.to_string(), group: None, queue: None, max_attempts: None, delay_secs: None, shared: Value::Null, max_items: 200, max_bytes: DEFAULT_MAX_BYTES }
}

#[derive(Debug, Clone)]
pub struct FanOut {
    function: String,
    group: Option<String>,
    queue: Option<String>,
    max_attempts: Option<u32>,
    delay_secs: Option<u64>,
    shared: Value,
    max_items: usize,
    max_bytes: usize,
}

impl FanOut {
    /// A name for this piece of work, with something that changes per run in it (`payroll-2026-10`).
    #[must_use]
    pub fn group(mut self, group: &str) -> Self {
        self.group = Some(group.to_string());
        self
    }

    /// A named queue, so the work can be given to its own workers.
    #[must_use]
    pub fn queue(mut self, queue: &str) -> Self {
        self.queue = Some(queue.to_string());
        self
    }

    /// How many times to try each chunk in all (1 to 20).
    #[must_use]
    pub fn max_attempts(mut self, attempts: u32) -> Self {
        self.max_attempts = Some(attempts);
        self
    }

    /// Make every chunk wait this long (counted from when that chunk is queued) before it may start.
    #[must_use]
    pub fn delay_secs(mut self, seconds: u64) -> Self {
        self.delay_secs = Some(seconds);
        self
    }

    /// Something every chunk gets alongside its items (a run id, a date). Small: it is copied into each.
    #[must_use]
    pub fn shared(mut self, shared: &impl Serialize) -> Self {
        self.shared = serde_json::to_value(shared).unwrap_or(Value::Null);
        self
    }

    /// The most items in a chunk. Default 200.
    #[must_use]
    pub fn chunk_size(mut self, items: usize) -> Self {
        self.max_items = items;
        self
    }

    /// The most serialized bytes of items in a chunk. Default [`DEFAULT_MAX_BYTES`].
    #[must_use]
    pub fn max_bytes(mut self, bytes: usize) -> Self {
        self.max_bytes = bytes.min(60_000);
        self
    }

    /// Queue one job per chunk. If it fails part way, the chunks already queued stay queued: send the
    /// same fan-out again and only the rest are added.
    pub fn send<T: Serialize>(self, items: &[T]) -> Result<Launched> {
        let group = self.group.clone().ok_or_else(|| Error::msg("name the group: fan_out(..).group(\"...\")"))?;
        if items.is_empty() {
            return Err(Error::msg("there is nothing to fan out"));
        }
        let overhead = serde_json::to_vec(&json!({ "group": group, "index": 0, "count": MAX_CHUNKS, "items": [], "shared": self.shared }))?.len();
        let ends = plan(items, self.max_items, self.max_bytes, overhead)?;
        if ends.len() > MAX_CHUNKS {
            return Err(Error::msg(format!("{} chunks is more than a plugin may queue ({MAX_CHUNKS}): use a larger chunk size", ends.len())));
        }
        let count = ends.len() as u32;
        let mut jobs = Vec::with_capacity(ends.len());
        let mut newly_queued = 0;
        let mut start = 0;
        for (index, end) in ends.into_iter().enumerate() {
            let chunk = Chunk { group: group.clone(), index: index as u32, count, items: items[start..end].iter().collect::<Vec<&T>>(), shared: self.shared.clone() };
            start = end;
            let mut job = scheduler::enqueue(&self.function).payload(&chunk).unique_key(&format!("{group}:{index}"));
            if let Some(queue) = &self.queue {
                job = job.queue(queue);
            }
            if let Some(attempts) = self.max_attempts {
                job = job.max_attempts(attempts);
            }
            if let Some(seconds) = self.delay_secs {
                job = job.delay_secs(seconds);
            }
            let queued = job.send().map_err(|e| Error::msg(format!("chunk {index} of {count} could not be queued ({newly_queued} were): {e}")))?;
            if queued.created {
                newly_queued += 1;
            }
            jobs.push(JobRef { index: index as u32, id: queued.id });
        }
        Ok(Launched { group, jobs, newly_queued })
    }
}

/// How a fan-out is going.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub total: u32,
    pub queued: u32,
    pub running: u32,
    pub succeeded: u32,
    pub failed: u32,
    pub cancelled: u32,
    /// Jobs the scheduler no longer has a record of (finished long ago and cleaned up).
    pub missing: u32,
    /// The chunks that failed, with the last error each gave.
    pub errors: Vec<ChunkError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkError {
    pub index: u32,
    pub error: String,
}

impl Progress {
    /// Nothing is waiting or running any more (some may have failed).
    pub fn is_finished(&self) -> bool {
        self.queued == 0 && self.running == 0
    }

    /// Finished, and every chunk succeeded.
    pub fn all_succeeded(&self) -> bool {
        self.is_finished() && self.failed == 0 && self.cancelled == 0 && self.missing == 0
    }

    /// The indexes of chunks that did not succeed, so only they can be run again.
    pub fn failed_indexes(&self) -> Vec<u32> {
        self.errors.iter().map(|e| e.index).collect()
    }
}

#[derive(Deserialize)]
struct Status {
    state: String,
    #[serde(default)]
    last_error: Option<String>,
}

/// Look at every job of a fan-out.
pub fn progress(jobs: &[JobRef]) -> Result<Progress> {
    let mut out = Progress { total: jobs.len() as u32, ..Progress::default() };
    for job in jobs {
        match scheduler::job::<Status>(&job.id)? {
            None => out.missing += 1,
            Some(status) => match status.state.as_str() {
                "queued" => out.queued += 1,
                "running" => out.running += 1,
                "succeeded" => out.succeeded += 1,
                "cancelled" => out.cancelled += 1,
                _ => {
                    out.failed += 1;
                    out.errors.push(ChunkError { index: job.index, error: status.last_error.unwrap_or_else(|| "failed".to_string()) });
                }
            },
        }
    }
    Ok(out)
}

/// Ask for `function` (one of this plugin's, taking a [`Join`]) to run once the jobs of `launched` are done.
pub fn join(function: &str, launched: &Launched) -> JoinBuilder {
    JoinBuilder { function: function.to_string(), jobs: launched.jobs.clone(), group: launched.group.clone(), every_secs: 10, give_up_after: 8640, then: Value::Null, queue: None }
}

pub struct JoinBuilder {
    function: String,
    jobs: Vec<JobRef>,
    group: String,
    every_secs: u64,
    give_up_after: u32,
    then: Value,
    queue: Option<String>,
}

impl JoinBuilder {
    /// How often to look (seconds, at least 5). Default 10.
    #[must_use]
    pub fn every_secs(mut self, seconds: u64) -> Self {
        self.every_secs = seconds.max(5);
        self
    }

    /// How many looks before giving up (default 8640: a day at 10 s).
    #[must_use]
    pub fn give_up_after(mut self, checks: u32) -> Self {
        self.give_up_after = checks.max(1);
        self
    }

    /// Whatever the finishing function needs to know (a run id): handed back in [`Join::then`].
    #[must_use]
    pub fn payload(mut self, then: &impl Serialize) -> Self {
        self.then = serde_json::to_value(then).unwrap_or(Value::Null);
        self
    }

    /// The queue for the waiting job (keep it off the queue the work runs on).
    #[must_use]
    pub fn queue(mut self, queue: &str) -> Self {
        self.queue = Some(queue.to_string());
        self
    }

    pub fn send(self) -> Result<()> {
        let join = Join { function: self.function, group: self.group, jobs: self.jobs, every_secs: self.every_secs, checks_left: self.give_up_after, then: self.then, queue: self.queue };
        join.requeue(self.every_secs)
    }
}

/// The input of a finishing function. See [`join`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Join {
    pub function: String,
    pub group: String,
    pub jobs: Vec<JobRef>,
    pub every_secs: u64,
    pub checks_left: u32,
    /// What the starter asked to be handed back.
    #[serde(default)]
    pub then: Value,
    #[serde(default)]
    pub queue: Option<String>,
}

/// What a look at a fan-out found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    /// Not finished; this function has been queued to look again.
    Waiting,
    /// Every job has finished (check [`Progress::all_succeeded`]).
    Finished(Progress),
    /// Not finished, and the number of looks ran out.
    GaveUp(Progress),
}

impl Join {
    fn requeue(&self, delay: u64) -> Result<()> {
        let mut job = scheduler::enqueue(&self.function).payload(self).delay_secs(delay).unique_key(&format!("{}:join:{}", self.group, self.checks_left));
        if let Some(queue) = &self.queue {
            job = job.queue(queue);
        }
        job.send()?;
        Ok(())
    }

    /// Look at the jobs. If they are still going, this function is queued again after `every_secs`.
    pub fn poll(&self) -> Result<Poll> {
        let progress = progress(&self.jobs)?;
        if progress.is_finished() {
            return Ok(Poll::Finished(progress));
        }
        if self.checks_left <= 1 {
            return Ok(Poll::GaveUp(progress));
        }
        Join { checks_left: self.checks_left - 1, ..self.clone() }.requeue(self.every_secs)?;
        Ok(Poll::Waiting)
    }

    /// The `then` value as a type.
    pub fn then<T: DeserializeOwned>(&self) -> Result<T> {
        Ok(serde_json::from_value(self.then.clone())?)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;
    use crate::testing::MockKernel;

    type Test = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn chunks_are_cut_by_count() -> Test {
        let items: Vec<u32> = (0..10).collect();
        assert_eq!(plan(&items, 4, 10_000, 0)?, vec![4, 8, 10]);
        assert_eq!(plan(&items, 100, 10_000, 0)?, vec![10]);
        assert_eq!(plan::<u32>(&[], 4, 10_000, 0)?, Vec::<usize>::new());
        assert!(plan(&items, 0, 10_000, 0).is_err());
        Ok(())
    }

    #[test]
    fn chunks_are_cut_by_size_too() -> Test {
        // Each item is a 10 character string: 12 bytes with quotes, plus a comma.
        let items: Vec<String> = (0..9).map(|i| format!("item-{i:05}")).collect();
        let ends = plan(&items, 100, 40, 0)?;
        assert_eq!(ends, vec![3, 6, 9], "three 13-byte items fit in 40 bytes, a fourth does not");
        assert!(plan(&items, 100, 10, 0).is_err(), "an item bigger than a chunk is refused, not cut");
        assert!(plan(&items, 100, 45, 40).is_err(), "the envelope counts against the budget");
        Ok(())
    }

    #[test]
    fn every_item_lands_in_exactly_one_chunk_in_order() -> Test {
        let items: Vec<u32> = (0..1000).collect();
        let ends = plan(&items, 37, 500, 40)?;
        let mut start = 0;
        for end in &ends {
            assert!(*end > start && *end - start <= 37);
            start = *end;
        }
        assert_eq!(start, 1000);
        Ok(())
    }

    fn mock_enqueue(counter: Arc<AtomicUsize>) -> MockKernel {
        MockKernel::new().reply("scheduler::enqueue", move |_| {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            json!({ "ok": true, "data": { "id": format!("job{n}"), "created": true } })
        })
    }

    #[test]
    fn a_fan_out_queues_one_unique_job_per_chunk() -> Test {
        let kernel = mock_enqueue(Arc::new(AtomicUsize::new(0)));
        let _installed = kernel.install();
        let employees: Vec<String> = (0..5).map(|i| format!("e{i}")).collect();
        let launched = fan_out("calculate_batch").group("payroll-2026-10").queue("payroll").max_attempts(4).shared(&json!({ "run": "r1" })).chunk_size(2).send(&employees)?;
        assert_eq!(launched.jobs.len(), 3);
        assert_eq!(launched.newly_queued, 3);
        let calls = kernel.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].1["function"], "calculate_batch");
        assert_eq!(calls[0].1["queue"], "payroll");
        assert_eq!(calls[0].1["max_attempts"], 4);
        assert_eq!(calls[0].1["unique_key"], "payroll-2026-10:0");
        assert_eq!(calls[2].1["unique_key"], "payroll-2026-10:2");
        assert_eq!(calls[0].1["payload"], json!({ "group": "payroll-2026-10", "index": 0, "count": 3, "items": ["e0", "e1"], "shared": { "run": "r1" } }));
        assert_eq!(calls[2].1["payload"]["items"], json!(["e4"]));
        // The job's own input reads back as a Chunk.
        let chunk: Chunk<String> = serde_json::from_value(calls[1].1["payload"].clone())?;
        assert_eq!((chunk.index, chunk.count, chunk.items.len()), (1, 3, 2));
        Ok(())
    }

    #[test]
    fn a_delay_applies_to_every_chunk() -> Test {
        let kernel = mock_enqueue(Arc::new(AtomicUsize::new(0)));
        let _installed = kernel.install();
        fan_out("f").group("g").chunk_size(1).delay_secs(30).send(&["a", "b"])?;
        assert!(kernel.calls().iter().all(|(_, payload)| payload["delay_secs"] == 30));
        Ok(())
    }

    #[test]
    fn a_fan_out_needs_a_group_and_something_to_do() -> Test {
        let kernel = mock_enqueue(Arc::new(AtomicUsize::new(0)));
        let _installed = kernel.install();
        assert!(fan_out("f").send(&["a"]).is_err());
        assert!(fan_out("f").group("g").send::<String>(&[]).is_err());
        assert!(kernel.calls().is_empty(), "nothing is queued when the request is wrong");
        Ok(())
    }

    #[test]
    fn a_repeated_fan_out_reports_what_already_existed() -> Test {
        let kernel = MockKernel::new().reply("scheduler::enqueue", |_| json!({ "ok": true, "data": { "id": "same", "created": false } }));
        let _installed = kernel.install();
        let launched = fan_out("f").group("g").chunk_size(1).send(&["a", "b"])?;
        assert_eq!((launched.jobs.len(), launched.newly_queued), (2, 0));
        Ok(())
    }

    #[test]
    fn a_failure_part_way_says_how_far_it_got() -> Test {
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = seen.clone();
        let kernel = MockKernel::new().reply("scheduler::enqueue", move |_| {
            if counter.fetch_add(1, Ordering::SeqCst) == 2 {
                json!({ "error": "too many jobs waiting", "kind": "invalid_payload" })
            } else {
                json!({ "ok": true, "data": { "id": "j", "created": true } })
            }
        });
        let _installed = kernel.install();
        let error = fan_out("f").group("g").chunk_size(1).send(&["a", "b", "c", "d"]).err().ok_or("it should fail")?;
        let message = error.to_string();
        assert!(message.contains("chunk 2 of 4") && message.contains("2 were"), "{message}");
        Ok(())
    }

    fn jobs() -> Vec<JobRef> {
        (0..5).map(|i| JobRef { index: i, id: format!("j{i}") }).collect()
    }

    fn states(table: &'static [(&'static str, &'static str)]) -> MockKernel {
        MockKernel::new().reply("scheduler::job", move |request| {
            let id = request["id"].as_str().unwrap_or_default();
            match table.iter().find(|(k, _)| *k == id) {
                Some((_, "gone")) | None => json!({ "ok": true, "data": null }),
                Some((_, "failed")) => json!({ "ok": true, "data": { "state": "failed", "last_error": "boom" } }),
                Some((_, state)) => json!({ "ok": true, "data": { "state": state } }),
            }
        })
    }

    #[test]
    fn progress_counts_every_state() -> Test {
        let kernel = states(&[("j0", "succeeded"), ("j1", "running"), ("j2", "queued"), ("j3", "failed"), ("j4", "gone")]);
        let _installed = kernel.install();
        let p = progress(&jobs())?;
        assert_eq!((p.total, p.succeeded, p.running, p.queued, p.failed, p.missing), (5, 1, 1, 1, 1, 1));
        assert_eq!(p.errors, vec![ChunkError { index: 3, error: "boom".into() }]);
        assert!(!p.is_finished() && !p.all_succeeded());
        assert_eq!(p.failed_indexes(), vec![3]);
        Ok(())
    }

    #[test]
    fn finished_with_a_failure_is_finished_but_not_a_success() -> Test {
        let kernel = states(&[("j0", "succeeded"), ("j1", "succeeded"), ("j2", "succeeded"), ("j3", "failed"), ("j4", "succeeded")]);
        let _installed = kernel.install();
        let p = progress(&jobs())?;
        assert!(p.is_finished() && !p.all_succeeded());
        Ok(())
    }

    fn join_of(checks_left: u32) -> Join {
        Join { function: "finish_run".into(), group: "g".into(), jobs: jobs(), every_secs: 10, checks_left, then: json!({ "run": "r1" }), queue: None }
    }

    #[test]
    fn a_join_waits_by_queueing_itself_again() -> Test {
        let kernel = states(&[("j0", "succeeded"), ("j1", "running"), ("j2", "queued"), ("j3", "succeeded"), ("j4", "succeeded")])
            .reply("scheduler::enqueue", |_| json!({ "ok": true, "data": { "id": "again", "created": true } }));
        let _installed = kernel.install();
        let join = join_of(5);
        assert_eq!(join.poll()?, Poll::Waiting);
        let call = kernel.calls().into_iter().find(|(c, _)| c == "scheduler::enqueue").ok_or("no re-queue")?;
        assert_eq!(call.1["function"], "finish_run");
        assert_eq!(call.1["delay_secs"], 10);
        assert_eq!(call.1["unique_key"], "g:join:4");
        assert_eq!(call.1["payload"]["checks_left"], 4);
        assert_eq!(join.then::<Value>()?, json!({ "run": "r1" }));
        Ok(())
    }

    #[test]
    fn a_join_finishes_and_a_join_gives_up() -> Test {
        let done = states(&[("j0", "succeeded"), ("j1", "succeeded"), ("j2", "succeeded"), ("j3", "succeeded"), ("j4", "succeeded")]);
        {
            let _installed = done.install();
            assert!(matches!(join_of(5).poll()?, Poll::Finished(p) if p.all_succeeded()));
            assert!(done.calls().iter().all(|(c, _)| c != "scheduler::enqueue"), "a finished join queues nothing");
        }
        let stuck = states(&[("j0", "running"), ("j1", "running"), ("j2", "running"), ("j3", "running"), ("j4", "running")]);
        let _installed = stuck.install();
        assert!(matches!(join_of(1).poll()?, Poll::GaveUp(p) if p.running == 5));
        Ok(())
    }

    #[test]
    fn a_join_is_started_with_its_first_look_delayed() -> Test {
        let kernel = mock_enqueue(Arc::new(AtomicUsize::new(0)));
        let _installed = kernel.install();
        let launched = Launched { group: "g".into(), jobs: jobs(), newly_queued: 5 };
        join("finish_run", &launched).every_secs(1).give_up_after(3).payload(&json!({ "run": "r1" })).queue("light").send()?;
        let call = &kernel.calls()[0].1;
        assert_eq!(call["delay_secs"], 5, "never looks more often than every 5 seconds");
        assert_eq!(call["queue"], "light");
        assert_eq!(call["payload"]["checks_left"], 3);
        assert_eq!(call["payload"]["jobs"].as_array().map(Vec::len), Some(5));
        Ok(())
    }
}
