use super::*;
use crate::desktop::{feed, store::Location};
use zerus_watch_protocol::Frame;
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn hello() -> Value {
    json!({"t":"hello","machine_id":"fixture","host":"observed-host","hgs_version":"1.56.0",
    "stream":id(),"heartbeat_ms":10000,"max_frame_bytes":1048576,
    "topics":["catalog","catalog.meta","activity","processes","tasks","organization"],
    "limits":{"activity_subscriptions":16,"processes_subscriptions":16,"tasks_subscriptions":16,
    "page_bytes":524288,"queue_bytes":4194304,"activity_tail":100,"activity_read":200,"control_line_bytes":65536}})
}
fn activity(after: Option<i64>) -> Value {
    let mut s = json!({"topic":"activity","session":{"name":"claude/example/a","run_id":"run","conversation_id":"conversation"}});
    if let Some(after) = after {
        s["after"] = json!(after);
    } else {
        s["tail"] = json!(100);
    }
    s
}
fn events(n: i64) -> Value {
    json!({"t":"events","topic":"activity","key":"run:run","events":[{"seq":n,"type":"Stop"}],"cursor":n})
}
fn output(queue: &Queue) -> Vec<Value> {
    let mut frames = Vec::new();
    while let Some(frame) = queue.take() {
        frames.push(serde_json::from_slice(&frame).unwrap());
    }
    frames
}
struct Fixture {
    _temp: tempfile::TempDir,
    store: Store,
    adapter: Adapter,
    broker: feed::Broker,
    producer: Arc<feed::Producer>,
    seq: u64,
    native_seq: u64,
    generation: u64,
    commands: Vec<Vec<Control>>,
    accept: bool,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let epoch = id();
        let location = Location {
            root: temp.path().canonicalize().unwrap().join("state/desktop"),
            configuration: "a".repeat(64),
        };
        location.prepare().unwrap();
        let store = Store::open(&location, &epoch).unwrap();
        let mut broker = feed::Broker::new(&epoch);
        let producer = broker.claim_internal().unwrap();
        let mut f = Self {
            _temp: temp,
            store,
            adapter: Adapter::new(&epoch),
            broker,
            producer,
            seq: 0,
            native_seq: 0,
            generation: 1,
            commands: Vec::new(),
            accept: true,
        };
        f.native(hello());
        f
    }
    fn open(&mut self) -> (String, Arc<Queue>) {
        let queue = Arc::new(Queue::default());
        let lease = self.adapter.open(&queue).unwrap()["lease"]
            .as_str()
            .unwrap()
            .to_owned();
        (lease, queue)
    }
    fn request(&mut self, lease: &str, body: Value) -> Value {
        let mut request = body;
        request["request_id"] = json!(id());
        request["epoch"] = json!(self.adapter.epoch);
        request["lease"] = json!(lease);
        let accept = self.accept;
        let commands = &mut self.commands;
        self.adapter.request(
            &self.store,
            &request,
            &mut |computer, generation, lines| {
                assert_eq!(computer, "");
                assert!(generation > 0);
                if accept {
                    commands.push(lines.to_vec());
                }
                accept
            },
            &mut self.seq,
        )
    }
    fn follow(&mut self, lease: &str, id: &str, spec: Value) -> Value {
        self.request(
            lease,
            json!({"op":"detail_follow","id":id,"computer":"","subscription":spec}),
        )
    }
    fn native(&mut self, mut body: Value) -> Frame {
        self.native_seq += 1;
        body["v"] = json!(1);
        body["seq"] = json!(self.native_seq);
        let frame: Frame = serde_json::from_value(body).unwrap();
        let packet = feed::Packet::Watch {
            computer: String::new(),
            generation: self.generation,
            frame: Box::new(frame.clone()),
        };
        self.broker
            .apply(&mut self.store, &self.producer, &packet)
            .unwrap();
        let accept = self.accept;
        let commands = &mut self.commands;
        self.adapter.packet(
            &packet,
            &mut |_, _, lines| {
                if accept {
                    commands.push(lines.to_vec());
                }
                accept
            },
            &mut self.seq,
        );
        frame
    }
    fn token(&self) -> String {
        self.adapter
            .groups
            .values()
            .find_map(|g| match &g.phase {
                Phase::Fence { token, .. } => Some(token.clone()),
                _ => None,
            })
            .unwrap()
    }
    fn pong(&mut self) {
        let token = self.token();
        self.native(json!({"t":"pong","id":token}));
    }
    fn tick(&mut self) {
        let accept = self.accept;
        let commands = &mut self.commands;
        self.adapter.tick(
            &mut |_, _, lines| {
                if accept {
                    commands.push(lines.to_vec());
                }
                accept
            },
            &mut self.seq,
        );
    }
}
#[test]
fn two_phase_fence_admits_only_new_frames_and_preserves_native_identity() {
    let mut f = Fixture::new();
    let (lease, queue) = f.open();
    let demand = id();
    let ack = f.follow(&lease, &demand, activity(None));
    assert_eq!(ack["state"], "starting");
    assert_eq!(f.commands.len(), 1);
    assert!(matches!(
        &f.commands[0][..],
        [Control::Unsubscribe(_), Control::Ping(_)]
    ));
    output(&queue);
    f.native(events(1));
    assert!(output(&queue).is_empty());
    f.native(json!({"t":"pong","id":id()}));
    assert_eq!(f.commands.len(), 1);
    f.pong();
    assert!(matches!(&f.commands[1][..], [Control::Subscribe(_)]));
    let ready = output(&queue);
    assert_eq!(ready.last().unwrap()["value"]["state"], "ready");
    assert_eq!(ready.last().unwrap()["value"]["complete"], false);
    let native = f.native(events(2));
    let frames = output(&queue);
    assert_eq!(frames.len(), 1);
    assert_eq!(
        frames[0]["value"]["frame"],
        serde_json::to_value(native).unwrap()
    );
    assert_eq!(frames[0]["value"]["detail_seq"], 1);
    assert_eq!(frames[0]["value"]["id"], demand);
    f.native(json!({"t":"heartbeat","at":1}));
    f.native(events(3));
    assert_eq!(output(&queue)[0]["value"]["detail_seq"], 2);
}
#[test]
fn changed_demand_rejects_old_pong_and_uses_a_new_route_revision() {
    let mut f = Fixture::new();
    let (lease, queue) = f.open();
    let old = id();
    let first = f.follow(&lease, &old, activity(Some(10)));
    let token = f.token();
    assert_eq!(
        f.follow(&lease, &old, activity(Some(20)))["code"],
        "detail_demand_changed"
    );
    let new = id();
    let second = f.follow(&lease, &new, activity(Some(20)));
    assert!(second["revision"].as_u64().unwrap() > first["revision"].as_u64().unwrap());
    output(&queue);
    f.native(json!({"t":"pong","id":token}));
    assert!(output(&queue).is_empty());
    f.pong();
    let Control::Subscribe(spec) = &f.commands.last().unwrap()[0] else {
        panic!("missing subscription")
    };
    assert_eq!(spec.after, Some(10));
    f.request(&lease, json!({"op":"detail_unfollow","id":old}));
    f.pong();
    let Control::Subscribe(spec) = &f.commands.last().unwrap()[0] else {
        panic!("missing subscription")
    };
    assert_eq!(spec.after, Some(20));
}
#[test]
fn subscribers_share_one_target_and_last_disconnect_unsubscribes() {
    let mut f = Fixture::new();
    let (a, qa) = f.open();
    let (b, qb) = f.open();
    let ia = id();
    let ib = id();
    f.follow(&a, &ia, activity(None));
    f.follow(&b, &ib, activity(None));
    assert_eq!(f.adapter.groups.len(), 1);
    f.pong();
    output(&qa);
    output(&qb);
    f.native(events(1));
    let fa = output(&qa);
    let fb = output(&qb);
    assert_eq!(fa[0]["value"]["id"], ia);
    assert_eq!(fb[0]["value"]["id"], ib);
    let command_count = f.commands.len();
    qa.close();
    f.tick();
    assert_eq!(f.adapter.subscribers.len(), 1);
    assert_eq!(f.adapter.groups.len(), 1);
    assert_eq!(
        f.commands.len(),
        command_count,
        "an unchanged merged view is not rearmed on leave"
    );
    assert!(
        output(&qb).is_empty(),
        "the remaining route keeps its revision and sequence"
    );
    f.native(events(2));
    let next = output(&qb);
    assert_eq!(next[0]["value"]["revision"], fb[0]["value"]["revision"]);
    assert_eq!(next[0]["value"]["detail_seq"], 2);
    qb.close();
    f.tick();
    assert!(f.adapter.groups.is_empty());
    assert!(f.adapter.subscribers.is_empty());
    assert!(matches!(
        &f.commands.last().unwrap()[..],
        [Control::Unsubscribe(_)]
    ));
}
#[test]
fn unfollow_of_an_identical_demand_keeps_the_remaining_view_ready() {
    let mut f = Fixture::new();
    let (lease, queue) = f.open();
    let a = id();
    let b = id();
    f.follow(&lease, &a, activity(None));
    f.follow(&lease, &b, activity(None));
    f.pong();
    output(&queue);
    f.native(events(1));
    let before = output(&queue);
    let count = f.commands.len();
    f.request(&lease, json!({"op":"detail_unfollow","id":a}));
    assert_eq!(f.commands.len(), count);
    assert!(output(&queue).is_empty());
    f.native(events(2));
    let next = output(&queue);
    assert_eq!(next.len(), 1);
    assert_eq!(next[0]["value"]["id"], b);
    assert_eq!(next[0]["value"]["revision"], before[0]["value"]["revision"]);
    assert_eq!(next[0]["value"]["detail_seq"], 2);
}
#[test]
fn a_shrunken_native_limit_is_incomplete_instead_of_unsupported() {
    let mut f = Fixture::new();
    let (lease, queue) = f.open();
    for n in 0..2 {
        f.follow(
            &lease,
            &id(),
            json!({"topic":"tasks","project":format!("p{n}")}),
        );
    }
    output(&queue);
    f.generation += 1;
    f.native_seq = 0;
    let mut smaller = hello();
    smaller["limits"]["tasks_subscriptions"] = json!(1);
    f.native(smaller);
    let statuses = output(&queue);
    let limited: Vec<_> = statuses
        .iter()
        .filter(|s| s["value"]["code"] == "detail_topic_limit")
        .collect();
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0]["value"]["state"], "incomplete");
}
#[test]
fn backpressure_after_pong_never_releases_frames_before_subscribe_admission() {
    let mut f = Fixture::new();
    let (lease, queue) = f.open();
    f.follow(&lease, &id(), activity(None));
    let token = f.token();
    f.accept = false;
    f.native(json!({"t":"pong","id":token}));
    output(&queue);
    f.native(events(1));
    assert!(output(&queue).is_empty());
    assert_eq!(f.commands.len(), 1);
    f.accept = true;
    f.tick();
    assert_eq!(f.commands.len(), 2);
    assert_eq!(output(&queue)[0]["value"]["state"], "ready");
    f.native(events(2));
    assert_eq!(output(&queue)[0]["value"]["detail_seq"], 1);
}
#[test]
fn reconnect_and_stale_generation_do_not_adopt_old_frames() {
    let mut f = Fixture::new();
    let (lease, queue) = f.open();
    let ack = f.follow(&lease, &id(), activity(None));
    let token = f.token();
    f.generation = 2;
    f.native_seq = 0;
    f.native(hello());
    output(&queue);
    f.native(json!({"t":"pong","id":token}));
    f.native(events(1));
    assert!(output(&queue).is_empty());
    f.pong();
    let ready = output(&queue);
    assert_eq!(ready[0]["value"]["generation"], 2);
    assert!(ready[0]["value"]["revision"].as_u64().unwrap() > ack["revision"].as_u64().unwrap());
    let packet=feed::Packet::Watch{computer:String::new(),generation:1,frame:Box::new(serde_json::from_value(json!({"v":1,"seq":99,"t":"events","topic":"activity","key":"run:run","events":[],"cursor":99})).unwrap())};
    f.adapter
        .packet(&packet, &mut |_, _, _| panic!("stale control"), &mut f.seq);
    assert!(output(&queue).is_empty());
    f.native(events(2));
    assert_eq!(output(&queue)[0]["value"]["detail_seq"], 1);
}
#[test]
fn closed_or_overflowed_queue_retires_its_lease_without_affecting_another() {
    let mut f = Fixture::new();
    let (lease, queue) = f.open();
    let (other, q2) = f.open();
    f.follow(&lease, &id(), activity(None));
    f.follow(&other, &id(), activity(None));
    for _ in 0..5 {
        queue.offer(vec![0; 1 << 20].into());
    }
    assert!(queue.closed());
    f.tick();
    assert_eq!(f.adapter.subscribers.len(), 1);
    assert!(!q2.closed());
    assert_eq!(
        f.follow(&lease, &id(), activity(None))["code"],
        "stale_detail_lease"
    );
}
#[test]
fn detail_and_topic_caps_and_explicit_unsupported_remain_bounded() {
    let mut f = Fixture::new();
    let (lease, queue) = f.open();
    for _ in 0..DEMANDS {
        assert_eq!(f.follow(&lease, &id(), activity(None))["accepted"], true);
        output(&queue); // A fast subscriber while testing demand, not queue, capacity.
    }
    assert_eq!(
        f.follow(&lease, &id(), activity(None))["code"],
        "detail_demand_limit"
    );
    assert_eq!(f.adapter.groups.len(), 1);
    assert!(!queue.closed());
    let mut f = Fixture::new();
    let (lease, _q) = f.open();
    for n in 0..16 {
        assert_eq!(
            f.follow(
                &lease,
                &id(),
                json!({"topic":"tasks","project":format!("p{n}")})
            )["accepted"],
            true
        );
    }
    assert_eq!(
        f.follow(&lease, &id(), json!({"topic":"tasks","project":"overflow"}))["code"],
        "detail_topic_limit"
    );
    let mut f = Fixture::new();
    let (lease, _q) = f.open();
    f.generation = 2;
    f.native_seq = 0;
    let mut unsupported = hello();
    unsupported["topics"] = json!(["catalog", "catalog.meta"]);
    f.native(unsupported);
    let ack = f.follow(&lease, &id(), activity(None));
    assert_eq!(ack["state"], "unsupported");
    assert!(f.commands.is_empty());
}
#[test]
fn malformed_identity_epoch_and_unknown_fields_never_admit_controls() {
    let mut f = Fixture::new();
    let (lease, _q) = f.open();
    let mut bad = activity(None);
    bad["path"] = json!("/unexpected");
    assert_eq!(f.follow(&lease, &id(), bad)["accepted"], false);
    let bad = json!({"topic":"activity","session":{"name":"a","extra":true}});
    assert_eq!(f.follow(&lease, &id(), bad)["accepted"], false);
    let bad = json!({"op":"detail_follow","request_id":id(),"lease":lease,"epoch":id(),"id":id(),"computer":"","subscription":activity(None)});
    assert_eq!(
        f.adapter.request(
            &f.store,
            &bad,
            &mut |_, _, _| panic!("stale admission"),
            &mut f.seq
        )["accepted"],
        false
    );
    assert!(f.commands.is_empty());
}
#[test]
fn bootstrap_parts_are_bound_and_preserve_offline_last_known_metadata() {
    let mut f = Fixture::new();
    let (lease, queue) = f.open();
    f.native(json!({"t":"snapshot","topic":"catalog.meta","value":{"peers":[],"machine_id":"fixture","capability":"kept"},"page":0,"last":true,"cursor":1}));
    let frames = output(&queue);
    assert_eq!(frames[0]["t"], "source");
    let hello = f.request(
        &lease,
        json!({"op":"detail_source","computer":"","generation":1,"part":"hello"}),
    );
    assert_eq!(hello["accepted"], true);
    assert_eq!(hello["value"]["machine_id"], "fixture");
    assert_eq!(hello["complete"], false);
    let packet = feed::Packet::Offline {
        computer: String::new(),
        generation: 1,
    };
    f.broker.apply(&mut f.store, &f.producer, &packet).unwrap();
    f.adapter.packet(&packet, &mut |_, _, _| true, &mut f.seq);
    let meta = f.request(
        &lease,
        json!({"op":"detail_source","computer":"","generation":1,"part":"meta"}),
    );
    assert_eq!(meta["accepted"], true);
    assert_eq!(meta["online"], false);
    assert_eq!(meta["complete"], false);
    assert_eq!(meta["value"]["capability"], "kept");
    assert_eq!(
        f.request(
            &lease,
            json!({"op":"detail_source","computer":"","generation":2,"part":"hello"})
        )["accepted"],
        false
    );
}
#[test]
fn stop_closes_lease_and_never_admits_more_upstream_work() {
    let mut f = Fixture::new();
    let (lease, queue) = f.open();
    f.follow(&lease, &id(), activity(None));
    let count = f.commands.len();
    f.adapter.stop();
    assert!(queue.closed());
    assert!(f.adapter.groups.is_empty());
    assert_eq!(
        f.follow(&lease, &id(), activity(None))["code"],
        "details_stopping"
    );
    assert_eq!(f.commands.len(), count);
}

