use super::*;
use serde_json::json;

fn uuid(n: usize) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}
fn context() -> Context {
    Context {
        quit_id: uuid(1),
        configuration_sha256: "a".repeat(64),
        gui: GuiBirth {
            pid: 42,
            birth: "fixture-birth".into(),
        },
        platform: Platform::Linux,
        boot: uuid(2),
        quit_confirmed_ns: 100,
        cutoff_ns: 100 + QUIT_NS,
    }
}
fn now() -> Observation {
    Observation {
        platform: Platform::Linux,
        clock: Clock {
            boot: uuid(2),
            ns: 101,
        },
    }
}
fn descriptor(n: usize, ordinal: u32, action: Action) -> Descriptor {
    Descriptor {
        ordinal,
        request_id: uuid(10 + n),
        captured: Captured {
            target: Target {
                computer: String::new(),
                name: format!("codex/example/{n}"),
                run_id: uuid(1_000_000 + n),
                conversation_id: uuid(2_000_000 + n),
            },
            action,
            state: "running".into(),
        },
        removal_choice: RemovalChoice::Keep,
        cleanup: None,
        restriction: CleanupRestriction::KeepForReview,
    }
}
fn record(n: usize) -> Record {
    let c = context();
    let g = Group::new(
        &c,
        uuid(3),
        0,
        Action::Terminate,
        (0..n)
            .map(|n| descriptor(n, n as u32, Action::Terminate))
            .collect(),
    )
    .unwrap();
    Record::new(c, vec![g]).unwrap()
}
fn global() -> Global {
    Global {
        version: VERSION,
        revision: 0,
        usage: Usage::default(),
        slot: None,
    }
}
struct Empty {
    calls: usize,
    conflict: Option<String>,
    fail: bool,
    target_conflict: bool,
}
impl Retained for Empty {
    fn request(&mut self, id: &str) -> Result<bool, Error> {
        self.calls += 1;
        if self.fail {
            return Err(Error::Unavailable);
        }
        Ok(self.conflict.as_deref() == Some(id))
    }
    fn target(&mut self, _: &Target) -> Result<bool, Error> {
        self.calls += 1;
        Ok(self.target_conflict)
    }
}
fn retained() -> Empty {
    Empty {
        calls: 0,
        conflict: None,
        fail: false,
        target_conflict: false,
    }
}
fn reserved(n: usize) -> ReservePlan {
    reserve(record(n), &global(), &now(), &mut retained()).unwrap()
}
fn worker(plan: &ReservePlan) -> Global {
    let mut g = plan.after.clone();
    g.slot.as_mut().unwrap().phase = SlotPhase::WorkerObserved;
    g
}
fn pending(r: &Record) -> MemberState {
    MemberState {
        binding: r.member_binding(0, 0).unwrap(),
        phase: MemberPhase::Pending,
    }
}

