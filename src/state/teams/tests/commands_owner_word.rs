use super::*;
use super::support::*;

/// U54: the lead relaunches a member only on a word that names it.
#[test]
fn the_leads_relaunch_needs_a_word_that_names_the_member() {
    use super::*;
    use crate::state::teams::model::Member;
    let team = Team {
        id: "t".into(),
        name: "T".into(),
        lead: "lead".into(),
        members: vec![Member { id: "dev".into(), role: "Backend".into(), agent: "claude".into(), ..Default::default() }],
        ..Default::default()
    };
    let rule = roster_word_rule(&team, "relaunch", &json!({"member":"dev"}));
    assert!(rule.needed());
    assert!(rule.names.iter().any(|name| name == "dev"), "{:?}", rule.names);
    assert!(rule.names.iter().any(|name| name.eq_ignore_ascii_case("backend")), "{:?}", rule.names);
    // Another member's relaunch is not this word's.
    assert!(!roster_word_rule(&team, "relaunch", &json!({"member":""})).needed());
}

/// #432 N9: the swap and the owner's word together, through the command
/// entry: the lead swaps on the exact sentence naming the member by its
/// role, and that sentence pays for one swap only.
#[test]
fn a_lead_swaps_a_member_on_the_owners_word() {
    let dir = tempfile::tempdir().unwrap();
    let now = crate::state::now();
    let said = "замени бэкендера на кими";
    owner_word_events(dir.path(), &[(now - 60.0, said)]);
    crate::state::with_test_state_root(dir.path(), || {
        set_test_caller_identities(Some((Some("c1".into()), None, None)));
        let mut store = Store::open(true).unwrap();
        let team = full_team();
        let id = team.id.clone();
        store.create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
        let swap = |request: &str, member: &str, agent: &str| {
            run(
                "swap-member",
                &json!({"request_id":request,"team":id,"member":member,"agent":agent,"owner_word":{"text":said}}),
                false,
            )
        };
        // Without the owner's sentence the lead does not swap.
        let error = run(
            "swap-member",
            &json!({"request_id":"22222222-2222-4222-8222-222222222222","team":id,"member":"m1","agent":"kimi"}),
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("your word"), "{error}");
        let swapped = swap("33333333-3333-4333-8333-333333333333", "m1", "kimi").unwrap();
        assert_eq!((swapped["status"].as_str(), swapped["agent"].as_str()), (Some("swapped"), Some("kimi")));
        // The same sentence pays for one swap only.
        let error = swap("44444444-4444-4444-8444-444444444444", "m2", "kimi").unwrap_err().to_string();
        assert!(error.contains("already used"), "{error}");
        assert!(Store::open(false).unwrap().load(&id).unwrap().unwrap().member("m2").unwrap().agent == "dsh");
    });
}

