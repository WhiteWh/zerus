// Mirrors the settings cases of tray/tests/test_speech.cpp.
use super::*;

const MACHINE: &str = "bbbbbbbb-bbbb-4bbb-bbbb-bbbbbbbbbbbb";
const KEY: &str = "synthetic-private-key";

fn ids() -> impl FnMut() -> String + Send + 'static {
    let mut next = 0;
    move || {
        next += 1;
        format!("aaaaaaaa-aaaa-4aaa-aaaa-{next:012}")
    }
}

fn page(host: &str, defaults: Defaults, pending: &str) -> Providers {
    Providers::new(Platform::Linux, host, defaults, pending, ids())
}

fn map(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap()
}

fn reply(value: Value) -> Answer {
    Answer {
        reply: map(value),
        error: String::new(),
    }
}

fn russian() -> Value {
    json!({"id": "russian", "name": "Russian", "adapter": "gigaam", "model": "v3_e2e_rnnt",
           "device": "cpu", "language": "ru", "available": true, "revision": 1})
}

fn listing(profiles: Value) -> Answer {
    reply(json!({
        "schema": 1, "machine_id": MACHINE, "revision": 1,
        "adapters": [
            {"id": "gigaam", "models": ["v3_e2e_rnnt", "v3_ctc"], "devices": ["cpu", "cuda"]},
            {"id": "openai_http", "models": []},
        ],
        "service": {"available": true},
        "profiles": profiles,
    }))
}

fn calls(effects: &[Effect]) -> Vec<Call> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Call(call) => Some(call.clone()),
            _ => None,
        })
        .collect()
}

fn generation(effects: &[Effect]) -> u64 {
    calls(effects)
        .iter()
        .find_map(|call| match call.id {
            CallId::List(generation) => Some(generation),
            _ => None,
        })
        .unwrap()
}

/// Refreshes and answers the listing with these profiles.
fn loaded(providers: &mut Providers, profiles: Value) {
    let effects = providers.refresh();
    let now = Instant::now();
    providers.answered(CallId::List(generation(&effects)), listing(profiles), now);
}

fn now() -> Instant {
    Instant::now()
}

fn remote_form() -> EditorForm {
    EditorForm {
        name: "Remote fixture".into(),
        id: "remote-fixture".into(),
        adapter: "openai_http".into(),
        model: "gigaam".into(),
        language: "ru".into(),
        device: String::new(),
        base_url: "https://speech.example.net/v1".into(),
        allow_insecure_http: false,
        credential: "replace".into(),
        key: KEY.into(),
    }
}

fn local_form(name: &str) -> EditorForm {
    EditorForm {
        name: name.into(),
        id: "russian".into(),
        adapter: "gigaam".into(),
        model: "v3_e2e_rnnt".into(),
        language: "ru".into(),
        device: "cpu".into(),
        credential: "keep".into(),
        ..EditorForm::default()
    }
}

fn editor(providers: &Providers) -> EditorView {
    providers.view().editor.expect("the editor is open")
}

/// Saves `form` in the open editor and sends it: the call made.
fn save_and_send(providers: &mut Providers, form: EditorForm) -> Call {
    let ticket = editor(providers).ticket;
    let effects = providers.save(ticket, form);
    assert!(
        matches!(effects.as_slice(), [Effect::StorePending(_)]),
        "{effects:?}"
    );
    let sent = providers.stored(true, now());
    calls(&sent).remove(0)
}

fn completed(call: &Call, extra: Value) -> Answer {
    let mut answer = map(json!({
        "schema": 1, "state": "completed",
        "machine_id": call.payload["expected_machine_id"],
        "request_id": call.payload["request_id"],
    }));
    answer.extend(map(extra));
    Answer {
        reply: answer,
        error: String::new(),
    }
}