#[test]
fn fifty_thousand_real_uuid_targets_fit_whole_capture_budget() {
    let r = record(50_000);
    eprintln!(
        "50k immutable capture bytes={} of {}",
        r.capture_bytes(),
        CAPTURE_BYTES
    );
    assert!(r.capture_bytes() <= CAPTURE_BYTES);
    let mut lookup = retained();
    let plan = reserve(r, &global(), &now(), &mut lookup).unwrap();
    assert_eq!(lookup.calls, 100_000);
    assert_eq!(plan.progress[0].total, 50_000);
    assert_eq!(plan.after.usage.rows, 50_000);
    assert_eq!(plan.progress[0].not_issued, 50_000);
    assert!(plan.after.usage.bytes <= CHARGED_BYTES);
    assert_eq!(plan.after.usage.exchanges, 0);
    // Exercise the actual whole serialized limit, including descriptors/hashes,
    // rather than a separate estimate of capture strings or SQLite row charge.
    let mut descriptors: Vec<_> = plan.record.groups()[0]
        .members
        .iter()
        .map(|m| m.descriptor.clone())
        .collect();
    let mut left = CAPTURE_BYTES - plan.record.capture_bytes();
    for d in &mut descriptors {
        let add = left.min(512);
        d.captured.target.computer = "a".repeat(add);
        left -= add;
    }
    assert_eq!(left, 0);
    let c = context();
    let g = Group::new(&c, uuid(3), 0, Action::Terminate, descriptors.clone()).unwrap();
    let exact = Record::new(c.clone(), vec![g]).unwrap();
    assert_eq!(exact.capture_bytes(), CAPTURE_BYTES);
    eprintln!(
        "Exact whole-record boundary accepted={} bytes",
        exact.capture_bytes()
    );
    descriptors
        .iter_mut()
        .find(|d| d.captured.target.computer.len() < 512)
        .unwrap()
        .captured
        .target
        .computer
        .push('a');
    let g = Group::new(&c, uuid(3), 0, Action::Terminate, descriptors).unwrap();
    assert_eq!(Record::new(c, vec![g]).unwrap_err(), Error::TooLarge);
}
#[test]
fn per_group_and_group_count_limits_are_whole_rejections() {
    let c = context();
    let descriptors = (0..50_001)
        .map(|n| descriptor(n, n as u32, Action::Terminate))
        .collect();
    assert_eq!(
        Group::new(&c, uuid(3), 0, Action::Terminate, descriptors).unwrap_err(),
        Error::TooMany
    );
    let groups = |count| {
        (0..count)
            .map(|n| {
                Group::new(
                    &c,
                    uuid(100 + n),
                    n as u32,
                    Action::Terminate,
                    vec![descriptor(n, 0, Action::Terminate)],
                )
                .unwrap()
            })
            .collect()
    };
    assert!(Record::new(c.clone(), groups(5)).is_ok());
    assert_eq!(
        Record::new(c.clone(), groups(6)).unwrap_err(),
        Error::TooMany
    );
}
#[test]
fn byte_counter_includes_full_descriptor_cleanup_and_exact_boundary() {
    let r = record(1);
    assert_eq!(r.capture_bytes(), serde_json::to_vec(&r).unwrap().len());
    let mut count = Count { bytes: 0, limit: 4 };
    assert_eq!(count.write(b"1234").unwrap(), 4);
    assert!(count.write(b"5").is_err());
    let mut d = descriptor(0, 0, Action::Terminate);
    d.removal_choice = RemovalChoice::RemoveAfterTermination;
    d.cleanup = Some(CleanupEvidence {
        target: d.captured.target.clone(),
        machine: "fixture".into(),
        root: "/work/example".into(),
        common_dir: "/work/repo/.git".into(),
        owner_id: String::new(),
    });
    let c = context();
    let mut kept = d.clone();
    kept.removal_choice = RemovalChoice::Keep;
    let kept = Record::new(
        c.clone(),
        vec![Group::new(&c, uuid(3), 0, Action::Terminate, vec![kept]).unwrap()],
    )
    .unwrap();
    assert!(kept.groups()[0].members[0]
        .descriptor
        .cleanup
        .as_ref()
        .unwrap()
        .owner_id
        .is_empty());
    let g = Group::new(&c, uuid(3), 0, Action::Terminate, vec![d]).unwrap();
    let with = Record::new(c, vec![g]).unwrap();
    assert!(with.capture_bytes() > r.capture_bytes());
    assert_eq!(
        Record::decode(&serde_json::to_vec(&with).unwrap()).unwrap(),
        with
    );
    let mut future = serde_json::to_value(&with).unwrap();
    future["groups"][0]["members"][0]["d"]["e"]["target"]["future"] = json!(true);
    assert!(Record::decode(&serde_json::to_vec(&future).unwrap()).is_err());
    assert!(with.groups()[0].members[0]
        .descriptor
        .cleanup
        .as_ref()
        .unwrap()
        .owner_id
        .is_empty());
    assert_eq!(
        with.groups()[0].members[0].descriptor.restriction,
        CleanupRestriction::KeepForReview
    );
    assert!(Record::decode(&vec![b' '; CAPTURE_BYTES + 1]).is_err());
}
#[test]
fn future_nested_fields_and_missing_optional_slots_refuse() {
    let r = record(1);
    let original = serde_json::to_value(&r).unwrap();
    assert_eq!(Record::decode(&serde_json::to_vec(&r).unwrap()).unwrap(), r);
    for path in [
        "record",
        "context",
        "group",
        "member",
        "descriptor",
        "action",
        "capture_action",
    ] {
        let mut v = original.clone();
        let object = match path {
            "record" => &mut v,
            "context" => &mut v["context"],
            "group" => &mut v["groups"][0],
            "member" => &mut v["groups"][0]["members"][0],
            "descriptor" => &mut v["groups"][0]["members"][0]["d"],
            "action" => &mut v["groups"][0]["action"],
            _ => &mut v["groups"][0]["members"][0]["d"]["c"][1],
        };
        object
            .as_object_mut()
            .unwrap()
            .insert("future".into(), json!(true));
        assert!(
            Record::decode(&serde_json::to_vec(&v).unwrap()).is_err(),
            "{path}"
        );
    }
    let mut v = serde_json::to_value(global()).unwrap();
    v.as_object_mut().unwrap().remove("slot");
    assert!(serde_json::from_value::<Global>(v).is_err());
    let mut v = original;
    v["groups"][0]["members"][0]["d"]
        .as_object_mut()
        .unwrap()
        .remove("e");
    assert!(Record::decode(&serde_json::to_vec(&v).unwrap()).is_err());
}
#[test]
fn original_clock_and_explicit_platform_control_every_plan() {
    let r = record(1);
    for ns in [0, 99, 100 + QUIT_NS, u64::MAX] {
        let mut n = now();
        n.clock.ns = ns;
        assert_eq!(
            reserve(r.clone(), &global(), &n, &mut retained()).unwrap_err(),
            Error::Expired
        );
    }
    let mut n = now();
    n.clock.ns = 100;
    assert!(reserve(r.clone(), &global(), &n, &mut retained()).is_ok());
    n.clock.boot = uuid(9);
    assert_eq!(
        reserve(r.clone(), &global(), &n, &mut retained()).unwrap_err(),
        Error::Expired
    );
    n = now();
    n.platform = Platform::Windows;
    assert_eq!(
        reserve(r.clone(), &global(), &n, &mut retained()).unwrap_err(),
        Error::Unsupported
    );
    let mut c = context();
    c.platform = Platform::Windows;
    let g = Group::new(
        &c,
        uuid(3),
        0,
        Action::Terminate,
        vec![descriptor(0, 0, Action::Terminate)],
    )
    .unwrap();
    let windows = Record::new(c, vec![g]).unwrap();
    assert_eq!(
        reserve(windows, &global(), &now(), &mut retained()).unwrap_err(),
        Error::Unsupported
    );
    let mut c = context();
    c.quit_confirmed_ns = u64::MAX;
    assert_eq!(c.validate(), Err(Error::Invalid));
}
#[test]
fn immutable_deadline_order_and_full_action_parameters_bind_every_member() {
    let c = context();
    let group = |id, ordinal, n| {
        Group::new(
            &c,
            uuid(id),
            ordinal,
            Action::Terminate,
            vec![descriptor(n, 0, Action::Terminate)],
        )
        .unwrap()
    };
    let ordered = Record::new(c.clone(), vec![group(20, 0, 2), group(10, 1, 1)]).unwrap();
    assert_eq!(ordered.groups()[0].id, uuid(20));
    assert!(Record::new(c.clone(), vec![group(20, 1, 2), group(10, 0, 1)]).is_err());
    let mut d = descriptor(
        0,
        0,
        Action::ClearContext {
            dismiss_prompt: false,
        },
    );
    assert_eq!(
        Group::new(
            &c,
            uuid(3),
            0,
            Action::ClearContext {
                dismiss_prompt: true
            },
            vec![d.clone()]
        )
        .unwrap_err(),
        Error::Invalid
    );
    d.captured.action = Action::ClearContext {
        dismiss_prompt: true,
    };
    let g = Group::new(
        &c,
        uuid(3),
        0,
        Action::ClearContext {
            dismiss_prompt: true,
        },
        vec![d],
    )
    .unwrap();
    assert!(Record::new(c, vec![g]).is_ok());
}
#[test]
fn duplicate_request_or_exact_target_rejects_the_whole_record() {
    let c = context();
    let d = descriptor(0, 0, Action::Terminate);
    let mut other = descriptor(1, 1, Action::Terminate);
    other.request_id = d.request_id.clone();
    let g = Group::new(&c, uuid(3), 0, Action::Terminate, vec![d.clone(), other]).unwrap();
    assert_eq!(
        Record::new(c.clone(), vec![g]).unwrap_err(),
        Error::Conflict
    );
    let mut other = d.clone();
    other.ordinal = 1;
    other.request_id = uuid(999);
    let g = Group::new(&c, uuid(3), 0, Action::Terminate, vec![d, other]).unwrap();
    assert_eq!(Record::new(c, vec![g]).unwrap_err(), Error::Conflict);
}
#[test]
fn retained_store_conflicts_and_unknown_lookup_fail_closed() {
    let r = record(1);
    let mut e = retained();
    e.conflict = Some(r.groups()[0].members[0].descriptor.request_id.clone());
    assert_eq!(
        reserve(r.clone(), &global(), &now(), &mut e).unwrap_err(),
        Error::Conflict
    );
    e = retained();
    e.target_conflict = true;
    assert_eq!(
        reserve(r.clone(), &global(), &now(), &mut e).unwrap_err(),
        Error::Conflict
    );
    e = retained();
    e.fail = true;
    assert_eq!(
        reserve(r, &global(), &now(), &mut e).unwrap_err(),
        Error::Unavailable
    );
}
#[test]
fn all_global_caps_and_arithmetic_refuse_without_release_or_refill() {
    for usage in [
        Usage {
            rows: RETAINED_ROWS,
            ..Usage::default()
        },
        Usage {
            bytes: CHARGED_BYTES,
            ..Usage::default()
        },
        Usage {
            summaries: RETAINED_SUMMARIES,
            ..Usage::default()
        },
    ] {
        let mut g = global();
        g.usage = usage;
        assert_eq!(
            reserve(record(1), &g, &now(), &mut retained()).unwrap_err(),
            Error::Capacity
        );
    }
    let mut g = global();
    g.revision = MAX_REVISION;
    assert_eq!(
        reserve(record(1), &g, &now(), &mut retained()).unwrap_err(),
        Error::Capacity
    );
    g = global();
    g.usage.rows = u64::MAX;
    assert_eq!(
        reserve(record(1), &g, &now(), &mut retained()).unwrap_err(),
        Error::Invalid
    );
}
#[test]
fn uncertain_or_closed_old_generation_blocks_new_configuration() {
    let plan = reserved(1);
    let mut g = plan.after;
    g.slot.as_mut().unwrap().phase = SlotPhase::AdmissionClosed;
    let mut c = context();
    c.quit_id = uuid(999);
    c.configuration_sha256 = "b".repeat(64);
    let group = Group::new(
        &c,
        uuid(888),
        0,
        Action::Terminate,
        vec![descriptor(88, 0, Action::Terminate)],
    )
    .unwrap();
    assert_eq!(
        reserve(
            Record::new(c, vec![group]).unwrap(),
            &g,
            &now(),
            &mut retained()
        )
        .unwrap_err(),
        Error::Busy
    );
}
#[test]
fn issue_consumes_pending_and_preserves_all_expected_before_images() {
    let plan = reserved(2);
    let g = worker(&plan);
    let m = pending(&plan.record);
    let p = &plan.progress[0];
    let issue = issue(&plan.record, &g, &m, p, &now()).unwrap();
    assert_eq!(issue.before, g);
    assert_eq!(issue.before_member, m);
    assert_eq!(issue.before_progress, *p);
    assert_eq!(issue.after_member.phase, MemberPhase::NativeSpawnAttempted);
    assert_eq!(issue.after_progress.issued, 1);
    assert_eq!(issue.after_progress.not_issued, 1);
    assert_eq!(issue.after.usage.exchanges, 1);
    assert_eq!(issue.after.usage.bytes, g.usage.bytes);
    assert_eq!(issue.after.revision, g.revision + 1);
    for phase in [
        MemberPhase::NativeSpawnAttempted,
        MemberPhase::Completed,
        MemberPhase::Failed,
        MemberPhase::Unknown,
    ] {
        let mut spent = m.clone();
        spent.phase = phase;
        assert_eq!(
            super::issue(&plan.record, &g, &spent, p, &now()).unwrap_err(),
            Error::Spent
        );
    }
}
#[test]
fn swapped_descriptor_group_progress_or_prior_slot_phase_refuse() {
    let p = reserved(2);
    let g = worker(&p);
    let m = pending(&p.record);
    let mut changed = m.clone();
    changed.binding.descriptor_sha256 = "f".repeat(64);
    assert_eq!(
        issue(&p.record, &g, &changed, &p.progress[0], &now()).unwrap_err(),
        Error::Invalid
    );
    let mut changed = m.clone();
    changed.binding.request_id = uuid(555);
    assert_eq!(
        issue(&p.record, &g, &changed, &p.progress[0], &now()).unwrap_err(),
        Error::Invalid
    );
    let mut changed = m.clone();
    changed.binding.group.id = uuid(555);
    assert_eq!(
        issue(&p.record, &g, &changed, &p.progress[0], &now()).unwrap_err(),
        Error::Invalid
    );
    let mut progress = p.progress[0].clone();
    progress.binding.id = uuid(555);
    assert_eq!(
        issue(&p.record, &g, &m, &progress, &now()).unwrap_err(),
        Error::Invalid
    );
    for phase in [
        SlotPhase::ReservedSummary,
        SlotPhase::CustodyPreparing,
        SlotPhase::CustodyBound,
        SlotPhase::WorkerSpawnAttempted,
        SlotPhase::AdmissionClosed,
    ] {
        let mut old = g.clone();
        old.slot.as_mut().unwrap().phase = phase;
        assert_eq!(
            issue(&p.record, &old, &m, &p.progress[0], &now()).unwrap_err(),
            Error::Spent
        );
    }
}
#[test]
fn max_four_exchanges_and_shared_original_cutoff_remain_charged() {
    let p = reserved(1);
    let mut g = worker(&p);
    let m = pending(&p.record);
    g.usage.exchanges = 4;
    assert_eq!(
        issue(&p.record, &g, &m, &p.progress[0], &now()).unwrap_err(),
        Error::Capacity
    );
    g.usage.exchanges = 0;
    let mut late = now();
    late.clock.ns = p.record.context().cutoff_ns;
    assert_eq!(
        issue(&p.record, &g, &m, &p.progress[0], &late).unwrap_err(),
        Error::Expired
    );
    let closed = close(&p.record, &g, p.progress.clone()).unwrap();
    assert_eq!(closed.after.usage, g.usage);
    assert_eq!(closed.after.slot.unwrap().phase, SlotPhase::AdmissionClosed);
}
#[test]
fn cleanup_choice_and_exact_target_evidence_cannot_be_swapped() {
    let c = context();
    let mut d = descriptor(0, 0, Action::Terminate);
    d.removal_choice = RemovalChoice::RemoveAfterTermination;
    assert_eq!(
        Group::new(&c, uuid(3), 0, Action::Terminate, vec![d.clone()]).unwrap_err(),
        Error::Invalid
    );
    d.cleanup = Some(CleanupEvidence {
        target: descriptor(1, 0, Action::Terminate).captured.target,
        machine: String::new(),
        root: "/work/example".into(),
        common_dir: "/work/repo/.git".into(),
        owner_id: String::new(),
    });
    assert_eq!(
        Group::new(&c, uuid(3), 0, Action::Terminate, vec![d]).unwrap_err(),
        Error::Invalid
    );
}
#[test]
fn future_phases_and_negative_restriction_do_not_default() {
    assert!(serde_json::from_value::<MemberPhase>(json!("future")).is_err());
    assert!(serde_json::from_value::<SlotPhase>(json!("finished_and_free")).is_err());
    assert!(serde_json::from_value::<CleanupRestriction>(json!("remove_allowed")).is_err());
}

#[test]
fn duplicate_native_action_fields_are_not_erased_by_json_projection() {
    let wire = serde_json::to_string(&record(1)).unwrap();
    let duplicate = wire.replace(
        "\"kind\":\"terminate\"",
        "\"kind\":\"future\",\"kind\":\"terminate\"",
    );
    assert!(Record::decode(duplicate.as_bytes()).is_err());
}

#[test]
fn construction_bounds_context_and_group_action_before_hashing_members() {
    let mut c = context();
    c.gui.birth = "x".repeat(1 << 20);
    assert_eq!(
        Group::new(
            &c,
            uuid(3),
            0,
            Action::Terminate,
            vec![descriptor(0, 0, Action::Terminate)]
        )
        .unwrap_err(),
        Error::Invalid
    );
    let invalid = Action::ForgetArchive {
        archive_id: "x".repeat(1 << 20),
    };
    assert_eq!(
        Group::new(
            &context(),
            uuid(3),
            0,
            invalid.clone(),
            vec![descriptor(0, 0, invalid)]
        )
        .unwrap_err(),
        Error::Invalid
    );
}
