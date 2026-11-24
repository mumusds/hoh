//! Durable role input routing, isolated from control so a slow turn cannot hold pause/cancel.
use super::{
    control::Inflight,
    http::Shared,
    runner::{Executor, TickError, no_run},
};
use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use hoh_core::delivery::{Clock, DeliveryError, Dispatcher, Transport};
use hoh_core::persistence::{
    ChangeInput, InputOutcome, InputRouting, OutboxRow, PersistError, SpecAction, SpecRequest,
};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tokio::sync::watch;

pub(super) async fn run(
    shared: Arc<Shared>,
    clock: Arc<dyn Clock>,
    executor: Option<Arc<dyn Executor>>,
    inflight: Inflight,
    mut stop: watch::Receiver<bool>,
    max_attempts: i64,
) {
    let mut watermark = shared.store.watermark();
    let transport = HostTransport {
        shared: &shared,
        clock: clock.as_ref(),
        executor: executor.as_deref(),
        inflight: &inflight,
    };
    let mut deliveries: FuturesUnordered<BoxFuture<'_, (String, Result<(), TickError>)>> =
        FuturesUnordered::new();
    let mut active = BTreeSet::new();
    let mut poll = tokio::time::interval(Duration::from_millis(200));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if *stop.borrow() {
            break;
        }
        tokio::select! {
            biased;
            _=stop.changed()=>break,
            Some((role, outcome))=deliveries.next(), if !deliveries.is_empty()=> {
                active.remove(&role);
                if let Err(e)=outcome {eprintln!("hoh host: input delivery ({role}): {e}");}
                continue;
            },
            changed=watermark.changed()=>{if changed.is_err(){break;}},
            _=poll.tick()=>{}
        }
        let roles = tokio::select! {
            biased;
            _=stop.changed()=>break,
            result=async {
                no_run(shared.store.settle_ended_inputs(&shared.project_id).await)?;
                no_run(no_run(shared.store.reader())?.input_roles(&shared.project_id))
            }=>match result {
                Ok(roles)=>roles,
                Err(e)=>{eprintln!("hoh host: input delivery: {e}");continue;}
            },
        };
        for role in roles {
            // One serial dispatcher per role; a bounded set of borrowed futures, owned by this loop.
            // No detached deliveries survive shutdown. Claim/start/settle and their generation fences stay intact.
            if active.len() == 32 {
                break;
            }
            if !active.insert(role.clone()) {
                continue;
            }
            let transport = &transport;
            let store = &shared.store;
            let clock = clock.as_ref();
            deliveries.push(
                async move {
                    let result = no_run(
                        Dispatcher {
                            store,
                            clock,
                            transport,
                            roles: vec![role.clone()],
                            max_attempts,
                            batch: 32,
                        }
                        .tick()
                        .await,
                    )
                    .map(drop);
                    (role, result)
                }
                .boxed(),
            );
        }
    }
}
fn delivery_error(error: PersistError) -> DeliveryError {
    match error {
        PersistError::Held { run, state } => DeliveryError::Held { run, state },
        PersistError::KeyReuse(_) => DeliveryError::KeyReuse,
        PersistError::FirstInputPending => DeliveryError::Deferred,
        other => DeliveryError::Lost(other.to_string()),
    }
}
struct HostTransport<'a> {
    shared: &'a Arc<Shared>,
    clock: &'a dyn Clock,
    executor: Option<&'a dyn Executor>,
    inflight: &'a Inflight,
}
impl Transport for HostTransport<'_> {
    fn inputs_only(&self) -> bool {
        true
    }
    async fn deliver(&self, row: &OutboxRow) -> Result<String, DeliveryError> {
        let store = &self.shared.store;
        let reply = if row.recipient_role == "runtime" && row.payload["kind"] == "CHANGE" {
            propose_change(self.shared, row).await?
        } else if row.recipient_role == "commander" {
            store
                .consume_input(row.clone(), self.clock.now())
                .await
                .map_err(delivery_error)?
                .ok_or(DeliveryError::Deferred)?
        } else {
            let works = self
                .inflight
                .lock()
                .map_err(|_| DeliveryError::Lost("inflight lock poisoned".into()))?
                .values()
                .filter(|e| {
                    Some(&e.work.run_id) == row.run_id.as_ref()
                        && e.work.role == row.recipient_role
                        && e.work.assignment.role_epoch == row.role_epoch
                })
                .map(|e| e.work.clone())
                .collect::<Vec<_>>();
            let work = if works.len() == 1 {
                works.first().filter(|w| {
                    w.assignment
                        .session_capabilities
                        .as_ref()
                        .is_some_and(|c| c["mid_turn_steer"] == true)
                })
            } else {
                None
            };
            let staged = store
                .route_input(InputRouting::Stage {
                    row: row.clone(),
                    attempt: work.map(|w| w.attempt_id.clone()),
                    now: self.clock.now(),
                })
                .await
                .map_err(delivery_error)?;
            if let Some(reply) = staged.reply {
                reply
            } else if let (Some(input), Some(work), Some(executor)) =
                (staged.steer, work, self.executor)
            {
                use hoh_agents::codex::SteerReceipt;
                let outcome = match tokio::time::timeout(
                    Duration::from_secs(10),
                    executor.steer(work, &input),
                )
                .await
                {
                    Ok(Ok(SteerReceipt::Delivered { .. })) => InputOutcome::Delivered,
                    Ok(Ok(SteerReceipt::NotNow | SteerReceipt::CapabilityUnsupported)) => {
                        InputOutcome::NextAttempt
                    }
                    Ok(Ok(SteerReceipt::Rejected { reason })) => InputOutcome::Rejected(reason),
                    _ => InputOutcome::Rejected("UNKNOWN_DELIVERY".into()),
                };
                store
                    .route_input(InputRouting::Finish {
                        input_id: input.input_id,
                        attempt: work.attempt_id.clone(),
                        outcome,
                    })
                    .await
                    .map_err(delivery_error)?
                    .reply
                    .ok_or(DeliveryError::Deferred)?
            } else {
                return Err(DeliveryError::Deferred);
            }
        };
        let hooks = self.shared.hooks.clone();
        let id = row.message_id.clone();
        super::begun::spawn_blocking(move || hooks.input_delivered(&id))
            .await
            .map_err(|e| DeliveryError::Lost(e.to_string()))?;
        Ok(reply)
    }
}

