//! A member whose terminal holds input (zerus-63rru.79), through the real
//! pass: the members' fleet rows as the producers make them
//! (`production::native`), the production availability and row stamp, and a
//! delivery port that answers as `hgs task team-deliver` does, read by the
//! production decode. The command's own answer for such a terminal is
//! witnessed in `native::a_hand_out_to_a_terminal_that_holds_input_*`.
use super::super::ports::{self, Ports};
use crate::teams::ops::tests::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// How the task command answers a member's hand-out.
#[derive(Clone, Copy, Debug)]
enum Answer {
    Submitted,
    /// Refused by the terminal gate: the terminal holds input.
    Waiting,
    /// Refused for another reason, with the command's sentence and, when
    /// given, a cause this build does not know.
    Refused(&'static str, Option<&'static str>),
}

struct Terminal {
    native: super::super::native::Native,
    answers: BTreeMap<String, Answer>,
    /// Each delivery: the pass's time, the member and what the decode read.
    deliveries: Vec<(i64, String, String)>,
    /// Each delivery's node, in the same order.
    nodes: Vec<u32>,
    now: i64,
}

/// The task command's reply, with the keys `team_deliver::Input::reply`
/// writes.
fn reply(id: &str, node: u32, member: &str, answer: Answer) -> Value {
    let mut out = json!({"schema":1,"id":id,"team":TEAM,"node":node,"member":member});
    match answer {
        Answer::Submitted => {
            out["state"] = json!("submitted");
            out["receipt"] = json!({"request_id":id,"status":"submitted"});
        }
        Answer::Waiting => {
            out["state"] = json!("refused");
            out["error"] = json!(crate::state::TERMINAL_DRAFT);
            out["cause"] = json!(crate::tasks::TERMINAL_WAITING);
        }
        Answer::Refused(sentence, cause) => {
            out["state"] = json!("refused");
            out["error"] = json!(sentence);
            if let Some(cause) = cause {
                out["cause"] = json!(cause);
            }
        }
    }
    out
}

impl Ports for Terminal {
    fn changes(&mut self, _: &str, _: &str, _: u64) -> ports::Result<zerus_replica::Changes> {
        Ok(zerus_replica::Changes {
            generation: "journal".into(),
            offset: 0,
            documents: Some(Default::default()),
        })
    }
    fn observe(
        &mut self,
        _: &mut crate::teams::store::Store,
        _: &zerus_team_plan::model::TaskRef,
    ) -> ports::Result<()> {
        Ok(())
    }
    fn free(
        &mut self,
        t: &zerus_team_plan::model::Team,
        m: &zerus_team_plan::model::Member,
    ) -> ports::Result<bool> {
        Ports::free(&mut self.native, t, m)
    }
    fn availability(
        &mut self,
        t: &zerus_team_plan::model::Team,
        m: &zerus_team_plan::model::Member,
    ) -> ports::Result<ports::Availability> {
        Ports::availability(&mut self.native, t, m)
    }
    fn row_stamp(
        &mut self,
        t: &zerus_team_plan::model::Team,
        m: &zerus_team_plan::model::Member,
    ) -> ports::Result<Option<String>> {
        Ports::row_stamp(&mut self.native, t, m)
    }
    fn deliver(
        &mut self,
        t: &zerus_team_plan::model::Team,
        n: &zerus_team_plan::model::Node,
        m: &zerus_team_plan::model::Member,
        id: &str,
    ) -> ports::DeliveryResult {
        let answer = self
            .answers
            .get(&m.id)
            .copied()
            .unwrap_or(Answer::Submitted);
        let bytes = reply(id, n.id, &m.id, answer).to_string().into_bytes();
        crate::desktop::teams::child::FIXTURE_EXIT.with(|s| *s.borrow_mut() = Some(bytes));
        let r = Ports::deliver(&mut self.native, t, n, m, id);
        crate::desktop::teams::child::FIXTURE_EXIT.with(|s| s.borrow_mut().take());
        self.deliveries
            .push((self.now, m.id.clone(), format!("{r:?}")));
        self.nodes.push(n.id);
        r
    }
}

fn setup(members: &[&str]) -> (Harness, super::super::Runtime, Terminal) {
    let mut h = Harness::new();
    for m in members {
        h.bind(m);
    }
    h.add(&person(), 1);
    let machine = h.store.team(TEAM).unwrap().unwrap().home;
    let native = super::production::native(&h);
    let a = super::super::Runtime::open(h.temp.path(), &machine).unwrap();
    let p = Terminal {
        native,
        answers: BTreeMap::new(),
        deliveries: vec![],
        nodes: vec![],
        now: 0,
    };
    (h, a, p)
}