#[test]
fn every_roster_change_needs_the_owner_word_naming_it() {
    use rusqlite::Connection;
    let now = crate::state::now();
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE events (seq INTEGER PRIMARY KEY, name TEXT, conversation TEXT, payload TEXT);").unwrap();
    let add = |text: &str| {
        db.execute(
            "INSERT INTO events(name, conversation, payload) VALUES ('claude/x/lead','c1',?1)",
            rusqlite::params![json!({"type":"UserPromptSubmit","at":now - 60.0,"agent_id":"","detail":text}).to_string()],
        ).unwrap();
    };
    add("Please add a backend to the roster");
    add("Please add a frontend to the roster");
    add("Please swap dev for another member");
    add("Please remove dev from the roster");
    add("Please change max members to 5");
    let mut team = Team { id: "t".into(), name: "T".into(), lead: "lead".into(), status: TeamStatus::Running, ..Default::default() };
    team.members.push(Member { id: "lead".into(), role: "Lead".into(), agent: "claude".into(),
        session: Some(SessionRef { name: "claude/x/lead".into(), conversation_id: "c1".into(), ..Default::default() }), ..Default::default() });
    team.members.push(Member { id: "dev".into(), role: "Backend".into(), agent: "codex".into(), ..Default::default() });
    let caller = Actor::Member("lead".into());
    let verify = |_: &Team, word: &Value, rule: &super::super::owner_word::WordRule, now: f64| {
        super::super::owner_word::verify_roster_in(&db, "claude/x/lead", "c1", word["text"].as_str().unwrap_or_default(), rule, now)
    };
    let cases = [
        ("add-member", json!({"member":{"id":"bob","role":"Backend","agent":"codex"}}), "Please add a backend to the roster", "Please add a frontend to the roster"),
        ("swap-member", json!({"member":"dev"}), "Please swap dev for another member", "Please add a frontend to the roster"),
        ("remove-member", json!({"member":"dev"}), "Please remove dev from the roster", "Please add a frontend to the roster"),
        ("settings", json!({"settings":{"max_members":5}}), "Please change max members to 5", "Please add a frontend to the roster"),
    ];
    for (op, base, sentence, other) in cases {
        let mut without = base.clone();
        let input = base.clone();
        assert!(super::roster_word_rule(&team, op, &input).needed(), "{op}");
        let error = super::check_roster_word(&team, op, &input, &mut without, &caller, verify).unwrap_err().to_string();
        assert!(error.contains("your word"), "{op}: {error}");
        // A caller-supplied roster_word is not a word.
        let mut claimed = base.clone();
        claimed["roster_word"] = Value::Bool(true);
        let input = claimed.clone();
        let error = super::check_roster_word(&team, op, &input, &mut claimed, &caller, verify).unwrap_err().to_string();
        assert!(error.contains("your word"), "{op}: {error}");
        // A word naming something else is refused.
        let mut wrong = base.clone();
        wrong["owner_word"] = json!({"text": other});
        let input = wrong.clone();
        let error = super::check_roster_word(&team, op, &input, &mut wrong, &caller, verify).unwrap_err().to_string();
        assert!(error.contains("your word") || error.contains("not in the lead session") || error.contains("does not name"), "{op}: {error}");
        // The fresh word naming the change fills the verified flag.
        let mut good = base.clone();
        good["owner_word"] = json!({"text": sentence});
        let input = good.clone();
        super::check_roster_word(&team, op, &input, &mut good, &caller, verify).unwrap();
        assert_eq!(good["roster_word"], true, "{op}");
        assert!(good["owner_word_checked"].is_object(), "{op}");
    }
}

#[test]
fn scale_and_drain_need_the_owner_word_naming_them() {
    // U49: the lead scales on the count the sentence names, and drains on
    // the word that removes the member.
    use rusqlite::Connection;
    let now = crate::state::now();
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE events (seq INTEGER PRIMARY KEY, name TEXT, conversation TEXT, payload TEXT);").unwrap();
    let say = |text: &str| {
        db.execute(
            "INSERT INTO events(name, conversation, payload) VALUES ('claude/x/lead','c1',?1)",
            rusqlite::params![json!({"type":"UserPromptSubmit","at":now - 60.0,"agent_id":"","detail":text}).to_string()],
        )
        .unwrap();
    };
    say("Please add two backend members to the roster");
    say("Please drain frontend2 from the roster");
    say("Please remove frontend2 from the roster");
    say("Please add three backend members to the roster");
    let mut team = Team { id: "t".into(), name: "T".into(), lead: "lead".into(), status: TeamStatus::Running, ..Default::default() };
    team.members.push(Member {
        id: "lead".into(),
        role: "Lead".into(),
        agent: "claude".into(),
        session: Some(SessionRef { name: "claude/x/lead".into(), conversation_id: "c1".into(), ..Default::default() }),
        ..Default::default()
    });
    let caller = Actor::Member("lead".into());
    let verify = |_: &Team, word: &Value, rule: &super::super::owner_word::WordRule, now: f64| {
        super::super::owner_word::verify_roster_in(&db, "claude/x/lead", "c1", word["text"].as_str().unwrap_or_default(), rule, now)
    };
    // scale: the sentence says to add, names the role and the count.
    let scale = json!({"role":"Backend","agent":"codex","count":2});
    assert!(super::roster_word_rule(&team, "scale", &scale).needed());
    let mut without = scale.clone();
    let error = super::check_roster_word(&team, "scale", &scale, &mut without, &caller, verify).unwrap_err().to_string();
    assert!(error.contains("your word"), "{error}");
    let mut worded = scale.clone();
    worded["owner_word"] = json!({"text":"Please add two backend members to the roster"});
    let mut filled = worded.clone();
    super::check_roster_word(&team, "scale", &worded, &mut filled, &caller, verify).unwrap();
    assert_eq!((filled["roster_word"].clone(), filled["owner_word_change"].as_str()), (json!(true), Some("scaling Backend by 2")));
    // Another count or another role is not this scale.
    for text in ["Please add three backend members to the roster", "Please add two frontend members to the roster"] {
        let mut wrong = json!({"role":"Backend","agent":"codex","count":2,"owner_word":{"text":text}});
        let input = wrong.clone();
        let error = super::check_roster_word(&team, "scale", &input, &mut wrong, &caller, verify).unwrap_err().to_string();
        assert!(error.contains("your word") || error.contains("does not name"), "{text}: {error}");
    }
    // drain: the owner's drain or remove sentence names the member.
    let drain = json!({"member":"frontend2"});
    assert!(super::roster_word_rule(&team, "drain", &drain).needed());
    let mut without = drain.clone();
    let error = super::check_roster_word(&team, "drain", &drain, &mut without, &caller, verify).unwrap_err().to_string();
    assert!(error.contains("your word"), "{error}");
    let mut worded = json!({"member":"frontend2","owner_word":{"text":"Please drain frontend2 from the roster"}});
    let input = worded.clone();
    super::check_roster_word(&team, "drain", &input, &mut worded, &caller, verify).unwrap();
    assert_eq!(worded["roster_word"], true);
    let mut removal = json!({"member":"frontend2","owner_word":{"text":"Please remove frontend2 from the roster"}});
    let input = removal.clone();
    super::check_roster_word(&team, "drain", &input, &mut removal, &caller, verify).unwrap();
    let mut other = json!({"member":"frontend2","owner_word":{"text":"Please drain backend from the roster"}});
    let input = other.clone();
    assert!(super::check_roster_word(&team, "drain", &input, &mut other, &caller, verify).is_err());
    // Cancelling a drain is no roster change.
    assert!(!super::roster_word_rule(&team, "drain", &json!({"member":"frontend2","draining":false})).needed());
}

