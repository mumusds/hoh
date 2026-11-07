//! Outbox delivery (REQ-022; docs/adr/ADR-018.md decision 4). One [`Dispatcher::tick`] takes the due messages of the
//! roles with a consumer, hands each to the [`Transport`] and settles it; it never sleeps (the caller waits until the
//! next due time, the tests move a fake clock). The clock, the transport and the consumers are ports.

use crate::persistence::{ConsumeEffect, OutboxRow, PersistError, Settlement, Store};
use crate::protocol::{self, Message};
use std::collections::BTreeMap;
use std::future::Future;
use std::time::SystemTime;

/// Where the time comes from.
pub trait Clock: Send + Sync {
    /// The time timers are judged by: retry waits, leases (REQ-042 behaviour 2: monotonic within a host's life).
    fn now(&self) -> SystemTime;

    /// The wall-clock time absolute deadlines (`deadline_at`) are judged by; [`Clock::now`] unless a clock tells them
    /// apart.
    fn wall(&self) -> SystemTime {
        self.now()
    }
}

/// The product host's clock (REQ-042 behaviour 2, docs/adr/ADR-020.md decision 4): the system time when it was made,
/// moved on by the monotonic clock, so a step of the system clock while the host runs neither hastens nor delays a
/// wait; a host started again anchors anew from the system time, so a persisted UTC time is judged afresh. Absolute
/// deadlines follow the system clock ([`Clock::wall`]).
#[derive(Debug, Clone, Copy)]
pub struct MonotonicClock {
    anchor: SystemTime,
    started: std::time::Instant,
}

impl MonotonicClock {
    pub fn new() -> MonotonicClock {
        MonotonicClock::anchored(SystemTime::now())
    }

    /// A clock that reads `anchor` now and moves on monotonically from it.
    pub fn anchored(anchor: SystemTime) -> MonotonicClock {
        MonotonicClock {
            anchor,
            started: std::time::Instant::now(),
        }
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        MonotonicClock::new()
    }
}

impl Clock for MonotonicClock {
    fn now(&self) -> SystemTime {
        self.anchor + self.started.elapsed()
    }

    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Why a delivery did not get its answer back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryError {
    /// No answer reached the sender: it is tried again (a lost ACK is found by the receipt).
    Lost(String),
    /// The row holds no valid message: it is recorded and dead at once.
    Invalid(String),
}

/// How a message reaches its consumer; the answer is the reply exactly as the consumer stored it.
pub trait Transport: Send + Sync {
    fn deliver(
        &self,
        row: &OutboxRow,
    ) -> impl Future<Output = Result<String, DeliveryError>> + Send;
}

/// What a role does with a message that passes its fences (the product's qa and reviewer consumers come with
/// M2-T16); applied in the transaction of the receipt.
pub trait Consumer: Send + Sync {
    fn effect(&self, message: &Message) -> ConsumeEffect;
}

/// Delivery within this process: the row's payload is decoded as a Message and consumed by its role's consumer.
pub struct LocalTransport<'a, C: Clock> {
    store: &'a Store,
    clock: &'a C,
    consumers: BTreeMap<String, Box<dyn Consumer + 'a>>,
}

impl<C: Clock> std::fmt::Debug for LocalTransport<'_, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalTransport")
            .field("roles", &self.roles())
            .finish_non_exhaustive()
    }
}

impl<'a, C: Clock> LocalTransport<'a, C> {
    pub fn new(store: &'a Store, clock: &'a C) -> Self {
        LocalTransport {
            store,
            clock,
            consumers: BTreeMap::new(),
        }
    }

    /// Register `consumer` for `role`.
    pub fn with(mut self, role: &str, consumer: impl Consumer + 'a) -> Self {
        self.consumers.insert(role.to_owned(), Box::new(consumer));
        self
    }

    /// The roles with a consumer.
    pub fn roles(&self) -> Vec<String> {
        self.consumers.keys().cloned().collect()
    }
}

/// `value` as an HCP message: a valid Message whose payload is valid for its kind and agrees with its envelope
/// ([`Message::check_payload`]).
pub fn decode_message(value: &serde_json::Value) -> Result<Message, String> {
    let message: Message = protocol::decode(value).map_err(|e| e.to_string())?;
    message.check_payload()?;
    Ok(message)
}

