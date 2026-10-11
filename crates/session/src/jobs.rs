use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::{SessionEvent, Stamp};
use anyhow::{Context, Result, ensure};
use nuzky_engine::edit::new_id;
use serde_json::{Value, json};

const MAX_ACTIVE: usize = 4;
/// Results of the most recent finished jobs stay readable; older ones are forgotten.
const MAX_FINISHED: usize = 32;

pub struct JobState {
    pub id: String,
    pub owner: String,
    pub run_id: Option<String>,
    pub kind: &'static str,
    pub stamp: Stamp,
    pub status: &'static str,
    pub progress: Option<f32>,
    pub phase: &'static str,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub cancel: Arc<AtomicBool>,
    sequence: u64,
    started: Instant,
    ended: Option<Instant>,
    estimate_from: Option<(Instant, f32)>,
    events: Option<Sender<SessionEvent>>,
    last_event: Instant,
}

impl JobState {
    /// A finished analysis's result can be large, and polling progress does not need it.
    fn summary(&self) -> Value {
        let now = self.ended.unwrap_or_else(Instant::now);
        let remaining = self.estimate_from.and_then(|(at, initial)| {
            let elapsed = now.duration_since(at).as_secs_f64();
            let progress = self.progress?;
            (elapsed >= 5.0 && progress - initial >= 0.03)
                .then(|| (elapsed * f64::from(1.0 - progress) / f64::from(progress - initial)) as u64)
        });
        json!({"job_id": self.id, "kind": self.kind, "revision": self.stamp.revision,
            "session_epoch": self.stamp.session_epoch, "status": self.status,
            "progress": self.progress, "phase": self.phase, "cancel_requested": self.cancel.load(Ordering::Relaxed),
            "elapsed_s": now.duration_since(self.started).as_secs(), "remaining_s": remaining,
            "owner": self.owner, "run_id": self.run_id, "error": self.error})
    }

    fn json(&self) -> Value {
        let mut job = self.summary();
        job["result"] = json!(self.result);
        job
    }

    fn emit(&mut self) {
        self.last_event = Instant::now();
        if let Some(events) = &self.events {
            let _ = events.send(SessionEvent::Job {
                id: self.id.clone(),
                kind: self.kind,
                status: self.status,
                progress: self.progress.unwrap_or(0.0),
                error: self.error.clone(),
            });
        }
    }
}

#[derive(Clone)]
pub struct Progress(Arc<Mutex<JobState>>);
impl Progress {
    pub fn set(&self, phase: &'static str, fraction: Option<f32>) {
        let mut state = self.0.lock().unwrap();
        let phase_changed = state.phase != phase;
        let before = state.progress;
        // A job whose next phase counts from 0 again, such as an export after finding the person,
        // estimates the time left of that phase.
        if fraction.unwrap_or(0.0) < before.unwrap_or(0.0) {
            state.estimate_from = None;
        }
        if let Some(fraction) = fraction.filter(|f| *f > 0.0)
            && state.estimate_from.is_none()
        {
            state.estimate_from = Some((Instant::now(), fraction));
        }
        state.phase = phase;
        state.progress = fraction;
        if phase_changed || (fraction != before && state.last_event.elapsed() >= Duration::from_millis(250)) {
            state.emit();
        }
    }
}

#[derive(Default)]
struct Lifecycle {
    stopped_runs: HashSet<String>,
    disconnected_clients: HashSet<String>,
    /// Owners whose jobs outlive their connections: the AI panel's conversation, which connects anew every turn.
    conversations: HashSet<String>,
    closing: bool,
}

#[derive(Default)]
pub struct Jobs {
    lifecycle: Mutex<Lifecycle>,
    entries: Mutex<HashMap<String, Arc<Mutex<JobState>>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    started: AtomicU64,
    events: Option<Sender<SessionEvent>>,
}

impl Jobs {
    pub fn with_events(mut self, events: Option<Sender<SessionEvent>>) -> Self {
        self.events = events;
        self
    }

