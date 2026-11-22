use super::*;
use crate::desktop::teams::items::kind::LAUNCH_FAILING;
use crate::desktop::teams::items::Failure as ItemFailure;

/// The failures a launch row must reach before its service item is written:
/// the retry table's 15-minute cap (zerus-63rru.24 and zerus-63rru.63).
pub(super) fn at_cap(failures: u32) -> bool {
    backoff(failures) >= 15 * MINUTE
}
const MINUTE: i64 = 60_000;
impl Runtime {
    pub(super) fn cached_accounts(
        &mut self,
        effects: &mut impl Effects,
        agent: &str,
    ) -> Result<Vec<zerus_team_plan::accounts::AccountUsage>> {
        self.accounts
            .entry(agent.into())
            .or_insert_with(|| effects.accounts(agent))
            .clone()
    }
    pub(super) fn retry(
        &mut self,
        store: &mut Store,
        row: &mut Row,
        problem: Problem,
        now: i64,
    ) -> Result<()> {
        row.state = State::Retry;
        self.failure(store, row, problem, now, true)
    }
    pub(super) fn failure(
        &mut self,
        store: &mut Store,
        row: &mut Row,
        problem: Problem,
        now: i64,
        notify: bool,
    ) -> Result<()> {
        self.admitted = None;
        let requested = std::mem::take(&mut row.retry_seen);
        row.problem = Some(bounded_problem(problem));
        row.failures = row.failures.saturating_add(1);
        let reset = row
            .problem
            .as_ref()
            .filter(|p| p.code == code::NO_ACCOUNT_AVAILABLE)
            .and_then(|p| p.refs.first())
            .and_then(|s| s.parse::<i64>().ok())
            .map(|s| s.saturating_mul(1000));
        row.retry_at = reset
            .unwrap_or(now.saturating_add(backoff(row.failures)))
            .max(now.saturating_add(MINUTE));
        self.save(row, now)?;
        if notify || requested || row.failures >= 3 {
            self.card(store, row, now)?;
        }
        Ok(())
    }
    pub(super) fn due(&mut self, store: &Store, row: &mut Row, now: i64) -> Result<bool> {
        if !row.retry_seen {
            if let Some(id) = row.card_id {
                if store
                    .card(&row.team, id)?
                    .is_some_and(|c| c.decision.as_deref() == Some("retry"))
                {
                    // The old card has a durable person decision. Clear its
                    // outbox once, before any retryable state's next check.
                    row.card_id = None;
                    row.card_request = None;
                    row.card_done = false;
                    row.retry_seen = true;
                    row.retry_at = now;
                    self.save(row, now)?;
                    return Ok(true);
                }
            }
        }
        Ok(now >= row.retry_at)
    }
    fn close_open_card(
        store: &mut Store,
        team: &Team,
        row: &mut Row,
        reason: &str,
        required_kind: Option<&str>,
        now: i64,
    ) -> Result<()> {
        if let Some(card) = row.card_id {
            if store.card(&row.team, card)?.is_some_and(|c| {
                c.state == "open" && required_kind.is_none_or(|kind| c.kind == kind)
            }) {
                apply(
                    store,
                    team,
                    Operation::CloseCard {
                        card,
                        reason: reason.into(),
                    },
                    &mut row.close_request,
                    now,
                )?;
            }
        }
        Ok(())
    }
    pub(super) fn close_stale(
        &mut self,
        store: &mut Store,
        team: &Team,
        row: &mut Row,
        now: i64,
    ) -> Result<()> {
        Self::close_open_card(
            store,
            team,
            row,
            "The launch precondition recovered; this card is stale.",
            None,
            now,
        )?;
        row.card_id = None;
        row.card_request = None;
        row.card_done = false;
        row.retry_seen = false;
        row.problem = None;
        row.failures = 0;
        row.retry_at = 0;
        // Best-effort: the caller's save must never wait on the items file
        // (zerus-63rru.24, round 2); the next pass's sweep clears a
        // lingering item.
        let _ = crate::desktop::teams::items::clear_at(
            &self.state,
            &row.team,
            LAUNCH_FAILING,
            &row.member,
        );
        Ok(())
    }
    pub(super) fn record_failure(
        &mut self,
        store: &mut Store,
        id: &str,
        error: Problem,
        now: i64,
    ) -> Result<()> {
        let Some((team, member)) = self.selected.clone().filter(|(t, _)| t == id) else {
            return Ok(());
        };
        let Some(mut row) = self.ledger().get(&team, &member)? else {
            return Ok(());
        };
        // An unknown effect intent must be handled as interrupted on the next
        // pass, never changed into retryable pre-effect preparation.
        if matches!(
            row.state,
            State::WorktreeIntent | State::LaunchIntent | State::BranchIntent | State::ParentIntent
        ) {
            return Ok(());
        }
        self.failure(store, &mut row, error, now, false)
    }
    pub(super) fn note_removed(
        &mut self,
        store: &mut Store,
        team: &Team,
        member: &Member,
        now: i64,
    ) -> Result<()> {
        self.selected = None;
        if let Some(mut row) = self.ledger().get(&team.id, &member.id)? {
            self.selected = Some((row.team.clone(), row.member.clone()));
            if self.owns(&row) {
                self.admitted = None;
            }
            if row.state == State::Blocked {
                // A blocked ledger row can still hold an unknown native
                // launch. Leave that warning open for acknowledgement.
                Self::close_open_card(
                    store,
                    team,
                    &mut row,
                    "Member removed",
                    Some("launch_blocked"),
                    now,
                )?;
            }
            if !row.late_after_stop
                && matches!(
                    row.state,
                    State::LaunchIntent | State::Submitted | State::LaunchUnknown
                )
            {
                row.late_after_stop = true;
                self.save(&mut row, now)?;
            }
        }
        Ok(())
    }
    pub(super) fn note_late(&mut self, team: &Team, now: i64) -> Result<()> {
        if self.ledger.is_none() {
            if !self.state.join("teams/member-launch-v1.sqlite3").is_file() {
                return Ok(());
            }
            self.ledger = Some(Ledger::open(&self.state)?);
        }
        let mut after = String::new();
        for _ in 0..2 {
            let page = self.ledger().rows_page(&team.id, &after)?;
            after = page.last().map(|r| r.member.clone()).unwrap_or_default();
            let more = page.len() == 200;
            for mut row in page {
                if !row.late_after_stop
                    && matches!(
                        row.state,
                        State::LaunchIntent | State::Submitted | State::LaunchUnknown
                    )
                {
                    row.late_after_stop = true;
                    self.save(&mut row, now)?;
                }
                // The stop closed the row's card: the item says the failure
                // at once, written once for the row's cap-tier failures.
                // The items file's own trouble never fails the retry
                // bookkeeping (round 2).
                let exists = crate::desktop::teams::items::failures_at(
                    &self.state,
                    &row.team,
                    LAUNCH_FAILING,
                    &row.member,
                )
                .unwrap_or(None)
                .is_some();
                if at_cap(row.failures) && !exists {
                    let problem = row
                        .problem
                        .as_ref()
                        .map(|p| format!("{}: {}", p.code, p.message))
                        .unwrap_or_else(|| "The launch keeps failing".into());
                    let _ = crate::desktop::teams::items::put_at(
                        &self.state,
                        &ItemFailure {
                            team: &row.team,
                            kind: LAUNCH_FAILING,
                            subject: &row.member,
                            node: None,
                            reason: &problem,
                            failures: row.failures,
                            now,
                            next_ms: Some(row.retry_at),
                        },
                    );
                } else if !at_cap(row.failures) && exists {
                    // The row succeeded below the cap and its item lingers
                    // from a failed clear: removed now.
                    let _ = crate::desktop::teams::items::clear_at(
                        &self.state,
                        &row.team,
                        LAUNCH_FAILING,
                        &row.member,
                    );
                }
            }
            if !more {
                break;
            }
        }
        Ok(())
    }
}
pub(super) fn backoff(failures: u32) -> i64 {
    MINUTE
        .saturating_mul(1i64 << failures.saturating_sub(1).min(4))
        .min(15 * MINUTE)
}
fn bounded_problem(mut p: Problem) -> Problem {
    p.message = super::card::cut(&p.message, 4096).into();
    p.refs = p
        .refs
        .into_iter()
        .take(8)
        .map(|r| super::card::cut(&r, 256).into())
        .collect();
    p
}
