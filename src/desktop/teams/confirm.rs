//! The service alone applies the team's folder-trust and self-exit delegations.
//! Every original is reserved durably and never reconstructed from saved data.
//! A person quitting the agent in its terminal looks like a clean exit and is
//! resumed. Use Stop member to stop it; three resumes per hour bound a tug of war.
//! Final admission and spawn share one immediate team-store transaction. A
//! revocation or pause committed before it is taken prevents spawning; writers
//! waiting behind it commit after admission. The audit reservation already exists.
//! The transaction is released as soon as spawn returns, never while waiting for
//! the child. Lock waits are bounded to 100 ms; a durable definite non-start is
//! retryable, while a crash before that record is written remains Unknown.
//! Ownership/launch proofs, native record, stop and preview are checked last
//! inside the section. Those external facts retain the window of spawn itself.
mod engine;
mod ledger;
mod native;
mod policy;
mod ports;
mod scan;
mod types;
pub(super) use scan::{step, Scan};
mod resumed;
#[allow(unused_imports)] // The agreed package-8 availability port consumes this seam.
pub(super) use resumed::{resumed_binding, ResumedBinding};

#[cfg(test)]
mod lead151;
#[cfg(test)]
mod test_inspect;