impl<C: Clock> Transport for LocalTransport<'_, C> {
    async fn deliver(&self, row: &OutboxRow) -> Result<String, DeliveryError> {
        let message: Message = match decode_message(&row.payload) {
            Ok(message) => message,
            Err(violations) => {
                self.store
                    .record_invalid_message(
                        &row.recipient_role,
                        &row.message_id,
                        row.run_id.as_deref(),
                        &violations,
                    )
                    .await
                    .map_err(|e| DeliveryError::Lost(e.to_string()))?;
                return Err(DeliveryError::Invalid(violations));
            }
        };
        let Some(consumer) = self.consumers.get(&row.recipient_role) else {
            return Err(DeliveryError::Lost(format!(
                "no consumer for {}",
                row.recipient_role
            )));
        };
        let effect = consumer.effect(&message);
        self.store
            .consume(&row.recipient_role, message, effect, self.clock.now())
            .await
            .map(|consumed| consumed.result_json)
            .map_err(|e| DeliveryError::Lost(e.to_string()))
    }
}

/// What one tick did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tick {
    pub delivered: Vec<String>,
    pub failed: Vec<String>,
    pub invalid: Vec<String>,
}

/// Takes, delivers and settles outbox messages of `roles`, at most `batch` a tick, up to `max_attempts` deliveries
/// each (`delivery.max_delivery_attempts`, REQ-018).
pub struct Dispatcher<'a, C: Clock, T: Transport> {
    pub store: &'a Store,
    pub clock: &'a C,
    pub transport: &'a T,
    pub roles: Vec<String>,
    pub max_attempts: i64,
    pub batch: usize,
}

impl<C: Clock, T: Transport> std::fmt::Debug for Dispatcher<'_, C, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dispatcher")
            .field("roles", &self.roles)
            .field("max_attempts", &self.max_attempts)
            .field("batch", &self.batch)
            .finish_non_exhaustive()
    }
}

impl<C: Clock, T: Transport> Dispatcher<'_, C, T> {
    /// Up to `batch` deliveries, each taken just before it is handed to the transport (so a cancelled tick costs no
    /// delivery that never started) and settled with its generation.
    pub async fn tick(&self) -> Result<Tick, PersistError> {
        let mut tick = Tick::default();
        for _ in 0..self.batch {
            let Some(row) = self
                .store
                .claim_outbox(self.clock.now(), self.roles.clone(), self.max_attempts)
                .await?
                .and_then(crate::persistence::Claimed::start)
            else {
                break;
            };
            let settlement = match self.transport.deliver(&row).await {
                Ok(_) => {
                    tick.delivered.push(row.message_id.clone());
                    Settlement::Delivered
                }
                Err(DeliveryError::Lost(e)) => {
                    tick.failed.push(row.message_id.clone());
                    Settlement::Failed(e)
                }
                Err(DeliveryError::Invalid(e)) => {
                    tick.invalid.push(row.message_id.clone());
                    Settlement::Invalid(e)
                }
            };
            self.store
                .settle_outbox(
                    &row.message_id,
                    row.attempt_count,
                    settlement,
                    self.max_attempts,
                )
                .await?;
        }
        Ok(tick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn the_hosts_clock_moves_on_monotonically_from_its_anchor_and_not_with_the_system_time() {
        // REQ-042 behaviour 2 (M2T19R-06): its time is its anchor plus the monotonic time since, whatever the system
        // clock says meanwhile; absolute deadlines still read the system clock.
        let clock = MonotonicClock::anchored(UNIX_EPOCH);
        let first = clock
            .now()
            .duration_since(UNIX_EPOCH)
            .expect("after its anchor");
        std::thread::sleep(Duration::from_millis(20));
        let second = clock
            .now()
            .duration_since(UNIX_EPOCH)
            .expect("after its anchor");
        assert!(first < Duration::from_secs(1), "{first:?}");
        assert!(
            second >= first + Duration::from_millis(20),
            "{first:?} {second:?}"
        );
        let wall = clock.wall().duration_since(UNIX_EPOCH).expect("now");
        assert!(wall > Duration::from_secs(1_700_000_000), "{wall:?}");
    }
}
