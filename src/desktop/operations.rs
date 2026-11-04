//! Dormant G2 policy/ledger bridge. Frozen originals and proposals are data,
//! never current admission, a reservation, dispatch or resource-drain proof.
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use zerus_client_core::{creation::canonical_uuid, operation_recovery::task_start as policy};
use zerus_desktop_ledger::operations as ledger;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Refused {
    Policy(policy::Review),
    Identity,
    Metadata,
    Store,
}

// Refuse before growing beyond the metadata budget. Errors contain no source,
// reply, provenance, SQL diagnostics or other private bodies.
fn encoded(value: &impl Serialize) -> Result<Vec<u8>, Refused> {
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > ledger::METADATA_BYTES - self.0.len() {
                return Err(std::io::Error::other("metadata limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Bounded(Vec::new());
    serde_json::to_writer(&mut out, value).map_err(|_| Refused::Metadata)?;
    Ok(out.0)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

// Exhaustive field/variant matches make changes to either closed schema a
// compile-time integration decision, rather than a permissive JSON projection.
fn to_ledger(binding: &policy::Binding) -> ledger::Binding {
    let policy::Binding {
        key,
        marker_id,
        source_record_sha256,
        ui_snapshot_sha256,
        task_start_reconcile_input_v1_sha256,
        executor,
    } = binding;
    let policy::Key {
        kind,
        schema,
        configuration,
        project,
        task,
        request_id,
    } = key;
    let policy::Executor {
        machine,
        session_id,
        run_id,
        conversation_id,
    } = executor;
    ledger::Binding {
        key: ledger::Key {
            kind: match kind {
                policy::Kind::TaskStartReconcile => ledger::Kind::TaskStartReconcile,
            },
            schema: *schema,
            configuration: configuration.clone(),
            project: project.clone(),
            task: task.clone(),
            request_id: request_id.clone(),
        },
        marker_id: marker_id.clone(),
        source_record_sha256: source_record_sha256.clone(),
        ui_snapshot_sha256: ui_snapshot_sha256.clone(),
        task_start_reconcile_input_v1_sha256: task_start_reconcile_input_v1_sha256.clone(),
        executor: ledger::Executor {
            machine: machine.clone(),
            session_id: session_id.clone(),
            run_id: run_id.clone(),
            conversation_id: conversation_id.clone(),
        },
    }
}
fn to_policy(binding: &ledger::Binding) -> policy::Binding {
    let ledger::Binding {
        key,
        marker_id,
        source_record_sha256,
        ui_snapshot_sha256,
        task_start_reconcile_input_v1_sha256,
        executor,
    } = binding;
    let ledger::Key {
        kind,
        schema,
        configuration,
        project,
        task,
        request_id,
    } = key;
    let ledger::Executor {
        machine,
        session_id,
        run_id,
        conversation_id,
    } = executor;
    policy::Binding {
        key: policy::Key {
            kind: match kind {
                ledger::Kind::TaskStartReconcile => policy::Kind::TaskStartReconcile,
            },
            schema: *schema,
            configuration: configuration.clone(),
            project: project.clone(),
            task: task.clone(),
            request_id: request_id.clone(),
        },
        marker_id: marker_id.clone(),
        source_record_sha256: source_record_sha256.clone(),
        ui_snapshot_sha256: ui_snapshot_sha256.clone(),
        task_start_reconcile_input_v1_sha256: task_start_reconcile_input_v1_sha256.clone(),
        executor: policy::Executor {
            machine: machine.clone(),
            session_id: session_id.clone(),
            run_id: run_id.clone(),
            conversation_id: conversation_id.clone(),
        },
    }
}

/// Private exact original; intentionally neither Debug, Serialize nor Clone.
pub(super) struct VerifiedOriginal {
    original: policy::Original,
    binding: ledger::Binding,
    raw: Vec<u8>,
    input: Vec<u8>,
    provenance: Value,
}
impl VerifiedOriginal {
    pub(super) fn verify(
        binding: policy::Binding,
        raw: &[u8],
        input: &[u8],
        provenance: &Value,
    ) -> Result<Self, Refused> {
        let original = policy::Original::verify(binding, raw, input).map_err(Refused::Policy)?;
        let binding = to_ledger(original.binding());
        encoded(&binding)?;
        encoded(provenance)?;
        Ok(Self {
            original,
            binding,
            raw: raw.to_vec(),
            input: input.to_vec(),
            provenance: provenance.clone(),
        })
    }

    /// Constructs only the existing storage request DATA. This module never
    /// calls admit/reserve or supplies a current guard for those actor calls.
    pub(super) fn admission<'a>(
        &'a self,
        admission_id: &'a str,
        writer_epoch: &'a str,
        service_epoch: &'a str,
    ) -> Result<ledger::Admission<'a>, Refused> {
        if ![admission_id, writer_epoch, service_epoch]
            .into_iter()
            .all(canonical_uuid)
        {
            return Err(Refused::Identity);
        }
        Ok(ledger::Admission {
            admission_id,
            binding: &self.binding,
            raw_record: &self.raw,
            retained_input: &self.input,
            provenance: &self.provenance,
            writer_epoch,
            service_epoch,
        })
    }
    pub(super) fn input_sha256(&self) -> String {
        hash(&self.input)
    }
    pub(super) fn consent_sha256(&self) -> String {
        ledger::consent_sha256(&self.input)
    }
}

/// One revalidated stored original, still without current runtime authority.
pub(super) struct Loaded {
    verified: VerifiedOriginal,
    admitted: ledger::Admitted,
}
impl Loaded {
    pub(super) fn load(db: &Connection, expected: &ledger::Admitted) -> Result<Self, Refused> {
        let frozen = ledger::load(db, &expected.operation_id).map_err(|_| Refused::Store)?;
        if frozen.admitted != *expected
            || !canonical_uuid(&frozen.admitted.admission_id)
            || !(1..=i64::MAX as u64).contains(&frozen.admitted.revision)
        {
            return Err(Refused::Identity);
        }
        let verified = VerifiedOriginal::verify(
            to_policy(&frozen.binding),
            &frozen.raw_record,
            &frozen.retained_input,
            &frozen.provenance,
        )?;
        if verified
            .binding
            .key
            .operation_id()
            .map_err(|_| Refused::Identity)?
            != expected.operation_id
            || hash(&encoded(&verified.binding)?) != expected.binding_sha256
        {
            return Err(Refused::Identity);
        }
        // Frozen exposes no stored plan/consent columns. Their derivations are
        // exact input bytes above; SQL assertions qualify those stored columns.
        Ok(Self {
            verified,
            admitted: frozen.admitted,
        })
    }

    pub(super) fn proposal(&self, cancelled: bool) -> Result<policy::Proposal, Refused> {
        match self
            .verified
            .original
            .step(policy::Input::LookupRequested, cancelled)
        {
            policy::Decision::Proposed(proposal) => Ok(proposal),
            policy::Decision::NeedsReview(reason) => Err(Refused::Policy(reason)),
            policy::Decision::SettledSubmitted { .. } => Err(Refused::Identity),
        }
    }

    /// Settles only an existing complete ticket. The ledger matches attempt,
    /// epochs and lease too; an unchanged old-epoch reserved result can finish
    /// after revoke. This never reserves, dispatches or releases resources.
    pub(super) fn finish_submitted(
        &self,
        db: &mut Connection,
        ticket: &ledger::Ticket,
        reply: &[u8],
    ) -> Result<(), Refused> {
        if ticket.operation_id != self.admitted.operation_id
            || ticket.admission_id != self.admitted.admission_id
            || ticket.binding_sha256 != self.admitted.binding_sha256
            || ticket.revision != self.admitted.revision
        {
            return Err(Refused::Identity);
        }
        let (binding, receipt) = match self
            .verified
            .original
            .step(policy::Input::Reply(reply), false)
        {
            policy::Decision::SettledSubmitted { binding, receipt } => (binding, receipt),
            policy::Decision::NeedsReview(reason) => return Err(Refused::Policy(reason)),
            policy::Decision::Proposed(_) => return Err(Refused::Identity),
        };
        if to_ledger(&binding) != self.verified.binding {
            return Err(Refused::Identity);
        }
        let policy::Receipt {
            schema,
            id,
            project,
            uuid,
            state,
        } = receipt;
        ledger::finish(
            db,
            ticket,
            &ledger::Outcome::Submitted {
                receipt: ledger::Receipt {
                    schema,
                    id,
                    project,
                    uuid,
                    state,
                },
            },
        )
        .map_err(|_| Refused::Store)
    }
}

#[cfg(test)]
mod tests;