#[test]
fn the_default_provider_is_restored_and_kept_while_browsing() {
    let defaults = Defaults {
        machine_id: MACHINE.into(),
        profile: "russian".into(),
        host: "speech-host".into(),
    };
    let mut providers = page("speech-host", defaults, "");
    providers.set_peers(vec!["speech-host".into(), "other-host".into()]);
    assert_eq!(
        providers
            .view()
            .hosts
            .iter()
            .map(|host| host.value.as_str())
            .collect::<Vec<_>>(),
        ["", "speech-host", "other-host"]
    );
    let effects = providers.refresh();
    assert!(calls(&effects)
        .iter()
        .all(|call| call.host == "speech-host"));
    providers.answered(
        CallId::List(generation(&effects)),
        listing(json!([
            {"id": "first", "name": "First", "available": true},
            {"id": "russian", "name": "Russian", "available": true},
            {"id": "offline", "name": "Offline", "available": false},
        ])),
        now(),
    );
    let view = providers.view();
    assert_eq!(view.profiles.len(), 3);
    assert_eq!(view.selected, Some(1));
    assert_eq!(view.default_text, "Default for new recordings: Russian");
    assert_eq!(view.profiles[2].readiness, "Not ready");

    providers.select(0);
    assert_eq!(
        providers.use_default(),
        [Effect::Set(vec![
            ("speech/machine_id", json!(MACHINE)),
            ("speech/profile", json!("first")),
            ("speech/host", json!("speech-host")),
            ("speech/profile_revision", json!(0)),
        ])]
    );
    providers.select(2);
    assert!(providers.view().can_edit);

    let effects = providers.choose_host("other-host");
    assert_eq!(
        effects[0],
        Effect::Set(vec![("speech/manage_host", json!("other-host"))])
    );
    assert!(calls(&effects).iter().all(|call| call.host == "other-host"));
    assert!(!providers.view().loaded);
    // Browsing writes no default.
    assert!(!effects
        .iter()
        .any(|effect| matches!(effect, Effect::Set(set) if set.iter().any(|(key, _)| *key == "speech/profile"))));
}

#[test]
fn an_older_hgs_refuses_management_until_it_is_updated() {
    let mut providers = page("", Defaults::default(), "");
    let effects = providers.refresh();
    providers.answered(
        CallId::List(generation(&effects)),
        Answer {
            reply: map(json!({"client_dispatch_started": false})),
            error: "Update hgs on this computer to manage speech providers.".into(),
        },
        now(),
    );
    let view = providers.view();
    assert!(view.can_refresh);
    assert!(!view.can_add);
    assert!(view.status.contains("Update hgs"));

    loaded(&mut providers, json!([russian()]));
    let view = providers.view();
    assert!(view.can_add);
    assert_eq!(view.profiles.len(), 1);
}

#[test]
fn an_earlier_refresh_answers_nothing() {
    let mut providers = page("", Defaults::default(), "");
    let first = providers.refresh();
    let second = providers.refresh();
    providers.answered(
        CallId::List(generation(&first)),
        listing(json!([russian()])),
        now(),
    );
    assert!(!providers.view().loaded);
    providers.answered(
        CallId::Stats(generation(&first)),
        reply(json!({"schema": 1})),
        now(),
    );
    assert_eq!(
        providers.view().statistics.map(|t| t.english()).as_deref(),
        Some("Loading this computer's voice statistics…")
    );
    providers.answered(CallId::List(generation(&second)), listing(json!([])), now());
    let view = providers.view();
    assert!(view.loaded);
    assert_eq!(
        view.status,
        "No providers yet. Add Local GigaAM or an OpenAI-compatible server."
    );
}

#[test]
fn an_invalid_listing_is_not_loaded() {
    for listing in [
        json!({"schema": 1, "machine_id": "not-a-machine", "revision": 1}),
        json!({"schema": 1, "machine_id": MACHINE, "revision": -1}),
        json!({"schema": 1, "machine_id": MACHINE, "revision": 1.5}),
        json!({"schema": 1, "machine_id": MACHINE, "revision": "1"}),
    ] {
        let mut providers = page("", Defaults::default(), "");
        let effects = providers.refresh();
        providers.answered(CallId::List(generation(&effects)), reply(listing), now());
        let view = providers.view();
        assert!(!view.loaded);
        assert_eq!(
            view.status,
            "The computer returned an invalid provider configuration. Update hgs and refresh."
        );
    }
}