/// U49 through `run`: the lead's scale and drain ask for the owner's word
/// before anything changes.
#[test]
fn a_lead_scales_and_drains_only_through_the_word() {
    let _env = crate::state::ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let _worker = hold_worker_lock();
        let mut store = Store::open(true).unwrap();
        let mut team = running_team();
        {
            let lead = team.members.iter_mut().find(|member| member.id == "lead").unwrap();
            lead.agent = "dsh".into();
            lead.session = Some(SessionRef { name: "dsh/x/lead".into(), run_id: "r1".into(), conversation_id: "conv-lead".into(), ..Default::default() });
        }
        let id = team.id.clone();
        store.create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
        super::set_test_caller_identities(Some((Some("conv-lead".into()), None, None)));
        let mut n = 0u8;
        let mut request = |op: &str, input: Value| {
            n += 1;
            let mut input = input;
            input["team"] = json!(id);
            input["request_id"] = json!(format!("00000000-0000-4000-8000-0000000006{n:02}"));
            run(op, &input, false)
        };
        let error = request("scale", json!({"role":"Backend","agent":"codex","count":1})).unwrap_err().to_string();
        assert!(error.contains("your word"), "{error}");
        let error = request("drain", json!({"member":"dev"})).unwrap_err().to_string();
        assert!(error.contains("your word"), "{error}");
        // Cancelling a drain changes no roster: the lead may do it alone.
        let cancelled = request("drain", json!({"member":"dev","draining":false})).unwrap();
        assert_eq!(cancelled["status"], "draining_cancelled");
        super::set_test_caller_identities(None);
    });
}

#[test]
fn one_owner_word_is_spent_on_one_roster_change() {
    use rusqlite::Connection;
    let now = crate::state::now();
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE events (seq INTEGER PRIMARY KEY, name TEXT, conversation TEXT, payload TEXT);").unwrap();
    for (seq, text) in [(1, "Please remove dev from the roster"), (2, "Please remove dev from the roster again")] {
        db.execute("INSERT INTO events(seq,name,conversation,payload) VALUES (?1,'claude/x/lead','c1',?2)",
            rusqlite::params![seq, json!({"type":"UserPromptSubmit","at":now - 60.0,"agent_id":"","detail":text}).to_string()]).unwrap();
    }
    let mut team = Team { id: "t".into(), name: "T".into(), lead: "lead".into(), status: TeamStatus::Running, ..Default::default() };
    team.members.push(Member { id: "lead".into(), role: "Lead".into(), agent: "claude".into(), ..Default::default() });
    let caller = Actor::Member("lead".into());
    let verify = |_: &Team, word: &Value, rule: &super::super::owner_word::WordRule, now: f64| {
        super::super::owner_word::verify_roster_in(&db, "claude/x/lead", "c1", word["text"].as_str().unwrap_or_default(), rule, now)
    };
    let base = json!({"member":"dev"});
    let mut first = base.clone();
    first["owner_word"] = json!({"text":"Please remove dev from the roster"});
    let input = first.clone();
    super::check_roster_word(&team, "remove-member", &input, &mut first, &caller, verify).unwrap();
    // The operation spends the event on the team.
    let event = first["owner_word_event"].as_i64().unwrap();
    team.record_owner_word(event, "the roster change");
    let mut second = base.clone();
    second["owner_word"] = json!({"text":"Please remove dev from the roster"});
    let input = second.clone();
    let error = super::check_roster_word(&team, "remove-member", &input, &mut second, &caller, verify).unwrap_err().to_string();
    assert!(error.contains("already used"), "{error}");
    // A new sentence works.
    let mut third = base.clone();
    third["owner_word"] = json!({"text":"Please remove dev from the roster again"});
    let input = third.clone();
    super::check_roster_word(&team, "remove-member", &input, &mut third, &caller, verify).unwrap();
}

