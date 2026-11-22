//! A member that a ready task or an open hand-out needs and cannot be read
//! (zerus-63rru.55), or whose terminal holds input (zerus-63rru.79). Unknown is never free, but it is not silent: the held
//! steps' row and timing, under `unreadable:<member>`. The row lasts while a
//! task needs the member and it cannot be read; it ends, with its
//! card, when either stops. A changed fact or task replaces the kept card
//! only once it has stood for the period, and a dismissal lasts while the
//! sentence is the same.
use super::*;
use crate::desktop::teams::actor::ports::Fact;
use crate::desktop::teams::actor::schedule::Needs;

const ROWS: &str = "gate:unreadable:";
const ARCHIVE: &str = "an archive from its session is missing from the archive catalog";

const CONFIRMATION: &str = "unreadable_confirmation:";

pub(super) fn key_is_unreadable(key: &str) -> bool {
    key.starts_with("unreadable:") || key.starts_with(CONFIRMATION)
}

impl Runtime {
    /// Standing confirmation needs no ready task to explain an unreadable end.
    /// Reuse the existing delay, opener receipt, Retry and recovery path.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(in crate::desktop::teams) fn confirmation_observation(
        &mut self,
        store: &mut Store,
        team: &Team,
        member: &Member,
        fault: Option<(&str, &str)>,
        now: i64,
        report: impl FnOnce(&str),
    ) -> Result<()> {
        let key = format!("{CONFIRMATION}{}", member.id);
        let Some((binding, error)) = fault else {
            return self.recovered(store, team, &key, now);
        };
        if self
            .provider_stops
            .get(&(team.id.clone(), member.id.clone()))
            .is_some_and(|stop| stop == binding)
        {
            return Ok(());
        }
        let reason = format!(
            "Member {} ({}) needs attention. {}. The service will check again.",
            member.id, member.role, crate::desktop::teams::reason_text(error).trim_end_matches('.')
        );
        self.for_row(|a| {
            a.hold_scoped(store, team, (&key, 0, 0, Some(binding)), (&reason, now), || report(error))
        })
    }
}

impl Runtime {
    pub(in crate::desktop::teams::actor) fn provider_confirmation(
        &self,
        team: &Team,
        member: &Member,
    ) -> Result<Option<(Row, ops::ProviderConfirmation)>> {
        let Some(row) = self
            .ledger
            .get(&team.id, &format!("gate:{CONFIRMATION}{}", member.id))?
        else {
            return Ok(None);
        };
        let Some((0, 0, Some(binding))) = super::held::held_by(&row) else {
            return Ok(None);
        };
        let gate: Gate =
            serde_json::from_value(row.data.clone()).map_err(crate::teams::store::storage)?;
        let confirmation = ops::ProviderConfirmation {
            binding,
            card: gate.card,
            notice: gate.notice,
        };
        Ok(Some((row, confirmation)))
    }

    pub(in crate::desktop::teams::actor) fn provider_stop_confirmed(
        &mut self,
        store: &mut Store,
        team: &Team,
        member: &Member,
        binding: String,
        confirmation: Option<&(Row, ops::ProviderConfirmation)>,
        now: i64,
    ) -> Result<()> {
        if let Some((row, confirmation)) = confirmation {
            if confirmation.binding == binding {
                // The release closed any committed card for this gate.
                // Keep any newer ledger version written after the snapshot.
                self.ledger.remove(row)?;
            }
        }
        self.provider_stops
            .insert((team.id.clone(), member.id.clone()), binding);
        self.for_row(|a| {
            a.close_gate(
                store,
                team,
                &format!("unreadable:{}", member.id),
                ops::CONFIRMATION_CLOSED,
                now,
            )
        })
    }

