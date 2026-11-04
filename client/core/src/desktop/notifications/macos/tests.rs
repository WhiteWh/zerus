use super::*;
use crate::attention;
use serde_json::json;

fn id(n: usize) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}
fn command(seq: i64) -> Command {
    Command {
        binding: super::super::Binding {
            session: Session {
                configuration: "a".repeat(64),
                service_epoch: id(1),
                writer_epoch: id(2),
                adapter: id(3),
                host_epoch: id(4),
                lease: id(5),
                platform: Platform::Macos,
            },
            request_id: id(100 + seq as usize),
            intent_id: "b".repeat(64),
            revision: 1,
            seq,
        },
        action: Action::Post {
            token: attention::encode_token(
                json!({"host":"peer.example","name":"fixture",
                "run":"fixture-run","notice":id(10)})
                .as_object()
                .unwrap(),
            ),
            title: "Needs attention".into(),
            body: "Fixture".into(),
        },
    }
}
#[derive(Default)]
struct Fake {
    auth: Mutex<VecDeque<Callback>>,
    posts: Mutex<VecDeque<(Envelope, Callback)>>,
    removes: Mutex<Vec<String>>,
}
impl Center for Fake {
    fn authorize(&self, reply: Callback) {
        self.auth.lock().unwrap().push_back(reply);
    }
    fn post(&self, envelope: &Envelope, _: &str, _: &str, reply: Callback) {
        self.posts
            .lock()
            .unwrap()
            .push_back((envelope.clone(), reply));
    }
    fn withdraw(&self, token: &str) {
        self.removes.lock().unwrap().push(token.into());
    }
}
impl Fake {
    fn auth(&self) -> Callback {
        self.auth.lock().unwrap().pop_front().unwrap()
    }
    fn posted(&self) -> (Envelope, Callback) {
        self.posts.lock().unwrap().pop_front().unwrap()
    }
}
fn fixture() -> (Host, Fake, Instant) {
    let mut host = Host::new(command(1).binding.session).unwrap();
    assert!(matches!(
        host.next_output().unwrap().frame,
        FromHost::Ready { .. }
    ));
    (host, Fake::default(), Instant::now())
}
fn stop(host: &mut Host, center: &Fake, now: Instant) {
    host.receive(
        ToHost::Stop {
            session: command(1).binding.session,
        },
        center,
        now,
    )
    .unwrap();
}
fn receipt(host: &mut Host) -> Emission {
    let output = host.next_output().unwrap();
    assert!(matches!(output.frame, FromHost::Receipt(_)));
    output
}

#[test]
fn ready_echoes_exact_context_but_wrong_context_never_reaches_sdk() {
    let session = command(1).binding.session;
    let mut host = Host::new(session.clone()).unwrap();
    assert_eq!(
        host.next_output().unwrap().frame,
        FromHost::Ready {
            abi: ABI.into(),
            application: APPLICATION.into(),
            session: session.clone(),
        }
    );
    let fake = Fake::default();
    let mut wrong = command(1);
    wrong.binding.session.lease = id(77);
    assert!(host
        .receive(ToHost::Command(wrong), &fake, Instant::now())
        .is_err());
    assert!(host
        .receive(
            ToHost::Hello {
                abi: ABI.into(),
                session: session.clone()
            },
            &fake,
            Instant::now()
        )
        .is_err());
    let mut wrong = session;
    wrong.platform = Platform::Linux;
    assert!(Host::new(wrong).is_none());
    assert!(fake.auth.lock().unwrap().is_empty());
}

#[test]
fn original_envelope_and_all_callback_output_and_delegate_holds_survive_completion() {
    let (mut host, fake, now) = fixture();
    let native = host.native_resources();
    let original = command(1);
    host.receive(ToHost::Command(original.clone()), &fake, now)
        .unwrap();
    let auth = fake.auth();
    let auth_copy = auth.clone();
    auth.complete(true);
    host.poll(&fake, now);
    let (envelope, post) = fake.posted();
    assert_eq!(envelope.correlation(), original.binding.request_id);
    assert_eq!(
        envelope.request_id(),
        attention::request_id(original.action.token())
    );
    auth.complete(false);
    post.complete(true);
    post.complete(false);
    host.poll(&fake, now);
    let output = receipt(&mut host);
    assert!(
        matches!(&output.frame, FromHost::Receipt(value) if value.matches(&original)
        && matches!(value.outcome, Outcome::Delivered { .. }))
    );
    stop(&mut host, &fake, now);
    drop(auth);
    drop(post);
    host.poll(&fake, now);
    assert!(!host.native_close_requested());
    drop(auth_copy);
    host.poll(&fake, now);
    assert!(
        !host.native_close_requested(),
        "pending encoded output owns the same command"
    );
    drop(output);
    host.poll(&fake, now);
    assert!(
        !host.native_close_requested(),
        "release output still owns transport work"
    );
    assert!(matches!(host.next_output().unwrap().frame,
        FromHost::Released { binding } if binding == original.binding));
    host.poll(&fake, now);
    assert!(host.native_close_requested());
    assert!(
        host.next_output().is_none(),
        "retained native delegate is still live"
    );
    drop(native);
    host.poll(&fake, now);
    assert!(matches!(
        host.next_output().unwrap().frame,
        FromHost::Stopped { .. }
    ));
    host.poll(&fake, now);
    assert!(host.next_output().is_none());
    assert!(fake.posts.lock().unwrap().is_empty());
}

