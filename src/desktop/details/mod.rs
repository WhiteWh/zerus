//! Subscriber-owned detail demand on the service's original raw watch fleet.
//! No per-session worker or data cache, no effect/delivery authority.
mod demand;
mod frames;
#[cfg(test)]
mod tests;
use super::{fanout::Queue, fleet, protocol, store::Store};
use demand::{Key, Spec};
use frames::Source;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    time::{Duration, Instant},
};
use zerus_desktop_migration::canonical_uuid;
use zerus_watch_protocol::{topic, Control, Ping, Subscription};

pub(super) const ABI: &str = "desktop-details-v1";
const DEMANDS: usize = 64;
const GROUPS: usize = protocol::SUBSCRIBERS * DEMANDS;
const WORK: usize = 16;
const FENCE: Duration = Duration::from_secs(15);
type Controls<'a> = dyn FnMut(&str, u64, &[Control]) -> bool + 'a;
struct Subscriber {
    queue: Weak<Queue>,
    demands: BTreeMap<String, Demand>,
}
struct Demand {
    key: Key,
    spec: Subscription,
    seq: u64,
}
struct Group {
    wanted: Subscription,
    revision: u64,
    generation: u64,
    phase: Phase,
}
enum Phase {
    Fence { token: String, until: Instant },
    Subscribe { until: Instant },
    Active,
    Retiring,
    Idle(&'static str),
}
impl Phase {
    fn state(&self) -> (&'static str, &'static str) {
        match self {
            Self::Fence { .. } => ("starting", "awaiting_fence"),
            Self::Subscribe { .. } => ("starting", "control_backpressure"),
            Self::Active => ("ready", "subscribed"),
            Self::Retiring => ("ended", "unfollowed"),
            Self::Idle(code) => (
                if *code == "unsupported_topic" {
                    "unsupported"
                } else {
                    "incomplete"
                },
                code,
            ),
        }
    }
}
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    #[serde(rename = "detail_follow")]
    Follow {
        request_id: String,
        epoch: String,
        lease: String,
        id: String,
        computer: String,
        subscription: Spec,
    },
    #[serde(rename = "detail_unfollow")]
    Unfollow {
        request_id: String,
        epoch: String,
        lease: String,
        id: String,
    },
    #[serde(rename = "detail_source")]
    Source {
        request_id: String,
        epoch: String,
        lease: String,
        computer: String,
        generation: u64,
        part: Part,
    },
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Part {
    Hello,
    Meta,
}
impl Request {
    fn scope(&self) -> (&str, &str, &str) {
        match self {
            Self::Follow {
                request_id,
                epoch,
                lease,
                ..
            }
            | Self::Unfollow {
                request_id,
                epoch,
                lease,
                ..
            }
            | Self::Source {
                request_id,
                epoch,
                lease,
                ..
            } => (request_id, epoch, lease),
        }
    }
}

pub(super) struct Adapter {
    epoch: String,
    subscribers: BTreeMap<String, Subscriber>,
    groups: BTreeMap<Key, Group>,
    sources: BTreeMap<String, Source>,
    revision: u64,
    cursor: usize,
    stopping: bool,
}
impl Adapter {
    pub fn new(epoch: &str) -> Self {
        Self {
            epoch: epoch.into(),
            subscribers: BTreeMap::new(),
            groups: BTreeMap::new(),
            sources: BTreeMap::new(),
            revision: 0,
            cursor: 0,
            stopping: false,
        }
    }
    pub fn open(&mut self, queue: &Arc<Queue>) -> Option<Value> {
        if self.stopping || queue.closed() || self.subscribers.len() >= protocol::SUBSCRIBERS {
            return None;
        }
        let lease = uuid::Uuid::new_v4().to_string();
        self.subscribers.insert(
            lease.clone(),
            Subscriber {
                queue: Arc::downgrade(queue),
                demands: BTreeMap::new(),
            },
        );
        Some(json!({"abi":ABI,"epoch":self.epoch,"lease":lease,"limits":Self::limits()}))
    }
    fn limits() -> Value {
        json!({"subscribers":protocol::SUBSCRIBERS,"demands_per_subscriber":DEMANDS,
        "groups":GROUPS,"activity_per_computer":16,"processes_per_computer":16,"projects_per_computer":16,
        "organization_per_computer":1,"queue_bytes":protocol::QUEUE_BYTES,"fence_ms":FENCE.as_millis()})
    }
    pub fn status(&self) -> Value {
        json!({"abi":ABI,"stopping":self.stopping,"subscribers":self.subscribers.len(),
        "demands":self.subscribers.values().map(|s|s.demands.len()).sum::<usize>(),"groups":self.groups.len(),"limits":Self::limits()})
    }
    fn emit(epoch: &str, seq: &mut u64, queue: &Queue, kind: &str, value: Value) {
        if queue.closed() {
            return;
        }
        let Some(next) = seq.checked_add(1) else {
            queue.close();
            return;
        };
        *seq = next;
        match protocol::encode(&super::frame(epoch, *seq, kind, value)) {
            Ok(bytes) => {
                queue.offer(bytes.into());
            }
            Err(_) => queue.close(),
        }
    }
    fn status_for(&mut self, key: &Key, seq: &mut u64) {
        let Some(group) = self.groups.get(key) else {
            return;
        };
        let (state, code) = group.phase.state();
        for (lease, subscriber) in &mut self.subscribers {
            let Some(queue) = subscriber.queue.upgrade().filter(|q| !q.closed()) else {
                continue;
            };
            for (id, demand) in &mut subscriber.demands {
                if &demand.key != key {
                    continue;
                }
                Self::emit(
                    &self.epoch,
                    seq,
                    &queue,
                    "detail_status",
                    json!({"lease":lease,"id":id,"computer":key.computer,
                    "generation":group.generation,"revision":group.revision,"detail_seq":demand.seq,
                    "state":state,"code":code,"complete":false}),
                );
            }
        }
    }
    fn merged(&self, key: &Key) -> Option<Subscription> {
        let mut all = self
            .subscribers
            .values()
            .flat_map(|s| s.demands.values())
            .filter(|d| &d.key == key);
        let mut result = all.next()?.spec.clone();
        for demand in all {
            if key.topic == topic::ACTIVITY {
                result.after = match (result.after, demand.spec.after) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    _ => None,
                };
                result.tail = result.after.is_none().then_some(
                    result
                        .tail
                        .unwrap_or(100)
                        .max(demand.spec.tail.unwrap_or(100)),
                );
            }
            if key.topic == topic::PROCESSES {
                result.interval_ms = Some(
                    result
                        .interval_ms
                        .unwrap_or(10_000)
                        .min(demand.spec.interval_ms.unwrap_or(10_000)),
                );
            }
        }
        Some(result)
    }
    fn allowed(&self, key: &Key) -> bool {
        let Some(source) = self.sources.get(&key.computer).filter(|s| s.online) else {
            return false;
        };
        let limit = source.limit(&key.topic).min(key.limit());
        limit > 0
            && self
                .groups
                .keys()
                .filter(|k| k.computer == key.computer && k.topic == key.topic && *k < key)
                .count()
                < limit
    }
    fn arm(&mut self, key: &Key, controls: &mut Controls<'_>, seq: &mut u64) {
        let Some(wanted) = self.merged(key) else {
            if let Some(group) = self.groups.get_mut(key) {
                group.phase = Phase::Retiring;
            }
            self.advance(key, controls, seq);
            return;
        };
        let Some(revision) = self.revision.checked_add(1) else {
            self.stop();
            return;
        };
        self.revision = revision;
        let generation = self.sources.get(&key.computer).map_or(0, |s| s.generation);
        let online = self.sources.get(&key.computer).is_some_and(|s| s.online);
        let allowed = self.allowed(key);
        let token = uuid::Uuid::new_v4().to_string();
        let mut phase = Phase::Idle(if !online {
            "source_offline"
        } else if self
            .sources
            .get(&key.computer)
            .is_some_and(|s| s.supports(&key.topic))
        {
            "detail_topic_limit"
        } else {
            "unsupported_topic"
        });
        if allowed {
            let old = Subscription {
                topic: key.topic.clone(),
                session: wanted.session.clone(),
                project: wanted.project.clone(),
                ..Subscription::default()
            };
            if controls(
                &key.computer,
                generation,
                &[
                    Control::Unsubscribe(old),
                    Control::Ping(Ping {
                        id: Some(json!(token)),
                    }),
                ],
            ) {
                phase = Phase::Fence {
                    token,
                    until: Instant::now() + FENCE,
                };
            } else {
                phase = Phase::Idle("control_backpressure");
            }
        }
        self.groups.insert(
            key.clone(),
            Group {
                wanted,
                revision,
                generation,
                phase,
            },
        );
        for subscriber in self.subscribers.values_mut() {
            for demand in subscriber.demands.values_mut() {
                if &demand.key == key {
                    demand.seq = 0;
                }
            }
        }
        self.status_for(key, seq);
    }
    fn leave(&mut self, key: &Key, controls: &mut Controls<'_>, seq: &mut u64) {
        let wanted = self.merged(key);
        if self
            .groups
            .get(key)
            .is_some_and(|g| wanted.as_ref() == Some(&g.wanted))
        {
            // Remaining consumers already hold this view; no new subscriber needs a snapshot.
            return;
        }
        self.arm(key, controls, seq);
    }
    fn advance(&mut self, key: &Key, controls: &mut Controls<'_>, seq: &mut u64) {
        let Some(group) = self.groups.get_mut(key) else {
            return;
        };
        if matches!(group.phase, Phase::Retiring) {
            let online = self
                .sources
                .get(&key.computer)
                .is_some_and(|s| s.online && s.generation == group.generation);
            let old = Subscription {
                topic: key.topic.clone(),
                session: group.wanted.session.clone(),
                project: group.wanted.project.clone(),
                ..Subscription::default()
            };
            if !online
                || controls(
                    &key.computer,
                    group.generation,
                    &[Control::Unsubscribe(old)],
                )
            {
                self.groups.remove(key);
            }
            return;
        }
        let now = Instant::now();
        if matches!(group.phase,Phase::Fence{until,..}|Phase::Subscribe{until} if now>=until) {
            group.phase = Phase::Idle("fence_timeout");
            self.status_for(key, seq);
            return;
        }
        if matches!(group.phase, Phase::Subscribe { .. })
            && controls(
                &key.computer,
                group.generation,
                &[Control::Subscribe(group.wanted.clone())],
            )
        {
            group.phase = Phase::Active;
            self.status_for(key, seq);
        }
    }
    pub fn tick(&mut self, controls: &mut Controls<'_>, seq: &mut u64) {
        let dead: Vec<_> = self
            .subscribers
            .iter()
            .filter(|(_, s)| s.queue.upgrade().is_none_or(|q| q.closed()))
            .map(|(id, _)| id.clone())
            .collect();
        for lease in dead {
            if let Some(subscriber) = self.subscribers.remove(&lease) {
                let keys: std::collections::BTreeSet<_> =
                    subscriber.demands.into_values().map(|d| d.key).collect();
                for key in keys {
                    self.leave(&key, controls, seq);
                }
            }
        }
        let keys: Vec<_> = self.groups.keys().cloned().collect();
        if keys.is_empty() {
            self.cursor = 0;
            return;
        }
        for offset in 0..WORK.min(keys.len()) {
            self.advance(&keys[(self.cursor + offset) % keys.len()], controls, seq);
        }
        self.cursor = (self.cursor + WORK) % keys.len();
    }
    pub fn stop(&mut self) {
        self.stopping = true;
        for subscriber in self.subscribers.values() {
            if let Some(queue) = subscriber.queue.upgrade() {
                queue.close();
            }
        }
        self.subscribers.clear();
        self.groups.clear();
        self.sources.clear();
    }
    pub fn request(
        &mut self,
        store: &Store,
        value: &Value,
        controls: &mut Controls<'_>,
        seq: &mut u64,
    ) -> Value {
        let id = value["request_id"].as_str().unwrap_or("");
        let refused = |code| json!({"request_id":id,"accepted":false,"code":code});
        let Ok(request) = serde_json::from_value::<Request>(value.clone()) else {
            return refused("invalid_detail_request");
        };
        let (request_id, epoch, lease) = request.scope();
        if !canonical_uuid(request_id) || !canonical_uuid(lease) || epoch != self.epoch {
            return refused("stale_detail_scope");
        }
        if self.stopping {
            return refused("details_stopping");
        }
        let Some(subscriber) = self
            .subscribers
            .get(lease)
            .filter(|s| s.queue.upgrade().is_some_and(|q| !q.closed()))
        else {
            return refused("stale_detail_lease");
        };
        let lease = lease.to_owned();
        match request {
            Request::Follow {
                id,
                computer,
                subscription,
                ..
            } => {
                if !canonical_uuid(&id)
                    || (!computer.is_empty() && !zerus_client_core::fleet::valid_alias(&computer))
                {
                    return refused("invalid_detail_target");
                }
                let Ok((native, refresh)) = subscription.native() else {
                    return refused("invalid_detail_subscription");
                };
                let key = Key::new(&computer, &native);
                if let Some(old) = subscriber.demands.get(&id) {
                    if old.key != key || old.spec != native {
                        return refused("detail_demand_changed");
                    }
                    if refresh {
                        if let Some(group) = self
                            .groups
                            .get(&key)
                            .filter(|g| matches!(g.phase, Phase::Active))
                        {
                            let mut update = group.wanted.clone();
                            update.refresh = Some(true);
                            // This is scheduling on the SAME processes target,
                            // not a new view: preserve its native minimum gap.
                            if !controls(
                                &key.computer,
                                group.generation,
                                &[Control::Subscribe(update)],
                            ) {
                                return refused("detail_control_backpressure");
                            }
                        }
                    }
                } else {
                    if subscriber.demands.len() >= DEMANDS {
                        return refused("detail_demand_limit");
                    }
                    let limit = self
                        .sources
                        .get(&computer)
                        .filter(|s| s.online)
                        .map_or(key.limit(), |s| s.limit(&key.topic).min(key.limit()).max(1));
                    if let Some(group) = self.groups.get(&key) {
                        if group.wanted.session != native.session {
                            return refused("conflicting_detail_identity");
                        }
                    } else if self.groups.len() >= GROUPS
                        || self
                            .groups
                            .keys()
                            .filter(|k| k.computer == computer && k.topic == key.topic)
                            .count()
                            >= limit
                    {
                        return refused("detail_topic_limit");
                    }
                    self.subscribers
                        .get_mut(&lease)
                        .expect("checked lease")
                        .demands
                        .insert(
                            id.clone(),
                            Demand {
                                key: key.clone(),
                                spec: native,
                                seq: 0,
                            },
                        );
                    // Reserve the group in the count before evaluating peer limits.
                    self.groups.entry(key.clone()).or_insert_with(|| Group {
                        wanted: Subscription::default(),
                        revision: 0,
                        generation: 0,
                        phase: Phase::Idle("starting"),
                    });
                    self.arm(&key, controls, seq);
                }
                let Some(group) = self.groups.get(&key) else {
                    return refused("details_stopping");
                };
                let (state, code) = group.phase.state();
                json!({"request_id":value["request_id"],"accepted":true,"lease":lease,"id":id,
                    "generation":group.generation,"revision":group.revision,"state":state,"code":code,"durable":false})
            }
            Request::Unfollow { id, .. } => {
                if !canonical_uuid(&id) {
                    return refused("invalid_detail_id");
                }
                if let Some(old) = self
                    .subscribers
                    .get_mut(&lease)
                    .expect("checked lease")
                    .demands
                    .remove(&id)
                {
                    self.leave(&old.key, controls, seq);
                }
                json!({"request_id":value["request_id"],"accepted":true,"lease":lease,"id":id,"state":"unfollowed","durable":false})
            }
            Request::Source {
                computer,
                generation,
                part,
                ..
            } => self.source_info(store, value, &computer, generation, part),
        }
    }
}
