//! Only a newly reserved claim can start an original child. Prepared claims
//! recovered after a restart are Unknown. The sole retryable record is a
//! durably confirmed admission-lock nonstart, atomically claimed again before
//! any spawn; its exact audit has never been offered to a child.
use super::{
    policy,
    ports::{Ports, Result},
    types::*,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

// The exiting supervisor can still own its name during post-archive cleanup.
// Include its sequential automatic deadlines and conservatively the separate
// manual leftover-delete budget, plus a standing safety observation interval.
// Filesystem/receipt I/O has no end-to-end deadline: allow the reviewed ten-minute
// outer wait cap instead when it is larger. This never bounds teardown itself.
const UNBOUNDED_TEARDOWN_WAIT_CAP_MS: i64 = 10 * 60 * 1000;
pub(super) const RESUME_SETTLE_MS: i64 = {
    let bounded = crate::state::AFTER_ARCHIVE_FIXED_TIME
        .saturating_add(crate::state::LEFTOVER_DELETE_TIME)
        .saturating_add(super::super::worker::SAFETY_OBSERVATION_INTERVAL)
        .as_millis() as i64;
    if bounded > UNBOUNDED_TEARDOWN_WAIT_CAP_MS {
        bounded
    } else {
        UNBOUNDED_TEARDOWN_WAIT_CAP_MS
    }
};

pub(super) fn key(parts: &Value) -> String {
    format!("{:x}", Sha256::digest(parts.to_string()))
}

pub(super) fn step(p: &mut impl Ports, b: &Binding, epoch: &str, now: i64) -> Result<()> {
    if let Some(mut row) = p.unsettled(b)? {
        if row.state == "waiting" {
            if let Err(why) = validate(p, &row.plan) {
                row.state = "cancelled".into();
                row.detail = why;
                p.save(&row)?;
                return settle(p, &mut row);
            }
            return execute(p, row.plan, epoch, now);
        }
        if row.state == "prepared" {
            row.state = "unknown".into();
            row.detail =
                "A reserved original survived its service turn; it will never be repeated".into();
            p.save(&row)?;
        }
        let audit_error = recover_audit(p, &mut row).err();
        settle(p, &mut row)?;
        return audit_error.map_or(Ok(()), Err);
    }
    if let Some(brief) = p.pending_brief(b)? {
        let ready = p.observe_member(&brief.binding)?;
        if ready.ready && ready.live && !ready.paused && !ready.ended && ready.questions.is_empty() {
            return execute(p, brief, epoch, now);
        }
    }
    let snapshot = p.observe_member(b)?;
    // A paused or terminating member was stopped by a person: not an exit and
    // not a prompt (zerus-63rru.56). Nothing is planned or claimed.
    if snapshot.paused {
        return Ok(());
    }
    let (kind, question) = if let Some(prompt) = policy::trust_candidate(b, &snapshot) {
        (Kind::Trust, prompt.clone())
    } else if !snapshot.questions.is_empty() {
        // Another pending question is the person's (zerus-63rru.73): no row,
        // no card and no audit reservation here.
        return Ok(());
    } else if snapshot.ended {
        if let Some(wait) = &snapshot.resume_wait {
            if now < wait.until {
                return Ok(());
            }
            let plan = candidate(b, &snapshot, Kind::Resume, Value::Null);
            let refusal = validate(p, &plan).err().unwrap_or_else(|| format!(
                "The original run did not settle: its terminal name is still held. {}", policy::SEAT,
            ));
            return execute_refusal(p, plan, epoch, now, Some(refusal));
        }
        (Kind::Resume, Value::Null)
    } else {
        return Ok(());
    };
    let plan = candidate(b, &snapshot, kind, question);
    execute(p, plan, epoch, now)
}

fn execute(p: &mut impl Ports, plan: Plan, epoch: &str, now: i64) -> Result<()> {
    let refusal = validate(p, &plan).err();
    execute_refusal(p, plan, epoch, now, refusal)
}
fn execute_refusal(p: &mut impl Ports, mut plan: Plan, epoch: &str, now: i64, refusal: Option<String>) -> Result<()> {
    if refusal.is_some() {
        plan.kind = Kind::Notice;
    }
    let claim = p.claim(&plan, epoch, now)?;
    let mut row = claim.record;
    if !claim.fresh {
        if row.state == "prepared" {
            row.state = "unknown".into();
            row.detail =
                "A reserved original survived its service turn; it will never be repeated".into();
            p.save(&row)?;
        }
        return settle(p, &mut row);
    }
    if let Some(reason) = refusal.or_else(|| {
        claim
            .limited
            .then(|| "Three resumes were already reserved in the last hour".into())
    }) {
        row.state = "cancelled".into();
        row.detail = reason;
        p.save(&row)?;
        return settle(p, &mut row);
    }
    if reserve_audit(p, &mut row)? {
        deliver(p, &mut row);
    }
    p.save(&row)?;
    settle(p, &mut row)
}

fn reserve_audit(p: &mut impl Ports, row: &mut Record) -> Result<bool> {
    if row.audit.is_some() {
        // Only a durable, definite non-start can reach a new claim with an audit.
        return Ok(true);
    }
    match p.audit_begin(&row.plan) {
        Ok(audit) => row.audit = Some(audit),
        Err(error) => {
            row.state = "cancelled".into();
            row.detail = "The audit reservation failed before delivery started".into();
            p.save(row)?;
            settle(p, row)?;
            return Err(error);
        }
    }
    p.save(row)?;
    if !row.audit.as_ref().is_some_and(|audit| audit.fresh) {
        row.state = "unknown".into();
        row.detail = "The audit already reserved this effect; it will not be repeated".into();
        return Ok(false);
    }
    Ok(true)
}
fn deliver(p: &mut impl Ports, row: &mut Record) {
    let outcome = p.original(row, &mut |p| validate(p, &row.plan));
    match outcome {
        Outcome::Retry => {
            row.state = "waiting".into();
            row.detail = "Admission lock busy; the original child never started".into();
        }
        Outcome::Approved(target) => {
            row.state = "approved".into();
            row.detail = "The exact native receipt confirmed the original request".into();
            row.result = target.map(|s| *s);
        }
        Outcome::Unknown(why) => {
            row.state = "unknown".into();
            row.detail = why;
        }
        Outcome::NotStarted(why) => {
            row.state = "cancelled".into();
            row.detail = why;
        }
        Outcome::Refused(why) => {
            row.state = "failed".into();
            row.detail = why;
        }
    }
}

fn recover_audit(p: &mut impl Ports, row: &mut Record) -> Result<()> {
    if row.audit.is_none() && row.plan.kind != Kind::Notice {
        // The claim and audit use separate transactions. A crash between the
        // audit commit and recording its ID is recovered by the audit's exact
        // identity, and immediately finished Unknown, never delivered.
        row.audit = Some(p.audit_begin(&row.plan)?);
        p.save(row)?;
    }
    Ok(())
}

fn settle(p: &mut impl Ports, row: &mut Record) -> Result<()> {
    if row.state == "waiting" {
        return Ok(());
    }
    let audit_error = if row.audit.is_some() && !row.audit_done {
        p.audit_finish(row).err()
    } else {
        None
    };
    if audit_error.is_none() && row.audit.is_some() && !row.audit_done {
        row.audit_done = true;
        p.save(row)?;
    }
    if row.state != "approved" && !row.card_done {
        p.card(row)?;
        row.card_done = true;
        p.save(row)?;
    }
    audit_error.map_or(Ok(()), Err)
}

pub(super) fn validate(p: &mut impl Ports, plan: &Plan) -> Result<()> {
    let b = &plan.binding;
    let facts = p.facts(b)?;
    policy::stored(b, &facts)?;
    let snapshot = p.observe(b)?;
    policy::session(b, &snapshot)?;
    policy::ownership(b, &p.ownership(b)?)?;
    if snapshot.run != plan.snapshot.run || snapshot.conversation != plan.snapshot.conversation {
        return Err("The frozen run or conversation changed".into());
    }
    match plan.kind {
        Kind::Trust => {
            if !facts.team.settings.delegations.confirm_trust {
                return Err("Folder-trust delegation is off".into());
            }
            if policy::trust(b, &snapshot)? != plan.question {
                return Err("The exact prompt or its hash changed".into());
            }
        }
        Kind::Resume => {
            if !facts.team.settings.delegations.resume_exited {
                return Err("Resume delegation is off".into());
            }
            policy::resume(&snapshot)?;
            if snapshot.exit_identity != plan.snapshot.exit_identity
                || snapshot.archive != plan.snapshot.archive
            {
                return Err("The exact exit or archive changed".into());
            }
        }
        Kind::Brief => {
            if !policy::brief_granted(plan, &facts)
                || !snapshot.ready
                || !snapshot.live
                || snapshot.paused
                || snapshot.ended
                || !snapshot.questions.is_empty()
            {
                return Err("The resumed member is not ready for its brief".into());
            }
        }
        Kind::Notice => return Err("A notice grants no session effect".into()),
    }
    Ok(())
}

pub(super) fn candidate(b: &Binding, s: &Snapshot, kind: Kind, question: Value) -> Plan {
    let tag = if kind == Kind::Trust {
        "trust"
    } else {
        "resume"
    };
    let key = key(&json!([b.team, b.member, b.launch, s.run, tag]));
    let text = format!("You are a member of team {} in role {}. Read `hgs team current` for your current team and work.", b.team, b.role);
    let question = if kind == Kind::Resume {
        synthetic(b, s, &text, "resume")
    } else {
        question
    };
    Plan {
        key,
        binding: b.clone(),
        snapshot: s.clone(),
        kind,
        question,
        text,
        parent: None,
    }
}

pub(super) fn brief(row: &Record, target: &Snapshot) -> Plan {
    let mut b = row.plan.binding.clone();
    b.run = target.run.clone();
    b.conversation = target.conversation.clone();
    Plan {
        key: key(&json!([
            row.plan.key,
            "brief",
            target.run,
            target.conversation
        ])),
        question: synthetic(&b, target, &row.plan.text, "brief"),
        binding: b,
        snapshot: target.clone(),
        kind: Kind::Brief,
        text: row.plan.text.clone(),
        parent: Some(row.plan.key.clone()),
    }
}

pub(super) fn synthetic(b: &Binding, s: &Snapshot, text: &str, kind: &str) -> Value {
    let hash = key(&json!([
        b.team,
        b.member,
        b.launch,
        s.run,
        s.conversation,
        s.exit_identity,
        kind,
        text
    ]));
    json!({"source":"team_resume","question_id":format!("{kind}:{}",s.run),"question_hash":hash,
        "run_id":s.run,"conversation_id":s.conversation,"can_answer":false,
        "questions":[{"id":"team_resume","question":text,"options":[]}]})
}

#[cfg(test)]
mod tests;

/// The lead gets its orientation once per launch run, including before the
/// first message has assigned a conversation. Identity never uses its name.
pub(super) fn lead_brief_key(b: &Binding, run: &str) -> String {
    key(&json!([b.team, b.member, b.launch, run, "lead_launch_brief"]))
}

pub(super) fn lead_brief(b: &Binding, s: &Snapshot, team: &zerus_team_plan::model::Team) -> Plan {
    let text = format!("You are the lead of team {}. Read `hgs team current` for your team and `hgs team inbox` for items waiting for you.", team.name);
    Plan {
        key: lead_brief_key(b, &s.run),
        binding: b.clone(),
        snapshot: s.clone(),
        kind: Kind::Brief,
        question: synthetic(b, s, &text, "brief"),
        text,
        parent: None,
    }
}
