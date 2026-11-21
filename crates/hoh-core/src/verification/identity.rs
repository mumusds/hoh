//! Session identity (CR-029, REQ-053): two records are of the same session when they share a HoH session id, or an
//! (agent, native session id) pair; the same native id string under another agent is no conflict. The one rule the
//! candidate reports' independence (persistence/reports.rs) and the specification review's (REQ-031 behaviour 4,
//! docs/adr/ADR-035.md) both judge by. Pure.

use std::collections::BTreeSet;

/// The identities one side of an independence check holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identities {
    /// HoH session ids.
    pub hoh: BTreeSet<String>,
    /// (agent, native session id) pairs.
    pub native: BTreeSet<(String, String)>,
}

/// One identity two sides share.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Identity {
    Hoh(String),
    Native { agent: String, id: String },
}

impl Identity {
    /// The id it names (a HoH session id or a native one).
    pub fn id(&self) -> &str {
        match self {
            Identity::Hoh(id) | Identity::Native { id, .. } => id,
        }
    }
}

impl std::fmt::Display for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Identity::Hoh(id) => write!(f, "HoH session {id}"),
            Identity::Native { agent, id } => write!(f, "{agent} native session {id}"),
        }
    }
}

impl Identities {
    /// Add HoH session `id`.
    pub fn add_hoh(&mut self, id: &str) {
        self.hoh.insert(id.to_owned());
    }

    /// Add `agent`'s native session `id`.
    pub fn add_native(&mut self, agent: &str, id: &str) {
        self.native.insert((agent.to_owned(), id.to_owned()));
    }

    /// Add `id`, whose kind is not known (a native attempt record's `session_ids` holds HoH and native ids alike), to
    /// both: as a HoH session id and as `agent`'s native id.
    pub fn claim(&mut self, agent: &str, id: &str) {
        self.add_hoh(id);
        self.add_native(agent, id);
    }

    /// Add every identity of `other`.
    pub fn extend(&mut self, other: &Identities) {
        self.hoh.extend(other.hoh.iter().cloned());
        self.native.extend(other.native.iter().cloned());
    }
}

/// The identities `a` and `b` share: equal HoH session ids, and equal (agent, native id) pairs; sorted.
pub fn shared(a: &Identities, b: &Identities) -> Vec<Identity> {
    let hoh = a
        .hoh
        .intersection(&b.hoh)
        .map(|id| Identity::Hoh(id.clone()));
    let native = a
        .native
        .intersection(&b.native)
        .map(|(agent, id)| Identity::Native {
            agent: agent.clone(),
            id: id.clone(),
        });
    hoh.chain(native).collect()
}

#[cfg(test)]
mod tests;