async fn propose_change(shared: &Arc<Shared>, row: &OutboxRow) -> Result<String, DeliveryError> {
    let input = shared
        .store
        .reader()
        .map_err(delivery_error)?
        .input(&row.message_id)
        .map_err(delivery_error)?
        .ok_or_else(|| DeliveryError::Lost("missing CHANGE input".into()))?;
    let request = if input.state == hoh_core::lifecycle::InputState::Queued {
        let root = shared.root.clone();
        let files = super::begun::spawn_blocking(move || super::spec::files(&root))
            .await
            .map_err(|e| DeliveryError::Lost(e.to_string()))?;
        let files = files.and_then(|files| {
            hoh_core::specindex::generate_index(&files.sources)
                .map(|index| (files, index.sha256()))
                .map_err(Into::into)
        });
        let (files, digest) = match files {
            Ok((files, digest)) => (Ok(files), digest),
            Err(error) => (Err(error), String::new()),
        };
        Some(SpecRequest {
            origin: hoh_core::origin::Origin::Unknown,
            action: SpecAction::Propose(hoh_core::protocol::ProposeSpecChange {
                reason: input.payload["text"]
                    .as_str()
                    .ok_or_else(|| DeliveryError::Lost("CHANGE has no text".into()))?
                    .to_owned(),
                spec_sha256: digest,
            }),
            files,
        })
    } else {
        None
    };
    let prepared = shared
        .store
        .change_input(ChangeInput::Prepare {
            row: row.clone(),
            request,
        })
        .await
        .map_err(delivery_error)?;
    if let Some(reply) = prepared.reply {
        return Ok(reply);
    }
    let snapshot = prepared
        .publication
        .ok_or_else(|| DeliveryError::Lost("CHANGE has no publication".into()))?;
    let root = shared.root.clone();
    super::begun::spawn_blocking(move || super::spec::publish(&root, &snapshot))
        .await
        .map_err(|e| DeliveryError::Lost(e.to_string()))?
        .map_err(DeliveryError::Lost)?;
    shared
        .store
        .change_input(ChangeInput::Published { row: row.clone() })
        .await
        .map_err(delivery_error)?
        .reply
        .ok_or_else(|| DeliveryError::Lost("CHANGE has no receipt".into()))
}