fn pass(h: &mut Harness, a: &mut super::super::Runtime, p: &mut Terminal, now: i64) {
    p.now = now;
    a.begin_pass();
    a.tick(&mut h.store, p, TEAM, now, false).unwrap();
}

/// What each delivery came to: the time, the member and the decode's kind.
fn sent(p: &Terminal) -> Vec<(i64, &str, &str)> {
    p.deliveries
        .iter()
        .map(|(at, member, read)| {
            let kind = if read.starts_with("TerminalWaiting") {
                "waiting"
            } else if read.contains("Submitted") {
                "submitted"
            } else if read.contains("Refused") {
                "refused"
            } else {
                "other"
            };
            (*at, member.as_str(), kind)
        })
        .collect()
}

fn intents(h: &Harness) -> Vec<(String, String)> {
    h.store
        .effects(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .filter(|e| e.node == 1 && e.kind == "handout")
        .map(|e| (e.member, e.state))
        .collect()
}

fn open_cards(h: &Harness) -> Vec<String> {
    h.store
        .cards(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .filter(|c| c.state == "open")
        .map(|c| c.text)
        .collect()
}

fn failure_gate(a: &super::super::Runtime) -> Option<Value> {
    a.ledger
        .get(TEAM, "gate:handout:1")
        .unwrap()
        .map(|row| row.data)
}

fn terminal_row(a: &super::super::Runtime, member: &str) -> Option<Value> {
    a.ledger
        .get(TEAM, &format!("terminal:{member}"))
        .unwrap()
        .map(|row| row.data)
}

fn outcome_texts(h: &Harness) -> Vec<String> {
    h.store
        .journal(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .filter(|j| j.kind == "handout_outcome")
        .map(|j| j.text)
        .collect()
}

const CARD: &str = "Member m1 (developer) is needed by ready task 1, but its terminal is waiting for input (a question, a dialog or an unsent draft). Answer it or clear it in the member's terminal. This card closes when the member can take work again or no ready task needs it.";

/// The investigation's eleven passes (zerus-63rru.79): the first free member's
/// terminal holds input, its row says free. Before this, every pass named it
/// again, its refusals counted as failed hand-outs and the third opened the
/// failure card. Now: one refused intent, no failure row and no card, and the
/// second free member takes the task on the next pass.
#[test]
fn a_terminal_that_holds_input_is_one_refused_intent_and_the_next_member_takes_the_task() {
    let (mut h, mut a, mut p) = setup(&["m1", "m2"]);
    p.answers.insert("m1".into(), Answer::Waiting);
    pass(&mut h, &mut a, &mut p, 100);
    assert_eq!(sent(&p), [(100, "m1", "waiting")]);
    assert_eq!(failure_gate(&a), None, "no failure row");
    let fact = terminal_row(&a, "m1").expect("the member's fact");
    assert_eq!(fact["refusals"], 1, "{fact}");
    for i in 1..11 {
        pass(&mut h, &mut a, &mut p, 100 + i * 5_000);
        assert_eq!(failure_gate(&a), None, "pass {i}");
        assert!(open_cards(&h).is_empty(), "pass {i}: {:?}", open_cards(&h));
    }
    assert_eq!(
        sent(&p),
        [(100, "m1", "waiting"), (5_100, "m2", "submitted")]
    );
    assert_eq!(
        intents(&h),
        [
            ("m1".to_owned(), "refused".to_owned()),
            ("m2".to_owned(), "submitted".to_owned())
        ]
    );
    let texts = outcome_texts(&h);
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(
        texts[0].ends_with(&format!(": refused: {}", crate::state::TERMINAL_DRAFT)),
        "{texts:?}"
    );
    assert!(texts[1].ends_with(": submitted"), "{texts:?}");
}

/// With no second member the task waits. The member is tried again only
/// after the retry table's growing delay (1, 2, then 4 minutes), the card
/// says what is true once the task has needed the member for the period, and
/// it closes when the member's terminal takes the task.
#[test]
fn with_no_other_member_the_task_waits_the_member_is_retried_by_a_growing_delay_and_the_card_tells_why(
) {
    let (mut h, mut a, mut p) = setup(&["m1"]);
    p.answers.insert("m1".into(), Answer::Waiting);
    for now in [100, 1_000, 30_000, 59_000] {
        pass(&mut h, &mut a, &mut p, now);
    }
    assert_eq!(sent(&p), [(100, "m1", "waiting")], "waits for 1 minute");
    assert!(open_cards(&h).is_empty());
    for now in [60_100, 61_000, 120_000, 179_000] {
        pass(&mut h, &mut a, &mut p, now);
    }
    assert_eq!(
        sent(&p),
        [(100, "m1", "waiting"), (60_100, "m1", "waiting")],
        "then waits for 2 minutes"
    );
    assert_eq!(terminal_row(&a, "m1").unwrap()["refusals"], 2);
    assert!(open_cards(&h).is_empty(), "not before the period");
    for now in [180_100, 181_000, 200_000] {
        pass(&mut h, &mut a, &mut p, now);
    }
    assert_eq!(sent(&p).len(), 3, "tried again after 2 minutes");
    assert_eq!(terminal_row(&a, "m1").unwrap()["refusals"], 3);
    assert_eq!(open_cards(&h), [CARD], "after the period, one card");
    for now in [300_000, 419_000] {
        pass(&mut h, &mut a, &mut p, now);
    }
    assert_eq!(sent(&p).len(), 3, "then waits for 4 minutes");
    assert_eq!(open_cards(&h), [CARD], "the one card stays");
    assert_eq!(failure_gate(&a), None);
    // The person cleared the terminal: the next try is submitted, and the
    // card closes once the member has taken the task.
    p.answers.insert("m1".into(), Answer::Submitted);
    pass(&mut h, &mut a, &mut p, 420_100);
    pass(&mut h, &mut a, &mut p, 421_000);
    assert_eq!(
        sent(&p),
        [
            (100, "m1", "waiting"),
            (60_100, "m1", "waiting"),
            (180_100, "m1", "waiting"),
            (420_100, "m1", "submitted")
        ]
    );
    assert!(open_cards(&h).is_empty(), "{:?}", open_cards(&h));
    assert_eq!(terminal_row(&a, "m1"), None);
    assert_eq!(failure_gate(&a), None);
}

#[test]
fn a_fresh_read_clears_the_unreadable_notice_while_a_terminal_still_waits() {
    let (mut h, mut a, mut p) = setup(&["m1"]);
    h.run(
        &person(),
        crate::teams::ops::Operation::ArchiveMember {
            member: "m2".into(),
        },
    )
    .unwrap();
    p.answers.insert("m1".into(), Answer::Waiting);
    for now in [100, 60_100, 180_100, 420_100] {
        pass(&mut h, &mut a, &mut p, now);
    }
    let waiting = terminal_row(&a, "m1").unwrap();
    assert_eq!(waiting["refusals"], 4);
    assert_eq!(waiting["until"], 900_100);

    let notices = |h: &Harness| {
        h.store
            .inbox(TEAM, 200, None)
            .unwrap()
            .rows
            .into_iter()
            .filter(|row| row.kind.starts_with("unstaffed:"))
            .collect::<Vec<_>>()
    };
    // This counter does not change the identity of the terminal refusal.
    let team = h.store.team(TEAM).unwrap().unwrap();
    let member = h.store.member(TEAM, "m1").unwrap().unwrap();
    set(&p, "$.process_work.count", &Value::Null);
    assert_eq!(
        p.availability(&team, &member).unwrap(),
        ports::Availability::Unreadable(ports::Fact::Background)
    );
    pass(&mut h, &mut a, &mut p, 420_101);
    assert!(notices(&h).is_empty());
    pass(&mut h, &mut a, &mut p, 600_101);
    let opened = notices(&h);
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].state, "open");
    assert_eq!(
        h.store.unreadable_members(TEAM).unwrap(),
        ["m1".into()].into()
    );

    set(&p, "$.process_work.count", &json!(0));
    assert_eq!(
        p.availability(&team, &member).unwrap(),
        ports::Availability::Free
    );
    pass(&mut h, &mut a, &mut p, 600_102);
    assert_eq!(notices(&h)[0].state, "open");
    pass(&mut h, &mut a, &mut p, 780_102);
    assert!(h.store.unreadable_members(TEAM).unwrap().is_empty());
    let closed = notices(&h);
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].seq, opened[0].seq);
    assert_eq!(closed[0].state, "closed");
    assert_eq!(terminal_row(&a, "m1"), Some(waiting));
    assert_eq!(open_cards(&h), [CARD]);
    assert_eq!(sent(&p).len(), 4);
}

