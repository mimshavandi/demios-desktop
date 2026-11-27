//! dn-runtime: the agent runtime every surface uses (ADR-0020).
//!
//! - [`assemble`]: the context assembler. Every model call's prompt is put together here, in
//!   the fixed slot order, within the model's real budget (its own tokenizer locally, a
//!   calibrated estimate remotely), with a record of what was left out.
//! - [`policy`]: per-surface policies.
//! - [`estimate`]: token estimates for models that can't count.
//!
//! Later stages add compaction, memory and retrieval (ADR-0020 rollout); the surfaces move
//! onto it in R1 (chat, `dn chat`, the raw callers).

pub mod assemble;
pub mod estimate;
pub mod policy;

pub use assemble::{AssembleError, Assembled, Item, Parts, assemble};
pub use estimate::Estimator;
pub use policy::{Overflow, Policy};

#[cfg(test)]
mod tests;
