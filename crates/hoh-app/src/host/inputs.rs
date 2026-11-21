//! Durable role input routing, isolated from control so a slow turn cannot hold pause/cancel.
use super::{
    control::Inflight,
    http::Shared,
    runner::{Executor, TickError, no_run},
};
use hoh_core::delivery::{Clock, DeliveryError, Dispatcher, Transport};
use hoh_core::persistence::{InputOutcome, InputRouting, OutboxRow, PersistError};
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;

pub(super) async fn run(
    shared: Arc<Shared>,
    clock: Arc<dyn Clock>,
    executor: Option<Arc<dyn Executor>>,
    inflight: Inflight,
    mut stop: watch::Receiver<bool>,
) {
    let mut watermark = shared.store.watermark();
    loop {
        if *stop.borrow() {
            break;
        }
        tokio::select! {
            biased;
            _=stop.changed()=>break,
            outcome=tick(&shared,clock.as_ref(),executor.as_deref(),&inflight)=> {
                if let Err(e)=outcome {eprintln!("hoh host: input delivery: {e}");}
            }
        }
        tokio::select! {_=stop.changed()=>break,_=watermark.changed()=>{},_=tokio::time::sleep(Duration::from_millis(200))=>{}}
    }
}
fn delivery_error(error: PersistError) -> DeliveryError {
    match error {
        PersistError::Held { run, state } => DeliveryError::Held { run, state },
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
        let reply = if row.recipient_role == "commander" {
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
async fn tick(
    shared: &Arc<Shared>,
    clock: &dyn Clock,
    executor: Option<&dyn Executor>,
    inflight: &Inflight,
) -> Result<(), TickError> {
    let store = &shared.store;
    no_run(store.settle_ended_inputs(&shared.project_id).await)?;
    let root = shared.root.clone();
    let config = super::begun::spawn_blocking(move || hoh_core::config::load(&root))
        .await
        .map_err(|e| TickError::Lost(format!("input delivery configuration: {e}")))?
        .map_err(|e| TickError::Lost(format!("input delivery configuration: {e}")))?;
    let transport = HostTransport {
        shared,
        clock,
        executor,
        inflight,
    };
    no_run(
        Dispatcher {
            store,
            clock,
            transport: &transport,
            roles: config.team.keys().cloned().collect(),
            max_attempts: i64::from(config.delivery.max_delivery_attempts),
            batch: 32,
        }
        .tick()
        .await,
    )?;
    Ok(())
}
