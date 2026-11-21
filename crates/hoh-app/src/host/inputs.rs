//! Host input delivery (ADR-036): only the routes implemented by this slice are consumed.
//! Other roles and HCP messages keep their existing outbox owner until the later routing slices.

use super::http::Shared;
use super::runner::{TickError, no_run};
use hoh_core::delivery::{Clock, Dispatcher, InputTransport};
use std::sync::Arc;

pub(super) async fn tick(shared: &Arc<Shared>, clock: &dyn Clock) -> Result<(), TickError> {
    let store = &shared.store;
    no_run(store.settle_ended_inputs(&shared.project_id).await)?;
    let root = shared.root.clone();
    let config = super::begun::spawn_blocking(move || hoh_core::config::load(&root))
        .await
        .map_err(|e| TickError::Lost(format!("input delivery configuration: {e}")))?
        .map_err(|e| TickError::Lost(format!("input delivery configuration: {e}")))?;
    let transport = InputTransport { store, clock };
    no_run(
        Dispatcher {
            store,
            clock,
            transport: &transport,
            roles: vec!["commander".to_owned()],
            max_attempts: i64::from(config.delivery.max_delivery_attempts),
            batch: 32,
        }
        .tick()
        .await,
    )?;
    Ok(())
}