#[test]
fn permission_denial_and_stop_before_post_never_issue_a_post() {
    for deny in [true, false] {
        let (mut host, fake, now) = fixture();
        host.receive(ToHost::Command(command(1)), &fake, now)
            .unwrap();
        let auth = fake.auth();
        if !deny {
            stop(&mut host, &fake, now);
        }
        auth.complete(!deny);
        host.poll(&fake, now);
        let output = receipt(&mut host);
        assert!(
            matches!(&output.frame, FromHost::Receipt(value) if matches!(value.outcome, Outcome::Refused { .. }))
        );
        assert!(fake.posts.lock().unwrap().is_empty());
    }
}

#[test]
fn a_running_host_releases_one_command_only_after_native_and_receipt_output_drain() {
    let (mut host, fake, now) = fixture();
    let native = host.native_resources();
    let original = command(1);
    host.receive(ToHost::Command(original.clone()), &fake, now)
        .unwrap();
    let auth = fake.auth();
    auth.complete(true);
    drop(auth);
    host.poll(&fake, now);
    let (_, post) = fake.posted();
    post.complete(true);
    host.poll(&fake, now);
    let output = receipt(&mut host);
    host.poll(&fake, now);
    assert!(host.next_output().is_none());
    drop(post);
    host.poll(&fake, now);
    assert!(
        host.next_output().is_none(),
        "the original output still holds the command"
    );
    drop(output);
    host.poll(&fake, now);
    assert!(matches!(host.next_output().unwrap().frame,
        FromHost::Released { binding } if binding == original.binding));
    assert!(!host.stopping);
    assert!(!host.native_close_requested());
    assert_eq!(
        Arc::strong_count(&native.0),
        2,
        "delegate and host stay alive"
    );
    host.receive(ToHost::Command(command(2)), &fake, now)
        .unwrap();
    assert_eq!(fake.auth.lock().unwrap().len(), 1);
}

#[test]
fn deadline_is_unknown_and_late_copies_cannot_rewrite_or_replay_the_attempt() {
    let (mut host, fake, now) = fixture();
    let native = host.native_resources();
    let original = command(1);
    host.receive(ToHost::Command(original.clone()), &fake, now)
        .unwrap();
    let auth = fake.auth();
    auth.complete(true);
    drop(auth);
    host.poll(&fake, now);
    let (_, post) = fake.posted();
    host.poll(&fake, now + WAIT);
    let output = receipt(&mut host);
    assert!(matches!(&output.frame, FromHost::Receipt(value)
        if value.outcome == Outcome::Unknown { code: Uncertainty::Deadline }));
    post.complete(true);
    host.poll(&fake, now + WAIT);
    assert!(host.next_output().is_none());
    assert!(host
        .receive(ToHost::Command(original), &fake, now + WAIT)
        .is_err());
    assert!(
        host.next_output().is_none(),
        "no false before-call refusal for the original effect"
    );
    stop(&mut host, &fake, now);
    drop(output);
    host.poll(&fake, now);
    assert!(!host.native_close_requested());
    drop(post);
    host.poll(&fake, now);
    assert!(!host.native_close_requested());
    assert!(matches!(host.next_output().unwrap().frame,
        FromHost::Released { binding } if binding == command(1).binding));
    host.poll(&fake, now);
    assert!(host.native_close_requested());
    drop(native);
    host.poll(&fake, now);
    assert!(matches!(
        host.next_output().unwrap().frame,
        FromHost::Stopped { .. }
    ));
}

#[test]
fn posting_uses_the_original_completion_time_even_when_the_poll_is_late() {
    for (timely, accepted) in [(true, true), (true, false), (false, true), (false, false)] {
        let (mut host, fake, now) = fixture();
        host.receive(ToHost::Command(command(1)), &fake, now)
            .unwrap();
        fake.auth().complete(true);
        host.poll(&fake, now);
        let (_, post) = fake.posted();
        if !timely {
            host.pending[0].until = Instant::now() - Duration::from_millis(1);
        }
        post.complete(accepted);
        host.poll(&fake, now + WAIT + WAIT);
        let output = receipt(&mut host);
        assert!(
            matches!(output.frame, FromHost::Receipt(Receipt { outcome, .. })
            if if timely && accepted { matches!(outcome, Outcome::Delivered { .. }) }
               else { outcome == Outcome::Unknown { code: if timely { Uncertainty::NativeError } else { Uncertainty::Deadline } } })
        );
    }
}

