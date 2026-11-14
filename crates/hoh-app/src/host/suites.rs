//! The compat suites the host runs (docs/adr/ADR-030.md decision 10, "何时运行与互斥", "有界、停止与崩溃"; K6, K7). After
//! each look of the registry's own loop, one suite at a time: for a wanted agent whose last look found it ready and
//! whose verdict is pending, its admission (deferred while the writer slot is held, the suite open meanwhile so no new
//! writer is granted); its run on a thread of its own with a stop of its own, ended past [`SUITE_LIMIT`] and by the
//! host's stop; its publication over the manifest the registry holds, only if the files' stamps at its end are the
//! look's, and the registry's manifest swapped only once the publication committed; the host's ends of it otherwise
//! (`timed_out`, `withdrawn`, `discarded`, `abandoned`). The store's transactions keep the mutual exclusion with the
//! writer slot; this keeps the order and the bounds.

use super::agents::{Ready, Registry, stamp_now};
use super::http::Shared;
use hoh_agents::compat::Agent;
use hoh_agents::compat::suite::{self, Discarded, SuiteResult};
use hoh_agents::probe::ProbeStop;
use hoh_core::compat_manifest::Files;
use hoh_core::delivery::Clock;
use hoh_core::persistence::{
    ABANDONED, Admission, DISCARDED, Publication, Published, SuiteSubject, TIMED_OUT, UNPUBLISHED,
    WITHDRAWN,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a suite may run before the host stops it and ends it `timed_out` (decision 10).
pub(super) const SUITE_LIMIT: Duration = Duration::from_secs(5 * 60);

/// How soon a run that published nothing (unpublished, discarded, timed out) is run again for the same look of its
/// agent: at most every 30 s (decision 10); a new look of the agent runs it at once.
const RETRY_EVERY: Duration = Duration::from_secs(30);

/// The suite this host has open: waiting for the writer slot (no run), or running.
struct Open {
    suite_id: String,
    agent: Agent,
    /// The registry's look it is for.
    ready: Ready,
    run: Option<Run>,
}

struct Run {
    stop: ProbeStop,
    started: Instant,
    task: tokio::task::JoinHandle<Result<SuiteResult, Discarded>>,
}

/// An end to try again each pass: one the store did not commit, or a publication that renamed the manifest and did
/// not commit (its manifest held once the end commits).
struct Closing {
    suite_id: String,
    status: String,
    error: Option<String>,
    adopt: Option<Value>,
}

/// The host's suites, driven by the registry's loop.
pub(super) struct Suites {
    shared: Arc<Shared>,
    clock: Arc<dyn Clock>,
    open: Option<Open>,
    closing: Option<Closing>,
    /// Per agent, the look whose run last published nothing, and when.
    paced: BTreeMap<Agent, (u64, Instant)>,
    /// Publications so far: the temporary files' tags.
    publications: u64,
    /// The last failure said, so a retry each pass says it once.
    said: Option<String>,
}

impl Suites {
    pub(super) fn new(shared: Arc<Shared>, clock: Arc<dyn Clock>) -> Suites {
        Suites {
            shared,
            clock,
            open: None,
            closing: None,
            paced: BTreeMap::new(),
            publications: 0,
            said: None,
        }
    }

    fn say(&mut self, what: String) {
        if self.said.as_ref() != Some(&what) {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "hoh host: the compat suite: {what}");
            self.said = Some(what);
        }
    }

    /// One pass after a look: an end that did not commit first; then the suite open, if any (its run's end, its limit,
    /// or, waiting, its admission again or its withdrawal); else the first agent due is admitted.
    pub(super) async fn step(&mut self, registry: &Arc<Registry>) {
        if !self.close(registry).await {
            return;
        }
        if self.open.is_some() {
            self.carry_on(registry).await;
            return;
        }
        let due = registry
            .pending_suites()
            .into_iter()
            .find(|(agent, ready)| {
                !self
                    .paced
                    .get(agent)
                    .is_some_and(|(look, at)| *look == ready.look && at.elapsed() < RETRY_EVERY)
            });
        if let Some((agent, ready)) = due {
            self.admit(agent, ready, registry).await;
        }
    }

    /// The host stops: a suite open is stopped and ended `abandoned` if that can still be written (decision 10); an end
    /// that did not commit is tried once more. What is not written the next host's epoch closes.
    pub(super) async fn abandon(&mut self) {
        let store = &self.shared.store;
        let project = &self.shared.project_id;
        if let Some(open) = self.open.take() {
            if let Some(run) = &open.run {
                run.stop.stop();
            }
            let _ = store
                .end_suite(project, &open.suite_id, ABANDONED, None)
                .await;
        }
        if let Some(closing) = self.closing.take() {
            let _ = store
                .end_suite(
                    project,
                    &closing.suite_id,
                    &closing.status,
                    closing.error.clone(),
                )
                .await;
        }
    }

    /// The end to try again, if any; whether there is none left.
    async fn close(&mut self, registry: &Registry) -> bool {
        let Some(closing) = &self.closing else {
            return true;
        };
        let ended = self
            .shared
            .store
            .end_suite(
                &self.shared.project_id,
                &closing.suite_id,
                &closing.status,
                closing.error.clone(),
            )
            .await;
        match ended {
            Ok(_) => {
                if let Some(Closing {
                    adopt: Some(manifest),
                    ..
                }) = self.closing.take()
                    && let Err(e) = registry.adopt(manifest)
                {
                    self.say(format!("the manifest published is not held: {e}"));
                }
                self.closing = None;
                true
            }
            Err(e) => {
                let what = format!("suite {} not ended: {e}", closing.suite_id);
                self.say(what);
                false
            }
        }
    }

    /// End `suite_id` with `status`; one that does not commit is tried again each pass.
    async fn end(&mut self, suite_id: String, status: &str, error: Option<String>) {
        let ended = self
            .shared
            .store
            .end_suite(&self.shared.project_id, &suite_id, status, error.clone())
            .await;
        if let Err(e) = ended {
            self.say(format!("suite {suite_id} not ended: {e}"));
            self.closing = Some(Closing {
                suite_id,
                status: status.to_owned(),
                error,
                adopt: None,
            });
        }
    }

    fn pace(&mut self, agent: Agent, look: u64) {
        self.paced.insert(agent, (look, Instant::now()));
    }

    async fn carry_on(&mut self, registry: &Arc<Registry>) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        match &open.run {
            Some(run) if run.task.is_finished() => {
                let Some(open) = self.open.take() else {
                    return;
                };
                let Some(run) = open.run else {
                    return;
                };
                let result = run
                    .task
                    .await
                    .unwrap_or_else(|e| Err(Discarded(format!("its thread ended: {e}"))));
                self.publish(open.suite_id, open.agent, &open.ready, result, registry)
                    .await;
            }
            Some(run) => {
                if run.started.elapsed() >= SUITE_LIMIT {
                    // Stopped: its thread ends by the stop, and what it gives later is never looked at (a publication
                    // would find the suite closed).
                    run.stop.stop();
                    let Some(open) = self.open.take() else {
                        return;
                    };
                    self.pace(open.agent, open.ready.look);
                    self.end(
                        open.suite_id,
                        TIMED_OUT,
                        Some(format!("it ran past {SUITE_LIMIT:?}")),
                    )
                    .await;
                }
            }
            None => {
                // Waiting for the writer slot: still wanted and still the agent's identity, or withdrawn.
                let agent = open.agent;
                let identity = open.ready.entry.identity();
                let current = registry
                    .pending_suites()
                    .into_iter()
                    .find(|(a, ready)| *a == agent && ready.entry.identity() == identity);
                match current {
                    Some((_, ready)) => self.admit(agent, ready, registry).await,
                    None => {
                        if let Some(open) = self.open.take() {
                            self.end(open.suite_id, WITHDRAWN, None).await;
                        }
                    }
                }
            }
        }
    }

    /// Ask the store to admit `agent`'s suite for the look `ready`, and run it if it is admitted.
    async fn admit(&mut self, agent: Agent, ready: Ready, registry: &Arc<Registry>) {
        let subject = SuiteSubject {
            agent: agent.as_str().to_owned(),
            cli_version: ready.version.clone(),
            identity: ready.entry.identity(),
        };
        let admitted = self
            .shared
            .store
            .begin_suite(&self.shared.project_id, subject, self.clock.now())
            .await;
        match admitted {
            Ok(Admission::Started(suite_id)) => {
                let run = self.spawn(agent, &ready, registry);
                self.open = Some(Open {
                    suite_id,
                    agent,
                    ready,
                    run: Some(run),
                });
            }
            Ok(Admission::Deferred(suite_id)) => {
                self.open = Some(Open {
                    suite_id,
                    agent,
                    ready,
                    run: None,
                });
            }
            Ok(Admission::Busy(suite_id)) => {
                // Open in this host's epoch, so this host's, yet not the one it holds: closed, so it holds nothing
                // back.
                if self.open.as_ref().is_none_or(|o| o.suite_id != suite_id) {
                    self.open = None;
                    self.end(
                        suite_id,
                        WITHDRAWN,
                        Some("open in this host epoch but not held by the host".to_owned()),
                    )
                    .await;
                }
            }
            Err(e) => self.say(format!("{}'s suite not admitted: {e}", agent.as_str())),
        }
    }

    /// Run the suite on a thread of its own, with its own stop (registered with the registry, so the host's stop ends
    /// it too).
    fn spawn(&self, agent: Agent, ready: &Ready, registry: &Registry) -> Run {
        let stop = registry.suite_stop();
        let env = registry.suite_env();
        let entry = ready.entry.clone();
        let hooks = self.shared.hooks.clone();
        let running = stop.clone();
        let task = tokio::task::spawn_blocking(move || {
            let ran = suite::run(agent, &entry, &env, &running);
            hooks.suite_ran(agent.as_str());
            ran
        });
        Run {
            stop,
            started: Instant::now(),
            task,
        }
    }

    /// A run's end: published if it earned an entry and the files' stamps are still the look's; ended `discarded`
    /// otherwise (decision 10, "结束时另重读身份戳").
    async fn publish(
        &mut self,
        suite_id: String,
        agent: Agent,
        ready: &Ready,
        result: Result<SuiteResult, Discarded>,
        registry: &Registry,
    ) {
        let result = match result {
            Ok(result) => result,
            Err(Discarded(why)) => {
                self.pace(agent, ready.look);
                self.end(suite_id, DISCARDED, Some(why)).await;
                return;
            }
        };
        let now = stamp_now(&ready.launcher, Path::new(&ready.entry.implementation_path));
        if now != Some(ready.stamp) {
            self.pace(agent, ready.look);
            self.end(
                suite_id,
                DISCARDED,
                Some(format!(
                    "the files' stamps changed while it ran: {:?} at its start, {now:?} at its end",
                    ready.stamp
                )),
            )
            .await;
            return;
        }
        let status = serde_json::to_value(result.entry.suite.status)
            .ok()
            .and_then(|s| s.as_str().map(str::to_owned))
            .unwrap_or_default();
        let entry = match serde_json::to_value(&result.entry) {
            Ok(entry) => entry,
            Err(e) => {
                self.pace(agent, ready.look);
                self.end(suite_id, UNPUBLISHED, Some(e.to_string())).await;
                return;
            }
        };
        self.publications += 1;
        let publication = Publication {
            project_id: self.shared.project_id.clone(),
            suite_id: suite_id.clone(),
            agent: agent.as_str().to_owned(),
            entry,
            base: registry.manifest(),
            files: Files::of(
                &self.shared.root,
                &format!("{}.{}", std::process::id(), self.publications),
            ),
            now: self.clock.now(),
        };
        match self.shared.store.publish_suite(publication).await {
            Ok(Published::Replaced(manifest)) => {
                if let Err(e) = registry.adopt(manifest) {
                    self.say(format!("the manifest published is not held: {e}"));
                }
            }
            // Ended meanwhile (past its limit, or by a new epoch): nothing was written.
            Ok(Published::Closed) => {}
            Ok(Published::Unpublished(why)) => {
                self.pace(agent, ready.look);
                self.say(format!("{}'s result not published: {why}", agent.as_str()));
            }
            Ok(Published::Uncommitted { manifest, error }) => {
                self.say(format!(
                    "{}'s manifest replaced, its suite not ended: {error}",
                    agent.as_str()
                ));
                self.closing = Some(Closing {
                    suite_id,
                    status,
                    error: None,
                    adopt: Some(manifest),
                });
            }
            Err(e) => {
                self.pace(agent, ready.look);
                self.say(format!("{}'s result not published: {e}", agent.as_str()));
                self.closing = Some(Closing {
                    suite_id,
                    status: UNPUBLISHED.to_owned(),
                    error: Some(e.to_string()),
                    adopt: None,
                });
            }
        }
    }
}