/// U29 through `run`: a task joins a running phase only on the owner's
/// word twice, with the warning between the two words.
#[test]
fn a_task_joins_a_running_phase_on_the_owners_word_twice() {
    let _env = crate::state::ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let _worker = hold_worker_lock();
        let mut store = Store::open(true).unwrap();
        let mut team = running_team();
        {
            let lead = team.members.iter_mut().find(|member| member.id == "lead").unwrap();
            lead.agent = "dsh".into();
            lead.session = Some(SessionRef { name: "dsh/x/lead".into(), run_id: "r1".into(), conversation_id: "conv-lead".into(), ..Default::default() });
        }
        let id = team.id.clone();
        store.create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
        let db = super::super::super::journal::event_db().unwrap();
        let say = |at: f64, text: &str| {
            db.execute(
                "INSERT INTO events(name, conversation, payload) VALUES ('dsh/x/lead','conv-lead',?1)",
                rusqlite::params![json!({"type":"UserPromptSubmit","at":at,"agent_id":"","detail":text}).to_string()],
            ).unwrap();
        };
        let mut n = 0u8;
        let mut request = |input: Value| {
            n += 1;
            let mut input = input;
            input["team"] = json!(id);
            input["request_id"] = json!(format!("00000000-0000-4000-8000-0000000005{n:02}"));
            run("add-to-phase", &input, false)
        };
        let task = json!([{"id":2,"title":"Fix the login crash","assignee":"dev"}]);
        let stored = || Store::open(false).unwrap().load("t").unwrap().unwrap();
        let lead = || super::set_test_caller_identities(Some((Some("conv-lead".into()), None, None)));
        let now = crate::state::now();
        say(now - 300.0, "Yes, add it");   // older than any warning
        say(now - 200.0, "please continue with the plan");
        say(now - 100.0, "Please add a task to the current phase: fix the login crash");
        lead();
        // 1. No word: refused, nothing changes.
        let error = request(json!({"phase":1,"tasks":task})).unwrap_err().to_string();
        assert!(error.contains("your word"), "{error}");
        // A sentence that does not ask to add a task is not the word.
        let error = request(json!({"phase":1,"tasks":task,"owner_word":{"text":"please continue with the plan"}})).unwrap_err().to_string();
        assert!(error.contains("has not asked to add a task to phase 1"), "{error}");
        // 2. The request word alone changes nothing and returns the warning.
        let warned = request(json!({"phase":1,"tasks":task,"owner_word":{"text":"Please add a task to the current phase"}})).unwrap();
        assert_eq!(warned["status"], "warning", "{warned}");
        assert!(warned["warning"].as_str().unwrap().contains("load balance of the lanes"), "{warned}");
        // #511 C1: the warning names the queued task titles, so the owner
        // confirms what joins.
        assert!(warned["warning"].as_str().unwrap().contains("\"Fix the login crash\""), "{warned}");
        let pending = warned["pending"].as_str().unwrap().to_owned();
        let team = stored();
        assert_eq!(team.tasks.len(), 1);
        assert_eq!(team.pending_additions.len(), 1);
        // 3. A confirmation older than the warning is refused.
        let confirm = |text: &str, phase: u32| json!({"phase":phase,"confirm":pending,"owner_word":{"text":text}});
        let error = request(confirm("Yes, add it", 1)).unwrap_err().to_string();
        assert!(error.contains("has not confirmed the addition to phase 1"), "{error}");
        // 4. A confirmation for another phase is refused: by its words and by its number.
        say(crate::state::now() + 0.2, "Yes, add it to phase 2");
        let error = request(confirm("Yes, add it to phase 2", 1)).unwrap_err().to_string();
        assert!(error.contains("has not confirmed"), "{error}");
        say(crate::state::now() + 0.2, "Confirmed, add it now");
        let error = request(confirm("Confirmed, add it now", 2)).unwrap_err().to_string();
        assert!(error.contains("for phase 1, not phase 2") || error.contains("not running"), "{error}");
        assert_eq!(stored().tasks.len(), 1);
        // C1: a refusal never confirms, an unrelated sentence with "ok" is
        // not the answer, and a fragment is not the owner's whole sentence.
        for said in ["No, don't add it", "не добавляй", "ok, what's the status"] {
            say(crate::state::now() + 0.2, said);
            let error = request(confirm(said, 1)).unwrap_err().to_string();
            assert!(error.contains("has not confirmed"), "{said}: {error}");
        }
        let error = request(confirm("add it", 1)).unwrap_err().to_string();
        assert!(error.contains("has not confirmed"), "{error}");
        assert_eq!(stored().tasks.len(), 1, "no refusal added a task");
        // 5. The confirmed addition adds the task with added_late.
        let added = request(confirm("Confirmed, add it now", 1)).unwrap();
        assert_eq!(added["status"], "added", "{added}");
        let team = stored();
        let task = team.task(2).unwrap();
        assert_eq!((task.phase, task.status), (1, TaskStatus::Pending));
        let late = task.added_late.as_ref().unwrap();
        assert_eq!(late.by, "Lead");
        assert_eq!(late.request, "Please add a task to the current phase: fix the login crash");
        assert_eq!(late.confirm, "Confirmed, add it now");
        assert!(team.pending_additions.is_empty());
        let feed = run("events", &json!({"team":"t"}), false).unwrap().to_string();
        assert!(feed.contains("phase_tasks_added") && feed.contains("phase_addition_warned"), "{feed}");
        // The same words are spent.
        let error = request(json!({"phase":1,"tasks":[{"id":3,"title":"Again","assignee":"dev"}],
            "owner_word":{"text":"Please add a task to the current phase"}})).unwrap_err().to_string();
        assert!(error.contains("already used"), "{error}");
        // 6. The owner on the desktop: the warning, then "add" or the phase number.
        super::set_test_caller_identities(Some((None, None, None)));
        let human = |value: Value| { let mut value = value; value["by"] = json!("human"); value };
        let warned = request(human(json!({"phase":1,"tasks":[{"id":3,"title":"Docs","assignee":"dev"}]}))).unwrap();
        assert!(warned["warning"].as_str().unwrap().contains("\"Docs\""), "{warned}");
        let pending = warned["pending"].as_str().unwrap().to_owned();
        let error = request(human(json!({"phase":1,"confirm":pending,"confirmation":"sure"}))).unwrap_err().to_string();
        assert!(error.contains("Type \"add\" or \"1\""), "{error}");
        let added = request(human(json!({"phase":1,"confirm":pending,"confirmation":"1"}))).unwrap();
        assert_eq!(added["status"], "added", "{added}");
        assert_eq!(stored().task(3).unwrap().added_late.as_ref().unwrap().by, "You");
        // The owner's Cancel declines a warning, with no word; the lead never does.
        let warned = request(human(json!({"phase":1,"tasks":[{"id":4,"title":"Maybe","assignee":"dev"}]}))).unwrap();
        let pending = warned["pending"].as_str().unwrap().to_owned();
        lead();
        let error = request(json!({"phase":1,"confirm":pending,"decline":true})).unwrap_err().to_string();
        assert!(error.contains("Only you decline"), "{error}");
        assert_eq!(stored().pending_additions.len(), 1);
        super::set_test_caller_identities(Some((None, None, None)));
        let declined = request(human(json!({"phase":1,"confirm":pending,"decline":true}))).unwrap();
        assert_eq!((declined["status"].as_str(), declined["found"].as_bool()), (Some("declined"), Some(true)), "{declined}");
        assert!(stored().pending_additions.is_empty() && stored().task(4).is_none());
        // A draft phase keeps edit-draft.
        let mut draft = stored();
        let mut events = Vec::new();
        ops::apply(&mut draft, "extend", &json!({"phase_title":"Next","tasks":[{"id":4,"title":"Later","assignee":"dev"}]}),
            &Actor::Human, &BTreeSet::new(), &mut events).unwrap();
        let error = ops::apply(&mut draft, "add-to-phase", &json!({"phase":2,"tasks":[{"id":5,"title":"Late","assignee":"dev"}]}),
            &Actor::Human, &BTreeSet::new(), &mut events).unwrap_err().to_string();
        assert!(error.contains("edit-draft") || error.contains("not running"), "{error}");
        // 7. A member never adds.
        super::set_test_caller_identities(Some((None, Some("codex/x/dev".into()), Some("r1".into()))));
        let error = request(json!({"phase":1,"tasks":[{"id":4,"title":"Mine","assignee":"dev"}],"by":"human"})).unwrap_err().to_string();
        assert!(!error.is_empty());
        assert!(stored().task(4).is_none());
        super::set_test_caller_identities(None);
    });
}