/// The member's committed fleet rows, one value changed in place.
fn set(p: &Terminal, path: &str, value: &Value) {
    let db =
        rusqlite::Connection::open(p.native.config.state.join("desktop/state.sqlite3")).unwrap();
    let changed = db
        .execute(
            "UPDATE desktop_catalog_rows SET json=json_set(json,?1,json(?2))",
            rusqlite::params![path, value.to_string()],
        )
        .unwrap();
    assert!(changed > 0);
}

/// The stamp's paths are values the reader reads.
#[test]
fn the_stamped_paths_are_read_paths() {
    use super::super::fleet::{READ, STAMPED};
    for path in STAMPED {
        assert!(READ.iter().any(|(read, _)| *read == path), "{path}");
    }
}

/// A changed conversation, phase or question count ends the member's fact
/// at once, before its time: it says another session, or one that waits.
#[test]
fn a_changed_row_ends_the_members_fact_at_once() {
    for (path, during, after) in [
        ("$.conversation_id", json!("conversation-after"), None),
        ("$.phase", json!("input"), Some(json!("idle"))),
        ("$.optional_questions.count", json!(1), Some(json!(0))),
    ] {
        let (mut h, mut a, mut p) = setup(&["m1"]);
        p.answers.insert("m1".into(), Answer::Waiting);
        pass(&mut h, &mut a, &mut p, 100);
        pass(&mut h, &mut a, &mut p, 500);
        assert_eq!(sent(&p), [(100, "m1", "waiting")], "{path}");
        set(&p, path, &during);
        p.answers.insert("m1".into(), Answer::Submitted);
        pass(&mut h, &mut a, &mut p, 1_000);
        if let Some(after) = &after {
            assert_eq!(terminal_row(&a, "m1"), None, "{path}: ended at once");
            set(&p, path, after);
            pass(&mut h, &mut a, &mut p, 2_000);
        }
        let at = if after.is_some() { 2_000 } else { 1_000 };
        assert_eq!(
            sent(&p),
            [(100, "m1", "waiting"), (at, "m1", "submitted")],
            "{path}"
        );
        assert_eq!(terminal_row(&a, "m1"), None, "{path}");
    }
}