#[test]
fn a_remote_provider_is_created_without_keeping_its_key() {
    let mut providers = page("", Defaults::default(), "");
    loaded(&mut providers, json!([russian()]));
    assert!(providers.open_editor(true));
    let view = editor(&providers);
    assert_eq!(view.title, "Add speech provider");
    assert_eq!(view.computer, "Processing computer: This computer (Linux)");
    assert!(view.start.id.starts_with("provider-"));
    assert_eq!(view.start.language, "ru");
    assert_eq!(view.start.adapter, "gigaam");
    assert_eq!(view.start.device, "cpu");

    let ticket = view.ticket;
    let stored = providers.save(ticket, remote_form());
    let [Effect::StorePending(pending)] = stored.as_slice() else {
        panic!("{stored:?}");
    };
    assert!(!pending.contains(KEY));
    assert!(!format!("{stored:?}").contains(KEY));
    let pending: Value = serde_json::from_str(pending).unwrap();
    assert_eq!(pending["action"], "profile_upsert");
    assert_eq!(pending["machine_id"], MACHINE);
    assert!(editor(&providers).busy);

    let sent = providers.stored(true, now());
    let call = calls(&sent).remove(0);
    assert!(!format!("{sent:?}").contains(KEY));
    assert_eq!(call.id, CallId::Change);
    assert_eq!(call.payload["request_id"], pending["request_id"]);
    assert_eq!(call.payload["expected_machine_id"], MACHINE);
    assert_eq!(call.payload["expected_revision"], 1);
    assert_eq!(
        call.payload["profile"]["base_url"],
        "https://speech.example.net/v1"
    );
    assert!(call.payload["profile"].get("device").is_none());
    assert_eq!(
        call.payload["credential"],
        json!({"mode": "replace", "value": KEY})
    );

    let effects = providers.answered(
        CallId::Change,
        completed(&call, json!({"profile_id": "remote-fixture"})),
        now(),
    );
    assert_eq!(effects[0], Effect::ClearPending);
    assert!(providers.view().editor.is_none());
    assert_eq!(
        providers.view().result,
        "Provider configuration saved on This computer (Linux)."
    );
    let generation = generation(&effects);
    providers.answered(
        CallId::List(generation),
        listing(
            json!([russian(), {"id": "remote-fixture", "name": "Remote fixture",
            "adapter": "openai_http", "revision": 2, "available": true}]),
        ),
        now(),
    );
    assert_eq!(providers.view().selected, Some(1));
    let set = providers.use_default();
    assert!(format!("{set:?}").contains(r#"("speech/profile_revision", Number(2))"#));
}

#[test]
fn the_editor_form_follows_qts_rules() {
    let adapters = vec![
        AdapterView {
            id: "gigaam".into(),
            label: "Local GigaAM".into(),
            models: vec!["v3_e2e_rnnt".into()],
            devices: vec!["cpu".into()],
        },
        AdapterView {
            id: "openai_http".into(),
            label: "OpenAI-compatible server".into(),
            models: vec![],
            devices: vec![],
        },
    ];
    assert!(remote_form().valid(&adapters));
    let with = |change: &dyn Fn(&mut EditorForm)| {
        let mut form = remote_form();
        change(&mut form);
        form.valid(&adapters)
    };
    assert!(!with(
        &|form| form.base_url = "http://127.0.0.1:9000/v1".into()
    ));
    assert!(with(&|form| {
        form.base_url = "http://127.0.0.1:9000/v1".into();
        form.allow_insecure_http = true;
    }));
    assert!(!with(
        &|form| form.base_url = "https://user:secret@example.net/v1".into()
    ));
    assert!(!with(
        &|form| form.base_url = "https://speech.example.net/v1?key=secret".into()
    ));
    assert!(!with(
        &|form| form.base_url = "https://speech.example.net/v1#part".into()
    ));
    assert!(!with(&|form| form.base_url = "https:///v1".into()));
    assert!(!with(
        &|form| form.base_url = "https://speech example.net/v1".into()
    ));
    assert!(!with(&|form| form.key.clear()));
    assert!(with(&|form| {
        form.key.clear();
        form.credential = "keep".into();
    }));
    assert!(!with(&|form| form.credential = "forget".into()));
    assert!(!with(&|form| form.id = "has space".into()));
    assert!(!with(&|form| form.id = "x".repeat(65)));
    assert!(!with(&|form| form.name = "   ".into()));
    assert!(!with(&|form| form.language = "RU".into()));
    assert!(with(&|form| form.language.clear()));
    assert!(local_form("Russian").valid(&adapters));
    assert!(!EditorForm {
        model: "unknown".into(),
        ..local_form("Russian")
    }
    .valid(&adapters));
    assert!(!EditorForm {
        device: "mps".into(),
        ..local_form("Russian")
    }
    .valid(&adapters));
    assert!(endpoint_valid("https://[::1]:8443/v1", false));
    assert!(!endpoint_valid(
        "https://speech.example.net:99999/v1",
        false
    ));
    assert!(!endpoint_valid("https://speech.example.net/%zz", false));

    for (mode, value) in [("keep", false), ("remove", false), ("replace", true)] {
        let secret = EditorForm {
            credential: mode.into(),
            ..remote_form()
        }
        .secret();
        assert_eq!(secret["mode"], mode);
        assert_eq!(secret.get("value").is_some(), value);
    }
    // A local provider keeps no key.
    assert_eq!(local_form("Russian").secret(), json!({"mode": "remove"}));
    assert!(!format!("{:?}", remote_form()).contains(KEY));
}

#[test]
fn a_save_stays_pinned_to_its_computer_while_browsing_another() {
    let defaults = Defaults::default();
    let mut providers = page("", defaults, "");
    providers.set_peers(vec!["other-host".into()]);
    loaded(&mut providers, json!([russian()]));
    assert!(providers.open_editor(false));
    let call = save_and_send(&mut providers, local_form("Only original computer"));
    assert_eq!(call.host, "");
    assert_eq!(call.payload["expected_machine_id"], MACHINE);

    let browse = providers.choose_host("other-host");
    providers.answered(
        CallId::List(generation(&browse)),
        listing(json!([russian()])),
        now(),
    );
    assert_eq!(providers.view().host, "other-host");
    let effects = providers.answered(CallId::Change, completed(&call, json!({})), now());
    // The page keeps showing the other computer.
    assert_eq!(effects, [Effect::ClearPending]);
    assert_eq!(providers.view().host, "other-host");
    assert!(providers.view().editor.is_none());
}

#[test]
fn a_conflict_keeps_the_form_and_never_retries() {
    let mut providers = page("", Defaults::default(), "");
    loaded(&mut providers, json!([russian()]));
    assert!(providers.open_editor(false));
    let call = save_and_send(&mut providers, local_form("My unsaved edit"));
    let mut conflict = completed(
        &call,
        json!({"state": "failed", "error_code": "revision_conflict"}),
    );
    conflict
        .reply
        .insert("error".into(), json!("Revision conflict"));
    let effects = providers.answered(CallId::Change, conflict, now());
    assert!(effects.contains(&Effect::ClearPending));
    let view = editor(&providers);
    assert!(view.conflict);
    assert!(!view.busy);
    assert!(view.status.contains("configuration changed"));
    assert!(providers
        .save(view.ticket, local_form("My unsaved edit"))
        .is_empty());
}

#[test]
fn a_lost_save_is_checked_by_its_receipt_after_a_restart() {
    let mut providers = page("", Defaults::default(), "");
    loaded(&mut providers, json!([russian()]));
    assert!(providers.open_editor(false));
    let ticket = editor(&providers).ticket;
    let stored = providers.save(ticket, local_form("Saved despite lost response"));
    let [Effect::StorePending(pending)] = stored.as_slice() else {
        panic!("{stored:?}");
    };
    let pending = pending.clone();
    let call = calls(&providers.stored(true, now())).remove(0);
    let effects = providers.answered(
        CallId::Change,
        Answer::failed(
            "The provider request could not be completed. Check its status before trying again.",
        ),
        now(),
    );
    assert!(effects.is_empty());
    let view = editor(&providers);
    assert!(view.status.contains("not confirmed"));
    assert!(view.check_visible && view.check_enabled);
    assert!(providers.save(ticket, local_form("Again")).is_empty());

    let mut restored = page("", Defaults::default(), &pending);
    let view = restored.view();
    assert!(view.pending.contains("not yet confirmed"));
    loaded(&mut restored, json!([russian()]));
    assert!(!restored.view().can_add);
    assert!(!restored.open_editor(true));
    let check = calls(&restored.check(now())).remove(0);
    assert_eq!(check.id, CallId::Status);
    assert_eq!(check.payload["action"], "config_status");
    assert_eq!(check.payload["operation_id"], call.payload["request_id"]);
    let effects = restored.answered(CallId::Status, completed(&call, json!({})), now());
    assert_eq!(effects[0], Effect::ClearPending);
    assert!(restored.view().pending.is_empty());
}

#[test]
fn an_unknown_receipt_stays_pending_without_a_replay() {
    let pending = json!({"host": "", "computer": "This computer", "machine_id": MACHINE,
        "request_id": "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa", "action": "profile_upsert"});
    let mut providers = page("", Defaults::default(), &pending.to_string());
    let check = calls(&providers.check(now())).remove(0);
    let effects = providers.answered(
        CallId::Status,
        reply(
            json!({"schema": 1, "machine_id": MACHINE, "request_id": pending["request_id"],
            "state": "failed", "error_code": "operation_not_found"}),
        ),
        now(),
    );
    assert!(effects.is_empty());
    assert_eq!(check.payload["operation_id"], pending["request_id"]);
    let view = providers.view();
    assert!(view.pending.contains("not yet confirmed"));
    assert!(view.can_check);
    assert!(!view.can_add);
}

#[test]
fn a_stored_change_without_a_valid_identity_is_ignored() {
    for pending in [
        json!({"machine_id": "BBBBBBBB-BBBB-4BBB-BBBB-BBBBBBBBBBBB", "request_id": "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa"}),
        json!({"machine_id": MACHINE, "request_id": "00000000-0000-0000-0000-000000000000"}),
        json!({"machine_id": MACHINE}),
        json!([MACHINE]),
    ] {
        let providers = page("", Defaults::default(), &pending.to_string());
        assert!(providers.view().pending.is_empty());
    }
}

#[test]
fn remove_asks_first_and_keeps_an_unavailable_default() {
    let defaults = Defaults {
        machine_id: MACHINE.into(),
        profile: "russian".into(),
        host: String::new(),
    };
    let mut providers = page("", defaults, "");
    loaded(&mut providers, json!([russian()]));
    let ask = providers.ask_remove().unwrap();
    assert!(ask
        .text
        .starts_with("Remove Russian from This computer (Linux)?"));
    assert!(providers.remove(ask.ticket, false).is_empty());
    let ask = providers.ask_remove().unwrap();
    let stored = providers.remove(ask.ticket, true);
    assert!(matches!(stored.as_slice(), [Effect::StorePending(_)]));
    let call = calls(&providers.stored(true, now())).remove(0);
    assert_eq!(call.payload["action"], "profile_remove");
    assert_eq!(call.payload["profile_id"], "russian");
    assert_eq!(call.payload["expected_revision"], 1);
    let effects = providers.answered(CallId::Change, completed(&call, json!({})), now());
    assert!(!effects
        .iter()
        .any(|effect| matches!(effect, Effect::Set(_))));
    providers.answered(
        CallId::List(generation(&effects)),
        listing(json!([])),
        now(),
    );
    assert!(providers.view().default_text.contains("unavailable"));
}

#[test]
fn a_confirmation_from_before_a_change_removes_nothing() {
    let mut providers = page("", Defaults::default(), "");
    providers.set_peers(vec!["other-host".into()]);
    loaded(&mut providers, json!([russian()]));
    let ask = providers.ask_remove().unwrap();
    let browse = providers.choose_host("other-host");
    providers.answered(
        CallId::List(generation(&browse)),
        listing(json!([russian()])),
        now(),
    );
    assert!(providers.remove(ask.ticket, true).is_empty());
}

#[test]
fn test_pins_the_saved_profile_revision() {
    let mut providers = page("", Defaults::default(), "");
    loaded(&mut providers, json!([russian()]));
    assert!(matches!(
        providers.test().as_slice(),
        [Effect::StorePending(_)]
    ));
    let call = calls(&providers.stored(true, now())).remove(0);
    assert_eq!(call.payload["action"], "profile_test");
    assert_eq!(call.payload["profile_id"], "russian");
    assert_eq!(call.payload["expected_profile_revision"], 1);
    assert!(call.payload.get("credential").is_none());
    assert!(call.payload.get("audio").is_none());
    providers.answered(CallId::Change, completed(&call, json!({})), now());
    assert_eq!(
        providers.view().result,
        "The provider test on This computer (Linux) completed successfully."
    );
    assert!(!providers.view().result_failed);
}

#[test]
fn a_running_test_polls_its_receipt_without_another_change() {
    let mut providers = page("", Defaults::default(), "");
    loaded(&mut providers, json!([russian()]));
    providers.test();
    let start = now();
    let call = calls(&providers.stored(true, start)).remove(0);
    let running = completed(&call, json!({"state": "running"}));
    let effects = providers.answered(CallId::Change, running.clone(), start);
    let request = call.payload["request_id"].as_str().unwrap().to_owned();
    assert_eq!(effects, [Effect::PollLater(request.clone())]);
    assert!(providers
        .view()
        .pending
        .starts_with("Working on the provider request"));
    assert!(providers.poll("another-request", start).is_empty());
    let check = calls(&providers.poll(&request, start)).remove(0);
    assert_eq!(check.id, CallId::Status);
    assert!(
        providers.poll(&request, start).is_empty(),
        "one check at a time"
    );
    let effects = providers.answered(CallId::Status, completed(&call, json!({})), start);
    assert_eq!(effects[0], Effect::ClearPending);
    assert!(providers.view().result.contains("completed successfully"));
    providers.answered(
        CallId::List(generation(&effects)),
        listing(json!([russian()])),
        start,
    );

    // Polling stops after 90 seconds; the change stays to check by hand.
    providers.test();
    let call = calls(&providers.stored(true, start)).remove(0);
    let late = start + Duration::from_secs(91);
    let effects = providers.answered(
        CallId::Change,
        completed(&call, json!({"state": "running"})),
        late,
    );
    assert!(effects.is_empty());
    assert!(providers.view().pending.contains("not yet confirmed"));
}

#[test]
fn a_change_whose_identity_cannot_be_stored_is_not_sent() {
    let mut providers = page("", Defaults::default(), "");
    loaded(&mut providers, json!([russian()]));
    providers.test();
    assert_eq!(providers.stored(false, now()), [Effect::ClearPending]);
    let view = providers.view();
    assert!(view.pending.is_empty());
    assert_eq!(
        view.result,
        "Could not save the operation identity. No provider request was sent."
    );
    assert!(view.result_failed);
    assert!(view.can_test);

    assert!(providers.open_editor(false));
    let ticket = editor(&providers).ticket;
    providers.save(ticket, local_form("Russian"));
    assert_eq!(providers.stored(false, now()), [Effect::ClearPending]);
    let view = editor(&providers);
    assert!(!view.busy);
    assert!(!view.check_visible);
    assert!(view
        .status
        .starts_with("Could not save the operation identity"));
}

#[test]
fn a_refusal_before_sending_fails_the_change() {
    let mut providers = page("", Defaults::default(), "");
    loaded(&mut providers, json!([russian()]));
    providers.test();
    providers.stored(true, now());
    let effects = providers.answered(
        CallId::Change,
        Answer {
            reply: map(json!({"client_dispatch_started": false})),
            error: "Update hgs on this computer to manage speech providers.".into(),
        },
        now(),
    );
    assert_eq!(effects[0], Effect::ClearPending);
    assert_eq!(
        providers.view().result,
        "Update hgs on this computer to manage speech providers."
    );
}

#[test]
fn the_editor_offers_saved_values_the_computer_no_longer_has() {
    let mut providers = page("", Defaults::default(), "");
    loaded(
        &mut providers,
        json!([{"id": "old", "name": "Old", "adapter": "gigaam", "model": "v2", "device": "mps",
            "available": true}, {"id": "future", "name": "Future", "adapter": "whisper"},
            {"id": "remote", "name": "Remote", "adapter": "openai_http", "model": "big",
            "base_url": "https://speech.example.net/v1", "credential": {"configured": true}}]),
    );
    assert!(providers.open_editor(false));
    let view = editor(&providers);
    assert_eq!(view.extra_model.as_deref(), Some("v2"));
    assert_eq!(view.extra_device.as_deref(), Some("mps"));
    assert_eq!(view.unsupported, None);
    assert_eq!(view.title, "Edit speech provider");
    providers.close_editor(view.ticket);

    providers.select(1);
    assert!(providers.open_editor(false));
    assert_eq!(editor(&providers).unsupported.as_deref(), Some("whisper"));
    providers.close_editor(editor(&providers).ticket);

    providers.select(2);
    assert!(providers.open_editor(false));
    let view = editor(&providers);
    assert!(view.key_stored);
    assert_eq!(view.start.model, "big");
    assert_eq!(view.extra_model, None);
}

#[test]
fn a_duplicate_new_id_is_refused_before_sending() {
    let mut providers = page("", Defaults::default(), "");
    loaded(&mut providers, json!([russian()]));
    assert!(providers.open_editor(true));
    let ticket = editor(&providers).ticket;
    assert!(providers.save(ticket, local_form("Another")).is_empty());
    assert_eq!(
        editor(&providers).status,
        "That profile ID already exists. Edit the existing provider or choose another ID."
    );
}

#[test]
fn statistics_read_as_qt_writes_them() {
    let text = statistics_text(&map(json!({
        "retention_days": 30.0,
        "totals": {"submitted": 4, "completed": 3, "failed": 1, "cancelled": 0,
            "uncertain": 0, "audio_ms": 90000, "processing_ms": 4250},
    })));
    assert_eq!(
        text.english(),
        "This computer (last 30 days)\n4 recordings, 3 completed, 1 failed, 0 cancelled, 0 uncertain\n1.5 minutes of audio, 4.2 seconds processing\nThese counters stay on the speech computer and contain no audio or recognized text."
    );
    assert!(statistics_text(&Map::new())
        .english()
        .starts_with("This computer (last 90 days)\n0 recordings"));
}

#[test]
fn documentation_links_follow_the_provider_type() {
    assert_eq!(
        documentation_url("gigaam"),
        "https://github.com/ufna/zerus/blob/main/docs/speech-setup.md#local-gigaam"
    );
    assert_eq!(
        documentation_url("openai_http"),
        "https://github.com/ufna/zerus/blob/main/docs/speech-setup.md#openai-compatible-server"
    );
    assert_eq!(
        documentation_url(""),
        "https://github.com/ufna/zerus/blob/main/docs/speech-setup.md"
    );
}

/// A test's folder, removed with everything written there (a payload may
/// hold the synthetic key), also when the test fails.
#[cfg(unix)]
struct Folder(std::path::PathBuf);

#[cfg(unix)]
impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn polling_stops_after_60_checks_within_90_seconds() {
    let mut providers = page("", Defaults::default(), "");
    loaded(&mut providers, json!([russian()]));
    providers.test();
    let start = now();
    let call = calls(&providers.stored(true, start)).remove(0);
    let request = call.payload["request_id"].as_str().unwrap().to_owned();
    let running = completed(&call, json!({"state": "running"}));
    let mut effects = providers.answered(CallId::Change, running.clone(), start);
    let mut polls = 0;
    while effects == [Effect::PollLater(request.clone())] {
        polls += 1;
        assert_eq!(calls(&providers.poll(&request, start)).len(), 1);
        effects = providers.answered(CallId::Status, running.clone(), start);
    }
    assert_eq!(polls, 60);
    assert!(effects.is_empty());
    assert!(providers.view().pending.contains("not yet confirmed"));
}

#[test]
fn a_remove_confirmed_after_any_change_sends_nothing() {
    // A newer revision, another machine, or a change under way.
    for change in ["revision", "machine", "pending"] {
        let mut providers = page("", Defaults::default(), "");
        loaded(&mut providers, json!([russian()]));
        let ask = providers.ask_remove().unwrap();
        match change {
            "revision" | "machine" => {
                let effects = providers.refresh();
                let mut listing = listing(json!([russian()]));
                if change == "revision" {
                    listing.reply.insert("revision".into(), json!(2));
                } else {
                    listing.reply.insert(
                        "machine_id".into(),
                        json!("cccccccc-cccc-4ccc-cccc-cccccccccccc"),
                    );
                }
                providers.answered(CallId::List(generation(&effects)), listing, now());
            }
            _ => {
                providers.test();
                providers.stored(true, now());
            }
        }
        assert!(providers.remove(ask.ticket, true).is_empty(), "{change}");
    }
}

#[test]
fn the_documentation_failure_names_the_link() {
    assert_eq!(
        documentation_failure("https://github.com/ufna/zerus/blob/main/docs/speech-setup.md#local-gigaam"),
        "Could not open the browser. Open this link manually:\nhttps://github.com/ufna/zerus/blob/main/docs/speech-setup.md#local-gigaam"
    );
}

#[test]
fn cancel_leaves_no_change_behind() {
    let mut providers = page("", Defaults::default(), "");
    loaded(&mut providers, json!([russian()]));
    assert!(providers.open_editor(true));
    providers.close_editor(editor(&providers).ticket);
    let view = providers.view();
    assert!(view.editor.is_none());
    assert!(view.pending.is_empty());
    assert!(view.can_add);
}

#[test]
fn an_edit_with_a_stored_key_keeps_or_removes_it_and_needs_consent_for_http() {
    for mode in ["keep", "remove"] {
        let mut providers = page("", Defaults::default(), "");
        loaded(
            &mut providers,
            json!([{"id": "remote", "name": "Remote", "available": true, "adapter": "openai_http", "model": "gigaam",
                "base_url": "https://speech.example.net/v1", "credential": {"configured": true}, "revision": 1}]),
        );
        assert!(providers.open_editor(false));
        let view = editor(&providers);
        assert!(view.key_stored);
        let form = |url: &str, insecure: bool| EditorForm {
            name: "Remote".into(),
            id: "remote".into(),
            adapter: "openai_http".into(),
            model: "gigaam".into(),
            language: "ru".into(),
            base_url: url.into(),
            allow_insecure_http: insecure,
            credential: mode.into(),
            ..EditorForm::default()
        };
        assert!(providers
            .save(view.ticket, form("http://127.0.0.1:9000/v1", false))
            .is_empty());
        assert!(providers
            .save(
                view.ticket,
                form("https://user:secret@example.net/v1", false)
            )
            .is_empty());
        assert!(providers
            .save(
                view.ticket,
                form("https://speech.example.net/v1?key=secret", false)
            )
            .is_empty());
        let call = save_and_send(&mut providers, form("http://127.0.0.1:9000/v1", true));
        assert_eq!(call.payload["credential"], json!({"mode": mode}));
        assert_eq!(call.payload["profile"]["allow_insecure_http"], true);
    }
}

#[test]
fn a_refused_store_clears_the_record_and_sends_nothing() {
    let mut providers = page("", Defaults::default(), "");
    loaded(&mut providers, json!([russian()]));
    providers.test();
    let effects = providers.stored(false, now());
    assert_eq!(effects, [Effect::ClearPending]);
    assert!(providers.view().pending.is_empty());
}

/// The src-tauri glue: the record goes into the store under the page's
/// lock, and only a write that reached the disk lets the change go.
#[cfg(unix)]
#[test]
fn a_record_the_disk_refuses_is_cleared_and_the_change_not_sent() {
    use std::os::unix::fs::PermissionsExt;
    let folder =
        Folder(std::env::temp_dir().join(format!("zerus-speech-store-{}", std::process::id())));
    let dir = &folder.0;
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    let settings = Settings::open(dir.join("settings.json"), None);
    let mut providers = page("", Defaults::default(), "");
    loaded(&mut providers, json!([russian()]));
    let effects = providers.test();
    let [Effect::StorePending(text)] = effects.as_slice() else {
        panic!("a change starts by storing its record");
    };
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let refused = std::fs::write(dir.join("probe"), b"").is_err();
    assert!(keep_pending(&settings, text));
    let written = pending_written(&settings);
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    if refused {
        assert!(!written);
        let effects = providers.stored(written, now());
        assert_eq!(effects, [Effect::ClearPending]);
        clear_pending(&settings);
        assert_eq!(settings.text(PENDING_KEY), "");
    }
    // Written before the folder goes: a store still writing would make it
    // again after the guard removed it.
    assert!(pending_written(&settings));
}

#[test]
fn test_and_remove_omit_what_the_profile_lacks() {
    let mut providers = page("", Defaults::default(), "");
    loaded(
        &mut providers,
        json!([{"name": "No id", "adapter": "gigaam"}]),
    );
    providers.test();
    let call = calls(&providers.stored(true, now())).remove(0);
    assert!(call.payload.get("profile_id").is_none());
    assert!(call.payload.get("expected_profile_revision").is_none());
    providers.answered(CallId::Change, completed(&call, json!({})), now());
    let effects = providers.refresh();
    providers.answered(
        CallId::List(generation(&effects)),
        listing(json!([{"name": "No id", "adapter": "gigaam"}])),
        now(),
    );
    let ask = providers.ask_remove().unwrap();
    providers.remove(ask.ticket, true);
    let call = calls(&providers.stored(true, now())).remove(0);
    assert_eq!(call.payload["action"], "profile_remove");
    assert!(call.payload.get("profile_id").is_none());
}

#[test]
fn every_view_is_newer_than_the_last() {
    let providers = page("", Defaults::default(), "");
    let first = providers.view().seq;
    assert!(providers.view().seq > first);
}

#[cfg(unix)]
mod transport {
    use super::*;
    use std::path::PathBuf;

    /// A folder with `hgs`, a symlink to the checked-in fake. A symlink,
    /// never a copy: writing an executable while another test thread forks
    /// lets the child inherit the write fd, and exec then fails with ETXTBSY.
    fn fixture(name: &str, management: bool) -> (Folder, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "zerus-speech-settings-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        if !management {
            std::fs::write(dir.join("legacy"), "").unwrap();
        }
        let hgs = dir.join("hgs");
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_speech_hgs.sh");
        std::os::unix::fs::symlink(script, &hgs).unwrap();
        (Folder(dir), hgs)
    }

    #[test]
    fn management_needs_its_own_marker_before_a_key_is_sent() {
        let (folder, hgs) = fixture("legacy", false);
        let dir = &folder.0;
        let mut supported = HashSet::new();
        let payload =
            json!({"action": "profile_upsert", "credential": {"mode": "replace", "value": KEY}});
        let answer = run(&hgs, "speech-host", &payload, &mut supported);
        assert_eq!(
            answer.error,
            "Update hgs on this computer to manage speech providers."
        );
        assert_eq!(
            answer.reply.get("client_dispatch_started"),
            Some(&json!(false))
        );
        assert!(!dir.join("payload").exists());
        // Statistics need only `speech --json`.
        let answer = run(
            &hgs,
            "speech-host",
            &json!({"action": "stats"}),
            &mut supported,
        );
        assert_eq!(answer.error, "");
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert_eq!(
            calls,
            "@speech-host --help\n@speech-host --help\n@speech-host speech --json\n"
        );
    }

    #[test]
    fn a_change_goes_on_standard_input_once_the_help_names_management() {
        let (folder, hgs) = fixture("current", true);
        let dir = &folder.0;
        let mut supported = HashSet::new();
        let payload =
            json!({"action": "profile_upsert", "credential": {"mode": "replace", "value": KEY}});
        let answer = run(&hgs, "", &payload, &mut supported);
        assert_eq!(answer.error, "");
        assert_eq!(answer.reply.get("state"), Some(&json!("completed")));
        assert!(answer.reply.get("client_dispatch_started").is_none());
        let sent: Value =
            serde_json::from_slice(&std::fs::read(dir.join("payload")).unwrap()).unwrap();
        assert_eq!(sent, payload);
        run(&hgs, "", &json!({"action": "config_list"}), &mut supported);
        run(&hgs, "", &json!({"action": "stats"}), &mut supported);
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert!(!calls.contains(KEY));
        assert_eq!(calls.matches("--help").count(), 1, "{calls}");
    }
}