/// U32: the owner's plain request adds a member by role or agent, raises a
/// full cap by the count the sentence names, and one word covers one add.
#[test]
fn the_owners_word_adds_a_member_by_role_and_raises_a_full_cap() {
    let dir = tempfile::tempdir().unwrap();
    let now = crate::state::now();
    let sentence = "может еще одного дипсик бэкендера добавим?";
    let frontend = "может еще одного фронтендера добавим?";
    let stale = "еще одного дипсик бэкендера добавим";
    let relayed = "[Zerus team · from: Zerus] should we add a backend?";
    owner_word_events(dir.path(), &[
        (now - 60.0, sentence),
        (now - 60.0, frontend),
        (now - super::super::owner_word::FRESH_SECONDS - 60.0, stale),
        (now - 60.0, relayed),
        (now - 60.0, "Please remove the backend from the roster"),
        (now - 60.0, "Please remove the tester from the roster"),
    ]);
    crate::state::with_test_state_root(dir.path(), || {
        set_test_caller_identities(Some((Some("c1".into()), None, None)));
        let mut store = Store::open(true).unwrap();
        let team = full_team();
        let id = team.id.clone();
        store.create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
        let add = |request: &str, member: &str, text: &str| {
            run(
                "add-member",
                &json!({"request_id":request,"team":id,"member":{"id":member,"role":"Backend","agent":"dsh"},
                    "owner_word":{"text":text}}),
                false,
            )
        };
        // A sentence naming another role does not add a backend.
        let error = add("22222222-2222-4222-8222-222222222222", "backend4", frontend).unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("does not name the member to add"), "{error}");
        // A stale sentence and a relayed one are not the owner's word.
        let error = add("33333333-3333-4333-8333-333333333333", "backend4", stale).unwrap_err().to_string();
        assert!(error.contains("your word"), "{error}");
        let error = add("44444444-4444-4444-8444-444444444444", "backend4", relayed).unwrap_err().to_string();
        assert!(error.contains("your word"), "{error}");
        // The owner's plain request adds a dsh Backend and raises the full cap by one.
        let value = add("55555555-5555-4555-8555-555555555555", "backend4", sentence).unwrap();
        assert_eq!(value["status"], "added");
        assert_eq!(value["by"], "Lead on your word");
        let saved = Store::open(false).unwrap().load(&id).unwrap().unwrap();
        assert_eq!(saved.settings.max_members, 7);
        let added = saved.member("backend4").unwrap().added.clone().unwrap();
        assert_eq!(added["by"], "Lead on your word");
        assert_eq!(added["owner_word"], sentence);
        // Status shows how the member joined.
        let status = run("status", &json!({"team":id}), false).unwrap();
        assert_eq!(status["settings"]["max_members"], 7);
        let member = status["members"].as_array().unwrap().iter().find(|member| member["id"] == "backend4").cloned().unwrap();
        assert_eq!(member["added"]["by"], "Lead on your word");
        assert_eq!(member["added"]["owner_word"], sentence);
        // The feed names both caps and carries the quote.
        let events = run("events", &json!({"team":id}), false).unwrap();
        let items = events["items"].as_array().unwrap();
        let raised = items.iter().find(|item| item["kind"] == "max_members_raised").unwrap();
        assert_eq!((raised["detail"]["old"].as_u64(), raised["detail"]["new"].as_u64()), (Some(6), Some(7)));
        let joined = items.iter().find(|item| item["kind"] == "member_added").unwrap();
        assert_eq!(
            (joined["detail"]["by"].as_str(), joined["detail"]["owner_word"].as_str()),
            (Some("Lead on your word"), Some(sentence))
        );
        // One word covers one add: the same sentence adds no second member.
        let error = add("66666666-6666-4666-8666-666666666666", "backend5", sentence).unwrap_err().to_string();
        assert!(error.contains("already used"), "{error}");
        assert!(Store::open(false).unwrap().load(&id).unwrap().unwrap().member("backend5").is_none());
        // N15: a removal names the member by id or by its role: a sentence
        // naming another role is refused, the role's own one is the word.
        let error = run(
            "remove-member",
            &json!({"request_id":"77777777-7777-4777-8777-777777777777","team":id,"member":"backend4",
                "owner_word":{"text":"Please remove the tester from the roster"}}),
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("your word"), "{error}");
        let removed = run(
            "remove-member",
            &json!({"request_id":"88888888-8888-4888-8888-888888888888","team":id,"member":"backend4",
                "owner_word":{"text":"Please remove the backend from the roster"}}),
            false,
        )
        .unwrap();
        assert_eq!((removed["status"].as_str(), removed["member"].as_str()), (Some("removed"), Some("backend4")));
    });
}