#[test]
fn eight_undrained_calls_bound_admission_even_after_receipts_arrive() {
    let (mut host, fake, now) = fixture();
    for seq in 1..=IN_FLIGHT as i64 {
        host.receive(ToHost::Command(command(seq)), &fake, now)
            .unwrap();
    }
    for _ in 0..IN_FLIGHT {
        let reply = fake.auth();
        reply.complete(false);
        fake.auth.lock().unwrap().push_back(reply);
    }
    host.poll(&fake, now);
    while host.next_output().is_some() {}
    host.receive(ToHost::Command(command(9)), &fake, now)
        .unwrap();
    assert!(matches!(
        receipt(&mut host).frame,
        FromHost::Receipt(Receipt {
            outcome: Outcome::Refused {
                code: Refusal::Capacity
            },
            ..
        })
    ));
    assert_eq!(fake.auth.lock().unwrap().len(), IN_FLIGHT);
    assert!(
        host.receive(ToHost::Command(command(9)), &fake, now)
            .is_err(),
        "even a capacity refusal consumes the frame highwater"
    );
}

#[test]
fn native_capture_copies_complete_once_across_threads() {
    let (mut host, fake, now) = fixture();
    host.receive(ToHost::Command(command(1)), &fake, now)
        .unwrap();
    let reply = fake.auth();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let copy = reply.clone();
            scope.spawn(move || copy.complete(true));
        }
    });
    reply.complete(false);
    host.poll(&fake, now);
    assert_eq!(fake.posts.lock().unwrap().len(), 1);
}

#[test]
fn dropped_uncalled_captures_refuse_before_post_and_stay_unknown_after_post() {
    for issued in [false, true] {
        let (mut host, fake, now) = fixture();
        host.receive(ToHost::Command(command(1)), &fake, now)
            .unwrap();
        let auth = fake.auth();
        if issued {
            auth.complete(true);
            drop(auth);
            host.poll(&fake, now);
            drop(fake.posted());
        } else {
            drop(auth);
        }
        host.poll(&fake, now);
        assert!(
            matches!(receipt(&mut host).frame, FromHost::Receipt(Receipt { outcome, .. })
            if if issued { matches!(outcome, Outcome::Unknown { .. }) }
               else { matches!(outcome, Outcome::Refused { code: Refusal::NotReady }) })
        );
    }
}

#[test]
fn restore_and_restart_click_correlation_never_post_or_rebuild_a_binding() {
    let (mut host, fake, now) = fixture();
    let original = command(1);
    let envelope = Envelope::new(original.action.token(), &original.binding.request_id).unwrap();
    let mapping = Activation {
        platform_context: None,
        request_id: envelope.correlation().into(),
        token: envelope.token().into(),
        native_id: NativeId::Macos {
            request_id: envelope.request_id(),
        },
    };
    host.clicked(&envelope).unwrap();
    assert_eq!(
        host.next_output().unwrap().frame,
        FromHost::Clicked(mapping.clone())
    );
    host.receive(
        ToHost::Restore {
            session: original.binding.session.clone(),
            mapping: mapping.clone(),
        },
        &fake,
        now,
    )
    .unwrap();
    assert!(fake.auth.lock().unwrap().is_empty());
    assert!(fake.posts.lock().unwrap().is_empty());
    assert!(fake.removes.lock().unwrap().is_empty());
    assert!(host.next_output().is_none());
    let withdraw = Command {
        action: Action::Withdraw {
            token: mapping.token,
            native_id: mapping.native_id,
        },
        ..command(2)
    };
    host.receive(ToHost::Command(withdraw), &fake, now).unwrap();
    host.poll(&fake, now);
    assert!(matches!(
        receipt(&mut host).frame,
        FromHost::Receipt(Receipt {
            outcome: Outcome::Unknown { .. },
            ..
        })
    ));
    assert_eq!(
        fake.removes.lock().unwrap().len(),
        1,
        "void native removals never fabricate a delivered receipt"
    );
}

#[test]
fn cache_capacity_reserves_space_for_pending_native_posts_and_outputs_are_bounded() {
    let (mut host, fake, now) = fixture();
    let command = command(1);
    host.receive(ToHost::Command(command.clone()), &fake, now)
        .unwrap();
    for n in 0..MAPPINGS - 1 {
        let mapping = Activation {
            platform_context: None,
            request_id: id(1000 + n),
            token: command.action.token().into(),
            native_id: NativeId::Macos {
                request_id: attention::request_id(command.action.token()),
            },
        };
        host.receive(
            ToHost::Restore {
                session: command.binding.session.clone(),
                mapping,
            },
            &fake,
            now,
        )
        .unwrap();
    }
    let envelope = Envelope::new(command.action.token(), &id(99999)).unwrap();
    let mapping = Activation {
        platform_context: None,
        request_id: envelope.correlation().into(),
        token: envelope.token().into(),
        native_id: NativeId::Macos {
            request_id: envelope.request_id(),
        },
    };
    assert!(host
        .receive(
            ToHost::Restore {
                session: command.binding.session,
                mapping
            },
            &fake,
            now
        )
        .is_err());
    for _ in 0..OUTPUTS {
        host.clicked(&envelope).unwrap();
    }
    assert!(host.clicked(&envelope).is_err());
}