#[test]
fn processes_tasks_and_organization_keep_their_native_pages_and_keys() {
    let mut f = Fixture::new();
    let (lease, queue) = f.open();
    for spec in [
        json!({"topic":"processes","session":{"name":"claude/example/a","run_id":"run","conversation_id":"conversation"}}),
        json!({"topic":"tasks","project":"p"}),
        json!({"topic":"organization"}),
    ] {
        assert_eq!(f.follow(&lease, &id(), spec)["accepted"], true);
    }
    while f
        .adapter
        .groups
        .values()
        .any(|g| matches!(g.phase, Phase::Fence { .. }))
    {
        f.pong();
    }
    output(&queue);
    for body in [
        json!({"t":"state","topic":"processes","key":"run:run","value":{"processes":{"items":[]}},"rev":1}),
        json!({"t":"snapshot","topic":"tasks","key":"p","items":[{"id":"task"}],"page":0,"last":false,"cursor":2}),
        json!({"t":"snapshot","topic":"tasks","key":"p","items":[],"page":1,"last":true,"cursor":2}),
        json!({"t":"snapshot","topic":"organization","items":[{"key":"organization","row":{"digest":"d1"}}],"page":0,"last":true,"cursor":3}),
        json!({"t":"delta","topic":"organization","upsert":[],"remove":[{"key":"project:old"}],"cursor":4}),
    ] {
        let native = f.native(body);
        let frames = output(&queue);
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0]["value"]["frame"],
            serde_json::to_value(native).unwrap()
        );
    }
    assert_eq!(f.adapter.groups.len(), 3);
}
#[test]
fn process_refresh_keeps_revision_and_native_interval_instead_of_replacing_the_view() {
    let mut f = Fixture::new();
    let (lease, queue) = f.open();
    let demand = id();
    let mut spec = json!({"topic":"processes","session":{"name":"claude/example/a","run_id":"run","conversation_id":"conversation"},"interval_ms":1000});
    let ack = f.follow(&lease, &demand, spec.clone());
    f.pong();
    output(&queue);
    spec["refresh"] = json!(true);
    let refresh = f.follow(&lease, &demand, spec);
    assert_eq!(refresh["revision"], ack["revision"]);
    assert_eq!(refresh["state"], "ready");
    let [Control::Subscribe(update)] = f.commands.last().unwrap().as_slice() else {
        panic!("refresh replaced native subscription")
    };
    assert_eq!(update.refresh, Some(true));
    assert_eq!(update.interval_ms, Some(1000));
}
#[test]
fn oversized_metadata_and_fence_timeout_stay_unavailable_without_truncation() {
    let mut f = Fixture::new();
    let (lease, queue) = f.open();
    f.follow(&lease, &id(), activity(None));
    for group in f.adapter.groups.values_mut() {
        if let Phase::Fence { until, .. } = &mut group.phase {
            *until = Instant::now() - Duration::from_secs(1);
        }
    }
    f.tick();
    assert_eq!(
        output(&queue).last().unwrap()["value"]["code"],
        "fence_timeout"
    );
    assert_eq!(f.commands.len(), 1);
    f.store
        .db
        .execute(
            "UPDATE desktop_catalog_sources SET meta=?1 WHERE computer=''",
            [json!("x".repeat(zerus_watch_protocol::MAX_FRAME_BYTES)).to_string()],
        )
        .unwrap();
    let result = f.request(
        &lease,
        json!({"op":"detail_source","computer":"","generation":1,"part":"meta"}),
    );
    assert_eq!(result["accepted"], false);
    assert!(result.get("value").is_none());
}
