//! Frozen candidates and their checks in the product (REQ-052; docs/adr/ADR-027.md decisions 1, 3 to 8): the freeze
//! and check drivers over git, the files and the store.

pub mod check;
pub mod environment;
pub mod freeze;
pub mod rundir;