/// U42 (#587): the lead discards an accepted phase, or returns one to a
/// draft, on the owner's sentence naming the phase and the change; the CLI
/// verifies it as it does for an approval.
#[test]
fn the_lead_discards_or_redrafts_an_accepted_phase_on_the_owners_word() {
    use crate::state::teams::model::{Phase, PhaseState, Task, TaskStatus};
    let dir = tempfile::tempdir().unwrap();
    let now = crate::state::now();
    owner_word_events(dir.path(), &[
        (now - 60.0, "discard phase 3, it is stale"),
        (now - 60.0, "верни фазу 2 в черновик"),
        (now - 60.0, "phase 2 looks fine"),
        (now - 60.0, "Don't discard phase 3, rework it"),
        (now - 60.0, "не отменяй фазу 2"),
        (now - 60.0, "phase 2 is not a draft any more"),
    ]);
    crate::state::with_test_state_root(dir.path(), || {
        set_test_caller_identities(Some((Some("c1".into()), None, None)));
        let mut store = Store::open(true).unwrap();
        let mut team = full_team();
        team.tasks = vec![
            Task { id: 1, title: "A".into(), assignee: "m1".into(), status: TaskStatus::InProgress, ..Default::default() },
            Task { id: 2, phase: 2, title: "B".into(), assignee: "m1".into(), status: TaskStatus::Pending, ..Default::default() },
            Task { id: 3, phase: 3, title: "C".into(), assignee: "m1".into(), status: TaskStatus::Pending, ..Default::default() },
        ];
        team.phases = vec![
            Phase { n: 2, order: 2, title: "Two".into(), state: PhaseState::Queued, tasks: vec![2], ..Default::default() },
            Phase { n: 3, order: 3, title: "Three".into(), state: PhaseState::Queued, tasks: vec![3], ..Default::default() },
        ];
        let id = team.id.clone();
        store.create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
        let call = |op: &str, request: &str, phase: u32, text: Option<&str>| {
            let mut input = json!({"request_id":request,"team":id,"phase":phase});
            if let Some(text) = text {
                input["owner_word"] = json!({"text":text});
            }
            run(op, &input, false)
        };
        // Without a word, or on a sentence that does not name the change,
        // the lead cannot.
        let error = call("discard-phase", "22222222-2222-4222-8222-222222222222", 3, None).unwrap_err().to_string();
        assert!(error.contains("Only you discard an accepted phase"), "{error}");
        let error = call("discard-phase", "33333333-3333-4333-8333-333333333333", 2, Some("phase 2 looks fine")).unwrap_err().to_string();
        assert!(error.contains("has not asked to discard phase 2"), "{error}");
        // A discard sentence names its own phase only.
        let error = call("discard-phase", "44444444-4444-4444-8444-444444444444", 2, Some("discard phase 3")).unwrap_err().to_string();
        assert!(error.contains("has not asked to discard phase 2"), "{error}");
        // #595 C1: a refusal never authorizes the change, in either language,
        // and the quote is the owner's whole sentence, never a fragment.
        let error = call("discard-phase", "4a444444-4444-4444-8444-444444444444", 3, Some("Don't discard phase 3")).unwrap_err().to_string();
        assert!(error.contains("has not asked to discard phase 3"), "{error}");
        let error = call("discard-phase", "4b444444-4444-4444-8444-444444444444", 2, Some("не отменяй фазу 2")).unwrap_err().to_string();
        assert!(error.contains("has not asked to discard phase 2"), "{error}");
        let error = call("return-to-draft", "4c444444-4444-4444-8444-444444444444", 2, Some("phase 2 is not a draft any more")).unwrap_err().to_string();
        assert!(error.contains("has not asked to return phase 2 to a draft"), "{error}");
        let error = call("discard-phase", "4d444444-4444-4444-8444-444444444444", 3, Some("discard phase 3")).unwrap_err().to_string();
        assert!(error.contains("has not asked to discard phase 3"), "a fragment is not the sentence: {error}");
        // The owner's discard of phase 3 is the lead's word for it: the whole
        // sentence, as the owner wrote it.
        let discarded = call("discard-phase", "55555555-5555-4555-8555-555555555555", 3, Some("discard phase 3, it is stale")).unwrap();
        assert_eq!(discarded["status"], "phase_discarded");
        // The Russian sentence returns phase 2 to a draft.
        let returned = call("return-to-draft", "66666666-6666-4666-8666-666666666666", 2, Some("верни фазу 2 в черновик")).unwrap();
        assert_eq!(returned["status"], "phase_returned_to_draft");
        let saved = Store::open(false).unwrap().load(&id).unwrap().unwrap();
        assert_eq!(saved.phases.iter().map(|phase| (phase.n, phase.state)).collect::<Vec<_>>(), vec![(2, PhaseState::Draft)]);
        assert_eq!(saved.task(2).unwrap().status, TaskStatus::Draft);
        let events = run("events", &json!({"team":id}), false).unwrap();
        let item = events["items"].as_array().unwrap().iter().find(|item| item["kind"] == "phase_returned_to_draft").cloned().unwrap();
        assert_eq!((item["detail"]["by"].as_str(), item["detail"]["owner_word"]["text"].as_str()), (Some("Lead"), Some("верни фазу 2 в черновик")));
    });
}