    pub fn start(
        &self,
        owner: &str,
        run_id: Option<&str>,
        kind: &'static str,
        stamp: Stamp,
        work: impl FnOnce(Arc<AtomicBool>, Progress) -> Result<Value> + Send + 'static,
    ) -> Result<Value> {
        let lifecycle = self.lifecycle.lock().unwrap();
        ensure!(!lifecycle.closing, "APP_CLOSED: host is closing");
        ensure!(!lifecycle.disconnected_clients.contains(owner), "CLIENT_CLOSED: client disconnected");
        ensure!(
            run_id.is_none_or(|run| !lifecycle.stopped_runs.contains(run)),
            "RUN_STOPPED: run was stopped by the user"
        );
        let mut entries = self.entries.lock().unwrap();
        ensure!(
            entries.values().filter(|j| j.lock().unwrap().status == "running").count() < MAX_ACTIVE,
            "JOB_LIMIT: wait for or cancel an active job"
        );
        let mut finished: Vec<_> = entries
            .iter()
            .filter_map(|(id, state)| {
                let state = state.lock().unwrap();
                (state.status != "running").then(|| (state.sequence, id.clone()))
            })
            .collect();
        finished.sort_unstable();
        for (_, id) in finished.iter().take(finished.len().saturating_sub(MAX_FINISHED)) {
            entries.remove(id);
        }
        let id = new_id();
        let state = Arc::new(Mutex::new(JobState {
            id: id.clone(),
            owner: owner.into(),
            run_id: run_id.map(str::to_owned),
            kind,
            stamp,
            status: "running",
            progress: None,
            phase: "starting",
            result: None,
            error: None,
            cancel: Arc::new(AtomicBool::new(false)),
            sequence: self.started.fetch_add(1, Ordering::Relaxed),
            started: Instant::now(),
            ended: None,
            estimate_from: None,
            events: self.events.clone(),
            last_event: Instant::now(),
        }));
        let response = {
            let mut state = state.lock().unwrap();
            state.emit();
            state.json()
        };
        let owned = state.clone();
        let worker = std::thread::Builder::new()
            .name(format!("nuzky-{kind}"))
            .spawn(move || {
                let cancel = owned.lock().unwrap().cancel.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    work(cancel.clone(), Progress(owned.clone()))
                }));
                let result = result.unwrap_or_else(|_| Err(anyhow::anyhow!("JOB_FAILED: worker panicked")));
                let mut state = owned.lock().unwrap();
                // An export that has already atomically published its file is complete.
                if cancel.load(Ordering::Relaxed) && !(kind == "export" && result.is_ok()) {
                    state.status = "cancelled";
                    state.phase = "cancelled";
                } else {
                    match result {
                        Ok(output) => {
                            state.status = "done";
                            state.phase = "done";
                            state.progress = Some(1.0);
                            state.result = Some(output);
                        }
                        Err(error) => {
                            log::error!("Agent {kind} job failed: {error:#}");
                            state.status = "failed";
                            state.phase = "failed";
                            state.error = Some(format!("{error:#}"));
                        }
                    }
                }
                state.ended = Some(Instant::now());
                state.emit();
            })
            .context("JOB_FAILED: starting thread")?;
        entries.insert(id, state);
        let mut workers = self.workers.lock().unwrap();
        let (done, running): (Vec<_>, Vec<_>) = workers.drain(..).partition(|worker| worker.is_finished());
        *workers = running;
        workers.push(worker);
        for worker in done {
            let _ = worker.join();
        }
        Ok(response)
    }

    pub fn get(&self, id: &str, cancel: bool) -> Result<Value> {
        self.get_for(None, id, cancel)
    }

    pub fn get_for(&self, client: Option<&str>, id: &str, cancel: bool) -> Result<Value> {
        let entries = self.entries.lock().unwrap();
        let state = entries.get(id).context("UNKNOWN_JOB: no such job in this session")?.lock().unwrap();
        ensure!(
            !cancel || client.is_none_or(|client| state.owner == client),
            "UNAUTHORIZED: job belongs to another client"
        );
        if cancel && state.status == "running" {
            state.cancel.store(true, Ordering::Relaxed);
        }
        Ok(state.json())
    }

    pub fn wait_for(&self, client: &str, id: &str, timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let ended = {
                let lifecycle = self.lifecycle.lock().unwrap();
                lifecycle.closing || lifecycle.disconnected_clients.contains(client)
            };
            let state = self.get_for(Some(client), id, false)?;
            if ended || state["status"] != "running" || state["cancel_requested"] == true || Instant::now() >= deadline
            {
                return Ok(state);
            }
            std::thread::sleep(Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    /// A running job of `kind`, to name again instead of starting the same work twice.
    pub fn running_id(&self, kind: &str) -> Option<String> {
        let entries = self.entries.lock().unwrap();
        entries
            .values()
            .map(|state| state.lock().unwrap())
            .find(|s| s.status == "running" && s.kind == kind)
            .map(|s| s.id.clone())
    }

    /// Kinds of the jobs that are still running.
    pub fn running(&self) -> Vec<&'static str> {
        let entries = self.entries.lock().unwrap();
        entries.values().map(|state| state.lock().unwrap()).filter(|s| s.status == "running").map(|s| s.kind).collect()
    }

    /// The jobs of `owner` without their results, oldest first: those running and those finished lately.
    pub fn owned_by(&self, owner: &str) -> Vec<Value> {
        let entries = self.entries.lock().unwrap();
        let mut jobs: Vec<_> = entries
            .values()
            .map(|state| state.lock().unwrap())
            .filter(|s| s.owner == owner)
            .map(|s| (s.sequence, s.summary()))
            .collect();
        jobs.sort_unstable_by_key(|(sequence, _)| *sequence);
        jobs.into_iter().map(|(_, job)| job).collect()
    }

    /// A client disconnected: its jobs stop, unless it is part of a conversation.
    pub fn cancel_owner(&self, owner: &str) {
        let mut lifecycle = self.lifecycle.lock().unwrap();
        let conversation = lifecycle.conversations.contains(owner);
        if !conversation {
            lifecycle.disconnected_clients.insert(owner.into());
        }
        for state in self.entries.lock().unwrap().values() {
            let state = state.lock().unwrap();
            // A reframe writes into the turn's run, which ends with the connection.
            if state.owner == owner && state.status == "running" && (!conversation || state.kind == "reframe") {
                state.cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Lets the jobs of `owner` run on after it disconnects, until the conversation ends.
    pub fn begin_conversation(&self, owner: &str) {
        self.lifecycle.lock().unwrap().conversations.insert(owner.into());
    }

    pub fn in_conversation(&self, owner: &str) -> bool {
        self.lifecycle.lock().unwrap().conversations.contains(owner)
    }

    /// Stops the conversation's jobs, and it starts no more.
    pub fn end_conversation(&self, owner: &str) {
        self.lifecycle.lock().unwrap().conversations.remove(owner);
        self.cancel_owner(owner);
    }

    pub fn cancel_run(&self, run_id: &str) {
        let mut lifecycle = self.lifecycle.lock().unwrap();
        lifecycle.stopped_runs.insert(run_id.into());
        for state in self.entries.lock().unwrap().values() {
            let state = state.lock().unwrap();
            if state.run_id.as_deref() == Some(run_id) {
                state.cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    pub fn begin_shutdown(&self) {
        let mut lifecycle = self.lifecycle.lock().unwrap();
        lifecycle.closing = true;
        for state in self.entries.lock().unwrap().values() {
            state.lock().unwrap().cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn shutdown(&self) {
        self.begin_shutdown();
        for worker in self.workers.lock().unwrap().drain(..) {
            let _ = worker.join();
        }
    }
}

pub fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED: job cancelled");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_wait_timeout_completion_and_stop() {
        for stop in ["cancel", "disconnect", "shutdown", "run"] {
            let jobs = Arc::new(Jobs::default());
            let (tx, rx) = std::sync::mpsc::channel();
            let job = jobs
                .start("client", Some("run"), "test", Stamp { revision: 1, session_epoch: "e".into() }, move |_, _| {
                    rx.recv().unwrap();
                    Ok(json!({}))
                })
                .unwrap();
            let id = job["job_id"].as_str().unwrap();
            let at = Instant::now();
            assert_eq!(jobs.wait_for("client", id, Duration::from_secs(1)).unwrap()["status"], "running");
            assert!(at.elapsed() >= Duration::from_secs(1) && at.elapsed() < Duration::from_millis(1500));
            std::thread::scope(|scope| {
                let waiting = scope.spawn(|| jobs.wait_for("client", id, Duration::from_secs(30)).unwrap());
                std::thread::sleep(Duration::from_millis(50));
                let at = Instant::now();
                match stop {
                    "cancel" => {
                        jobs.get(id, true).unwrap();
                    }
                    "disconnect" => jobs.cancel_owner("client"),
                    "shutdown" => jobs.begin_shutdown(),
                    _ => jobs.cancel_run("run"),
                }
                waiting.join().unwrap();
                assert!(at.elapsed() < Duration::from_millis(250));
            });
            tx.send(()).unwrap();
            jobs.shutdown();
            let at = Instant::now();
            assert_eq!(jobs.wait_for("client", id, Duration::from_secs(30)).unwrap()["status"], "cancelled");
            assert!(at.elapsed() < Duration::from_millis(50));
        }
    }

    #[test]
    fn job_progress_estimate_and_events() {
        let (events, rx) = std::sync::mpsc::channel();
        let jobs = Jobs::default().with_events(Some(events));
        let (release, hold) = std::sync::mpsc::channel();
        let (send, receive) = std::sync::mpsc::channel();
        let job = jobs
            .start("client", None, "test", Stamp { revision: 1, session_epoch: "e".into() }, move |_, progress| {
                send.send(progress).unwrap();
                hold.recv().unwrap();
                Ok(json!({}))
            })
            .unwrap();
        let progress = receive.recv().unwrap();
        assert!(matches!(rx.recv().unwrap(), SessionEvent::Job { status: "running", progress: 0.0, .. }));
        progress.set("recognising", Some(0.1));
        assert!(matches!(rx.recv().unwrap(), SessionEvent::Job { progress: 0.1, .. }));
        assert!(progress.0.lock().unwrap().json()["remaining_s"].is_null());
        {
            let mut state = progress.0.lock().unwrap();
            state.started = Instant::now() - Duration::from_secs(10);
            state.estimate_from = Some((Instant::now() - Duration::from_secs(5), 0.1));
        }
        progress.set("recognising", Some(0.12));
        assert!(progress.0.lock().unwrap().json()["remaining_s"].is_null());
        progress.set("aligning", Some(0.5));
        let state = progress.0.lock().unwrap().json();
        assert_eq!(state["elapsed_s"], 10);
        assert_eq!(state["remaining_s"], 6);
        assert!(matches!(rx.recv().unwrap(), SessionEvent::Job { progress: 0.5, .. }));
        progress.set("aligning", Some(0.55));
        assert!(rx.try_recv().is_err(), "progress alone is sent at most every 250 ms");
        progress.set("rendering", Some(0.25));
        let state = progress.0.lock().unwrap().json();
        assert!(
            state["progress"] == 0.25 && state["remaining_s"].is_null(),
            "a phase from 0 again estimates anew: {state}"
        );
        assert!(matches!(rx.recv().unwrap(), SessionEvent::Job { progress: 0.25, .. }));
        std::thread::sleep(Duration::from_millis(250));
        progress.set("rendering", Some(0.75));
        assert!(matches!(rx.recv().unwrap(), SessionEvent::Job { progress: 0.75, .. }));
        release.send(()).unwrap();
        jobs.wait_for("client", job["job_id"].as_str().unwrap(), Duration::from_secs(1)).unwrap();
        jobs.shutdown();
        assert!(matches!(rx.recv().unwrap(), SessionEvent::Job { status: "done", progress: 1.0, .. }));
    }

    #[test]
    fn finished_jobs_and_their_threads_are_pruned() {
        let jobs = Jobs::default();
        let mut ids = Vec::new();
        for _ in 0..MAX_FINISHED + 8 {
            let stamp = Stamp { revision: 1, session_epoch: "epoch".into() };
            let started = jobs.start("client", None, "test", stamp, |_, _| Ok(json!({}))).unwrap();
            let id = started["job_id"].as_str().unwrap().to_owned();
            while jobs.get(&id, false).unwrap()["status"] == "running" {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            ids.push(id);
        }
        assert!(jobs.entries.lock().unwrap().len() <= MAX_FINISHED + 1);
        assert!(jobs.workers.lock().unwrap().len() < 8);
        assert!(jobs.get(&ids[0], false).unwrap_err().to_string().starts_with("UNKNOWN_JOB"));
        assert_eq!(jobs.get(ids.last().unwrap(), false).unwrap()["status"], "done");
        assert_eq!(jobs.get(&ids[ids.len() - MAX_FINISHED], false).unwrap()["status"], "done");
    }
    #[test]
    fn cancel_does_not_publish_late_results() {
        let jobs = Jobs::default();
        let (tx, rx) = std::sync::mpsc::channel();
        let started = jobs
            .start("client", None, "test", Stamp { revision: 7, session_epoch: "epoch".into() }, move |_, _| {
                rx.recv().unwrap();
                Ok(json!({"late":true}))
            })
            .unwrap();
        let id = started["job_id"].as_str().unwrap();
        let cancel = jobs.get(id, true).unwrap();
        assert_eq!(cancel["cancel_requested"], true);
        assert_eq!(cancel["status"], "running");
        tx.send(()).unwrap();
        jobs.shutdown();
        let result = jobs.get(id, false).unwrap();
        assert_eq!(result["status"], "cancelled");
        assert_eq!(result["result"], Value::Null);
        assert_eq!(result["revision"], 7);
    }

    #[test]
    fn a_conversation_keeps_its_jobs_after_a_turn_except_a_reframe() {
        let jobs = Jobs::default();
        jobs.begin_conversation("chat");
        let start = |kind| {
            let stamp = Stamp { revision: 1, session_epoch: "epoch".into() };
            let started = jobs.start("chat", None, kind, stamp, |cancel, _| {
                while !cancel.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Ok(json!({}))
            });
            started.unwrap()["job_id"].as_str().unwrap().to_owned()
        };
        let export = start("export");
        let reframe = start("reframe");
        let cancelled = |id: &str| jobs.get(id, false).unwrap()["cancel_requested"] == true;
        jobs.cancel_owner("chat");
        assert!(!cancelled(&export));
        assert!(cancelled(&reframe));
        assert!(jobs.owned_by("chat").iter().all(|job| job.get("result").is_none()));
        jobs.end_conversation("chat");
        assert!(cancelled(&export));
        jobs.shutdown();
    }
}