/// Values that change while a terminal still waits do not end the fact: a
/// member whose children count, background-work count or activity changes
/// on every pass is tried again only by the growing delay (1, then 2
/// minutes), one refused intent per delay, and the card opens after the
/// period.
#[test]
fn work_counts_and_activity_that_change_while_the_terminal_waits_keep_the_fact() {
    for (path, busy, free) in [
        ("$.subagent_active_count", json!(1), json!(0)),
        ("$.process_work.count", json!(1), json!(0)),
        ("$.activity", json!("busy"), json!("idle")),
    ] {
        let (mut h, mut a, mut p) = setup(&["m1"]);
        p.answers.insert("m1".into(), Answer::Waiting);
        for (i, now) in [
            100, 1_000, 30_000, 59_000, 60_100, 61_000, 120_000, 179_000, 180_100, 181_000, 200_000,
        ]
        .into_iter()
        .enumerate()
        {
            set(&p, path, if i % 2 == 0 { &free } else { &busy });
            pass(&mut h, &mut a, &mut p, now);
        }
        assert_eq!(
            sent(&p),
            [
                (100, "m1", "waiting"),
                (60_100, "m1", "waiting"),
                (180_100, "m1", "waiting")
            ],
            "{path}"
        );
        assert_eq!(terminal_row(&a, "m1").unwrap()["refusals"], 3, "{path}");
        assert_eq!(open_cards(&h), [CARD], "{path}");
        assert_eq!(failure_gate(&a), None, "{path}");
    }
}

/// Every other refusal is still a failed hand-out, as before: a failure row,
/// the hand-out's backoff (the second member is not tried on the next pass),
/// and no fact about the member. One of each kind the command gives: a rule
/// of the request, an attempt the fence did not admit, a refusal before any
/// input for another cause, and a cause this build does not know.
#[test]
fn every_other_refusal_is_still_a_failed_hand_out() {
    for (kind, sentence, cause) in [
        (
            "rule",
            "Hand-out 1 to m1 is not an open effect of this team",
            None,
        ),
        ("not admitted", "The attempt was not admitted.", None),
        ("before input", "invalid tracked pane", None),
        (
            "unknown cause",
            "terminal has a draft, dialog, or unrecognized prompt",
            Some("a_cause_from_a_newer_build"),
        ),
    ] {
        let (mut h, mut a, mut p) = setup(&["m1", "m2"]);
        p.answers
            .insert("m1".into(), Answer::Refused(sentence, cause));
        pass(&mut h, &mut a, &mut p, 100);
        pass(&mut h, &mut a, &mut p, 5_100);
        assert_eq!(sent(&p), [(100, "m1", "refused")], "{kind}");
        let gate = failure_gate(&a).unwrap_or_else(|| panic!("{kind}: a failure row"));
        assert_eq!(gate["failures"], 1, "{kind}: {gate}");
        assert_eq!(terminal_row(&a, "m1"), None, "{kind}");
    }
}