/// N7: "add two backends" authorizes two adds, and the full cap grows by
/// one for each of them; a third add on the same sentence is refused.
#[test]
fn a_count_word_authorizes_that_many_adds_and_the_cap_grows_by_one_each() {
    let dir = tempfile::tempdir().unwrap();
    let now = crate::state::now();
    let sentence = "add two deepseek backends to the roster";
    owner_word_events(dir.path(), &[(now - 60.0, sentence)]);
    crate::state::with_test_state_root(dir.path(), || {
        set_test_caller_identities(Some((Some("c1".into()), None, None)));
        let mut store = Store::open(true).unwrap();
        let team = full_team();
        let id = team.id.clone();
        let cap = team.settings.max_members;
        store.create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
        let add = |request: &str, member: &str| {
            run(
                "add-member",
                &json!({"request_id":request,"team":id,"member":{"id":member,"role":"Backend","agent":"dsh"},
                    "owner_word":{"text":sentence}}),
                false,
            )
        };
        let members = || Store::open(false).unwrap().load(&id).unwrap().unwrap().settings.max_members;
        assert_eq!(add("22222222-2222-4222-8222-222222222222", "backend4").unwrap()["status"], "added");
        assert_eq!(members(), cap + 1);
        assert_eq!(add("33333333-3333-4333-8333-333333333333", "backend5").unwrap()["status"], "added");
        assert_eq!(members(), cap + 2);
        let error = add("44444444-4444-4444-8444-444444444444", "backend6").unwrap_err().to_string();
        assert!(error.contains("already used"), "{error}");
    });
}

