use super::*;

impl Runtime {
    pub(super) fn card(&mut self, store: &mut Store, row: &mut Row, now: i64) -> Result<()> {
        let mut team = store
            .team(&row.team)?
            .ok_or_else(|| invalid("Team disappeared"))?;
        if row.card_done {
            // The kept card stands while the row's present state keeps it
            // (`kept::stands`): open with the list the row gives now, the
            // person's dismissal from that list, or a Retry the row reads.
            // Otherwise the current card opens; an open one is closed first,
            // and a list never changes in place.
            let kept = match row.card_id {
                Some(id) => store.card(&row.team, id)?,
                None => None,
            };
            let Some(kept) = kept else {
                return Ok(());
            };
            if super::super::kept::stands(&kept, &decisions(row), reads(row)) {
                return Ok(());
            }
            if kept.state == "open" {
                apply(
                    store,
                    &team,
                    Operation::CloseCard {
                        card: kept.seq,
                        reason: "The launch's state changed what this card can take.".into(),
                    },
                    &mut row.close_request,
                    now,
                )?;
            }
            row.card_id = None;
            row.card_request = None;
            row.card_done = false;
            self.save(row, now)?;
            team = store
                .team(&row.team)?
                .ok_or_else(|| invalid("Team disappeared"))?;
        }
        if row.card_request.is_none() {
            let problem = row
                .problem
                .as_ref()
                .ok_or_else(|| invalid("Blocked launch has no reason"))?;
            let detail = cut(&problem.message, 2048);
            let timing = if row.state.waits_on_backoff() {
                if problem.code == code::NO_ACCOUNT_AVAILABLE && !problem.refs.is_empty() {
                    " Automatic retry follows that account window reset. Choosing Retry checks sooner, on the next service pass (60-second safety interval)."
                } else {
                    " Automatic retry uses a backoff from one to fifteen minutes. Choosing Retry checks sooner, on the next service pass (60-second safety interval)."
                }
            } else {
                ""
            };
            let subject = if row.member == crate::desktop::teams::ledger::BUDGET {
                "Team member creation".into()
            } else {
                format!("Member {}", row.member)
            };
            let text = if problem.detail.as_deref() == Some(crate::teams::OLDER_DEEPSEEK_HOST) {
                let action = if row.member == LEAD {
                    "stop this team and create a new one"
                } else {
                    "remove this member and add a replacement"
                };
                format!("{subject} needs a DeepSeek host with Agent Teams support. Stop idle DeepSeek sessions or restart the host yourself, then {action} once the host is back, since Zerus will not retry this start.")
            } else {
                format!(
                    "{}: {}. {} {}{}",
                    subject,
                    problem.code,
                    detail,
                    problem_refs(problem),
                    timing
                )
            };
            row.card_request = Some(Request {
                request_id: uuid::Uuid::new_v4().to_string(),
                team: row.team.clone(),
                expected_revision: team.revision,
                operation: Operation::OpenCard {
                    parameters: Some(parameters(row, problem)),
                    kind: if row.member == crate::desktop::teams::ledger::BUDGET {
                        "launch_capacity"
                    } else if problem.code == code::DELIVERY_UNCERTAIN {
                        "launch_uncertain"
                    } else {
                        "launch_blocked"
                    }
                    .into(),
                    text: cut(&text, 3072).into(),
                    node: None,
                    decisions: Some(decisions(row)),
                },
            });
            self.save(row, now)?;
        }
        // The saved request keeps its pre-change shape: its parameters are
        // never stored (zerus-63rru.34, F1). They are derived here, at write
        // time, from the ledger record's own member and code, also when a
        // request saved before a restart is replayed.
        let mut request = row.card_request.as_ref().expect("reserved above").clone();
        if let Operation::OpenCard {
            parameters: current,
            ..
        } = &mut request.operation
        {
            if current.is_none() {
                if let Some(problem) = row.problem.as_ref() {
                    *current = Some(parameters(row, problem));
                }
            }
        }
        let actor = Actor::Service {
            person: row.person.clone(),
            team: row.team.clone(),
        };
        match ops::apply(store, &actor, &request, now) {
            Ok(reply) => {
                row.card_done = true;
                row.card_id = reply.item;
            }
            Err(p) if p.code == code::STALE_REVISION => row.card_request = None,
            Err(p) => return Err(p),
        }
        self.save(row, now)
    }
}

/// The parameters a window substitutes for this row's card. The team-wide
/// capacity notice of the budget marker carries none: it is not a member
/// (zerus-63rru.34, F5). Derived at write time from the row's saved member and
/// the blocked launch's code; never stored inside the saved request.
fn parameters(row: &Row, problem: &Problem) -> Vec<(String, String)> {
    if row.member == crate::desktop::teams::ledger::BUDGET {
        Vec::new()
    } else {
        let mut parameters = vec![
            ("member".to_owned(), row.member.clone()),
            ("code".to_owned(), problem.code.clone()),
        ];
        if problem.detail.as_deref() == Some(crate::teams::OLDER_DEEPSEEK_HOST) {
            parameters.push(("reason".into(), crate::teams::OLDER_DEEPSEEK_HOST.into()));
        }
        parameters
    }
}

/// The decisions a launch card lists. Retry, which the budget row reads on
/// every pass and a member row while it waits on backoff, comes first, as the
/// card's primary decision; acknowledged follows and only closes the card,
/// which then stays closed while the row stays and its automatic backoff goes
/// on. Otherwise nothing reads a decision, and the card takes acknowledged
/// alone.
fn decisions(row: &Row) -> Vec<String> {
    if retry_read(row) {
        vec!["retry".to_owned(), "acknowledged".to_owned()]
    } else {
        vec!["acknowledged".to_owned()]
    }
}

/// The decisions the row reads now: Retry on the budget row and while a
/// member row waits on backoff (`due`), none otherwise.
fn reads(row: &Row) -> &'static [&'static str] {
    if retry_read(row) {
        &["retry"]
    } else {
        &[]
    }
}

fn retry_read(row: &Row) -> bool {
    row.member == crate::desktop::teams::ledger::BUDGET || row.state.waits_on_backoff()
}

fn problem_refs(problem: &Problem) -> String {
    if crate::desktop::teams::ledger::is_budget(problem) {
        return String::new();
    }
    if problem.code == code::NO_ACCOUNT_AVAILABLE {
        if let Some((seconds, time)) = problem
            .refs
            .first()
            .and_then(|s| s.parse::<i64>().ok())
            .and_then(|s| chrono::DateTime::from_timestamp(s, 0).map(|t| (s, t)))
        {
            return format!(
                "Next account window resets at {} ({seconds}).",
                time.to_rfc3339()
            );
        }
    }
    problem.refs.join(", ")
}

pub(super) fn cut(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