    /// A reservation can discover the first-send gate after selection. Keep
    /// that first observation on the same named row the next pass carries.
    pub(in crate::desktop::teams::actor) fn first_message_wait(
        &mut self,
        store: &mut Store,
        team: &Team,
        member: &Member,
        node: u32,
        now: i64,
    ) -> Result<()> {
        let key = format!("unreadable:{}", member.id);
        let reason = sentence(&member.id, &member.role, node, Fact::FirstMessage, true);
        self.for_row(|a| a.hold_write(store, team, (&key, 0, 0), &reason, now))
    }
    /// Ends the rows of the members nobody needs now, except a member whose
    /// read was not due this pass, then holds the needed ones. With no
    /// needs (no ready task or hand-out, or a paused team) every row ends.
    pub(in crate::desktop::teams::actor) fn unreadable_members(
        &mut self,
        store: &mut Store,
        team: &Team,
        needs: &Needs,
        now: i64,
    ) -> Result<()> {
        // A row exists only for a member the pass read, at most the 400 it
        // reads; four pages hold every one of them.
        let mut after = String::new();
        for _ in 0..4 {
            let rows = self.ledger.prefix_page(&team.id, ROWS, &after)?;
            for row in &rows {
                let member = &row.key[ROWS.len()..];
                if !needs.needed.contains_key(member) && !needs.unread(member) {
                    self.recovered(store, team, &format!("unreadable:{member}"), now)?;
                }
            }
            match rows.last() {
                Some(last) if rows.len() == 200 => after = last.key.clone(),
                _ => break,
            }
        }
        for (member, (node, role, fact)) in &needs.needed {
            let key = format!("unreadable:{member}");
            let reason = sentence(
                member,
                role,
                *node,
                fact.clone(),
                needs.holders.contains(member),
            );
            // Catalog recovery ends that hold immediately, even when the
            // archived member still cannot take work until it is restored.
            let generation = u32::from(*fact == Fact::Archive);
            self.for_row(|a| a.hold_write(store, team, (&key, 0, generation), &reason, now))?;
        }
        Ok(())
    }
}

/// The card's sentence. It says only what this build does: what cannot be
/// read, and what the person can do about it with the operations that exist.
pub(in crate::desktop::teams::actor) fn sentence(
    member: &str,
    role: &str,
    node: u32,
    fact: Fact,
    holding: bool,
) -> String {
    if holding {
        let what = match fact {
            Fact::FirstMessage => return format!("Member {member} ({role}) holds an open hand-out of task {node} but is waiting for its first-message readiness. Finish startup in the member's terminal. The service checks readiness again. This card closes when the member can take its hand-out or no longer holds it."),
            Fact::NoHandle => "no durable handle or launch evidence identifies its session",
            Fact::Unavailable => "its session record cannot be read",
            Fact::Archive => ARCHIVE,
            _ => "its binding cannot be resolved",
        };
        return format!("Member {member} ({role}) holds an open hand-out of task {node} but its binding cannot be read: {what}. The service checks this binding again. This card closes when the member can be read or it no longer holds an open hand-out.");
    }
    const FINAL: &str = "This seat cannot be bound again; archive the member.";
    let (what, next) = match fact {
        Fact::FirstMessage => (
            "waiting for its first-message readiness",
            "Finish startup in the member's terminal. The service checks readiness again.",
        ),
        // The member can be read; its terminal is what is not free
        // (zerus-63rru.79). The sentence names the dialog the gate knew
        // (zerus-63rru.83).
        Fact::TerminalWaiting { dialog } => {
            let what = match dialog.as_deref().and_then(crate::state::dialog_clause) {
                Some(clause) => format!("({clause})"),
                None => "(a question, a dialog or an unsent draft)".to_owned(),
            };
            return format!(
                "Member {member} ({role}) is needed by ready task {node}, but its terminal is waiting for input {what}. Answer it or clear it in the member's terminal. This card closes when the member can take work again or no ready task needs it."
            )
        }
        Fact::Children => (
            "its session has reported no subagent count; its hooks have sent no event yet",
            "Wait for its first hook event. If its hooks never report, stop or archive the member.",
        ),
        Fact::ChildrenUnsettled => (
            "the state of some of its subagents cannot be settled",
            "If this lasts, stop or archive the member.",
        ),
        Fact::Questions => (
            "its question index cannot be read",
            "If this lasts, stop or archive the member.",
        ),
        Fact::Background => (
            "its background work cannot be verified from the process table",
            "If this lasts, stop or archive the member.",
        ),
        Fact::NoHandle => (
            "it was bound before session handles, and nothing names its session now",
            FINAL,
        ),
        Fact::Gone => (
            "its session no longer exists here",
            "If it was archived, restore it and the member is read again; otherwise archive the member.",
        ),
        Fact::Archive => (
            ARCHIVE,
            "The service requests catalog repair at most once every five minutes on this computer.",
        ),
        Fact::Changed => (
            "its session now belongs to another launch, agent or folder",
            FINAL,
        ),
        Fact::StartedFresh => (
            "its session was started fresh, and a new conversation does not take over its tasks",
            FINAL,
        ),
        Fact::Unavailable => (
            "its session record did not answer in time",
            "Retry reads it again on the next pass.",
        ),
    };
    format!(
        "Member {member} ({role}) is needed by ready task {node} but cannot be read: {what}. {next} This card closes when the member can be read or no ready task needs it."
    )
}