/// #728: the lead relaunches a stuck member on the owner's setting without a
/// word; the owner has the same route; without the setting the word is still
/// asked.
#[test]
fn the_lead_relaunches_a_stuck_member_with_lead_restarts_stuck() {
    use crate::state::teams::model::MemberStuck;
    let (mut team, _, _) = review_team();
    team.settings.lead_restarts_stuck = true;
    team.members.iter_mut().find(|member| member.id == "dev").unwrap().stuck =
        Some(MemberStuck { at: 1.0, reason: "no_progress".into(), phase: "tool".into(), ..Default::default() });
    with_review_team(&team, || {
        let receipt = as_lead(|| call("relaunch", json!({"member":"dev"}))).unwrap();
        assert_eq!(receipt["status"], json!("relaunching"));
        assert!(stored().member("dev").unwrap().restart_pending);
        // The owner takes the same route.
        let owner = call("relaunch", json!({"member":"dev","by":"human"})).unwrap();
        assert_eq!(owner["status"], json!("relaunching"));
    });
    // Without the setting the lead still needs the owner's word.
    let (mut strict, _, _) = review_team();
    strict.members.iter_mut().find(|member| member.id == "dev").unwrap().stuck =
        Some(MemberStuck { at: 1.0, reason: "no_progress".into(), phase: "tool".into(), ..Default::default() });
    with_review_team(&strict, || {
        let error = as_lead(|| call("relaunch", json!({"member":"dev"}))).unwrap_err().to_string();
        assert!(error.contains("owner_word"), "{error}");
    });
}