/// A restart of the service between the refusal and the next pass keeps the
/// member's fact: it lives in the actor's ledger. A restart between the
/// recorded outcome and its settling settles it as the same fact.
#[test]
fn a_restart_between_the_refusal_and_the_next_pass_keeps_the_members_fact() {
    let (mut h, mut a, mut p) = setup(&["m1", "m2"]);
    p.answers.insert("m1".into(), Answer::Waiting);
    pass(&mut h, &mut a, &mut p, 100);
    drop(a);
    let home = h.store.team(TEAM).unwrap().unwrap().home;
    let mut a = super::super::Runtime::open(h.temp.path(), &home).unwrap();
    pass(&mut h, &mut a, &mut p, 5_100);
    assert_eq!(
        sent(&p),
        [(100, "m1", "waiting"), (5_100, "m2", "submitted")]
    );
    assert_eq!(failure_gate(&a), None);

    let (mut h, a, mut p) = setup(&["m1", "m2"]);
    p.answers.insert("m1".into(), Answer::Waiting);
    super::super::delivery::CRASH.with(|c| c.set(Some("outcome")));
    let mut a = a;
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pass(&mut h, &mut a, &mut p, 100)
    }));
    super::super::delivery::CRASH.with(|c| c.set(None));
    assert!(crashed.is_err(), "the outcome boundary was not reached");
    drop(a);
    let home = h.store.team(TEAM).unwrap().unwrap().home;
    let mut a = super::super::Runtime::open(h.temp.path(), &home).unwrap();
    for now in [200, 5_100, 10_100] {
        pass(&mut h, &mut a, &mut p, now);
    }
    assert_eq!(failure_gate(&a), None, "settled as the member's fact");
    assert_eq!(terminal_row(&a, "m1").unwrap()["refusals"], 1);
    let sent: Vec<(&str, &str)> = sent(&p).into_iter().map(|(_, m, k)| (m, k)).collect();
    assert_eq!(sent, [("m1", "waiting"), ("m2", "submitted")]);
}

/// A reopen to a member whose fact stands waits too (its own task returned
/// while its terminal refused another hand-out), and goes once the fact
/// lapses.
#[test]
fn a_reopen_to_a_member_whose_terminal_holds_input_waits_for_the_fact_to_end() {
    use crate::teams::ops::{Operation, TaskStage};
    let (mut h, mut a, mut p) = setup(&["m1"]);
    h.add(&person(), 2);
    pass(&mut h, &mut a, &mut p, 100);
    assert_eq!(sent(&p), [(100, "m1", "submitted")]);
    let first = p.nodes[0];
    h.observe(first, TaskStage::InReview);
    h.run(
        &service(),
        Operation::RecordReport {
            node: first,
            generation: 1,
            member: "m1".into(),
            text: "Report".into(),
            commit: COMMIT.into(),
            notes: Vec::new(),
        },
    )
    .unwrap();
    // m1 is free again; its terminal refuses the other task.
    p.answers.insert("m1".into(), Answer::Waiting);
    pass(&mut h, &mut a, &mut p, 1_000);
    assert_eq!(sent(&p).len(), 2);
    assert_eq!(sent(&p)[1], (1_000, "m1", "waiting"));
    // The lead returns the first task: its reopen is for m1.
    let lead = h.bind("lead");
    h.run(
        &lead,
        Operation::Return {
            node: first,
            reason: "Revise".into(),
        },
    )
    .unwrap();
    p.answers.insert("m1".into(), Answer::Submitted);
    for now in [2_000, 30_000, 60_000] {
        pass(&mut h, &mut a, &mut p, now);
    }
    assert_eq!(sent(&p).len(), 2, "nothing is sent while the fact stands");
    pass(&mut h, &mut a, &mut p, 61_100);
    assert_eq!(sent(&p).len(), 3, "{:?}", sent(&p));
    assert_eq!(sent(&p)[2], (61_100, "m1", "submitted"));
}
