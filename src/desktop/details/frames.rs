use super::*;
use crate::desktop::feed;
use anyhow::{ensure, Result};
use rusqlite::{params, OptionalExtension};
use zerus_watch_protocol::{Body, Frame, Hello};

pub(super) struct Source {
    pub generation: u64,
    pub online: bool,
    watch: bool,
    caps: BTreeMap<String, usize>,
}
impl Source {
    fn watch(generation: u64, hello: &Hello) -> Self {
        let mut caps = BTreeMap::new();
        if hello.topics.len() <= 64 {
            for (topic, limit) in [
                (topic::ACTIVITY, hello.limits.activity_subscriptions.min(16)),
                (
                    topic::PROCESSES,
                    hello.limits.processes_subscriptions.min(16),
                ),
                (topic::TASKS, hello.limits.tasks_subscriptions.min(16)),
                (topic::ORGANIZATION, 1),
            ] {
                if hello.topics.iter().any(|t| t == topic) {
                    caps.insert(topic.into(), limit);
                }
            }
        }
        Self {
            generation,
            online: true,
            watch: true,
            caps,
        }
    }
    pub fn limit(&self, topic: &str) -> usize {
        self.caps.get(topic).copied().unwrap_or(0)
    }
    pub fn supports(&self, topic: &str) -> bool {
        self.caps.contains_key(topic)
    }
}
fn route(body: &Body) -> Option<(&str, Option<&str>)> {
    match body {
        Body::Snapshot(s) => Some((&s.topic, s.key.as_deref())),
        Body::Delta(d) => Some((&d.topic, None)),
        Body::State(s) => Some((&s.topic, Some(&s.key))),
        Body::Events(e) => Some((&e.topic, Some(&e.key))),
        Body::Event(e) => Some((&e.topic, Some(&e.key))),
        Body::Gap(g) => Some((&g.topic, g.key.as_deref())),
        Body::Error(e) => e.topic.as_deref().map(|t| (t, e.key.as_deref())),
        _ => None,
    }
}
impl Adapter {
    /// The caller holds this original fleet event/ingress permit through SQL
    /// acceptance AND these bounded queue offers. Display feed JSON cannot call it.
    pub fn observe(
        &mut self,
        event: &fleet::FleetEvent,
        controls: &mut Controls<'_>,
        seq: &mut u64,
    ) {
        self.packet(&event.packet, controls, seq);
    }
    pub(super) fn packet(
        &mut self,
        packet: &feed::Packet,
        controls: &mut Controls<'_>,
        seq: &mut u64,
    ) {
        if self.stopping {
            return;
        }
        let Some((computer, generation)) = packet.scope() else {
            return;
        };
        match packet {
            feed::Packet::Watch { frame, .. } => {
                if let Body::Hello(hello) = &frame.body {
                    if !self.sources.contains_key(computer) && self.sources.len() >= 64 {
                        return;
                    }
                    self.sources
                        .insert(computer.into(), Source::watch(generation, hello));
                    self.source_frame(computer, generation, frame, seq);
                    let keys: Vec<_> = self
                        .groups
                        .keys()
                        .filter(|k| k.computer == computer)
                        .cloned()
                        .collect();
                    for key in keys {
                        self.arm(&key, controls, seq);
                    }
                    return;
                }
                if !self
                    .sources
                    .get(computer)
                    .is_some_and(|s| s.online && s.watch && s.generation == generation)
                {
                    return;
                }
                if matches!(&frame.body,Body::Snapshot(s) if s.topic==topic::CATALOG_META) {
                    self.source_frame(computer, generation, frame, seq);
                    return;
                }
                if let Body::Pong(pong) = &frame.body {
                    let fenced = self.groups.iter().find_map(|(key, group)| {
                        if key.computer != computer || group.generation != generation {
                            return None;
                        }
                        match &group.phase {
                            Phase::Fence { token, until }
                                if pong.id.as_ref().and_then(Value::as_str)
                                    == Some(token.as_str())
                                    && Instant::now() < *until =>
                            {
                                Some((key.clone(), *until))
                            }
                            _ => None,
                        }
                    });
                    if let Some((key, until)) = fenced {
                        let group = self.groups.get_mut(&key).expect("fenced group");
                        group.phase = Phase::Subscribe { until };
                        // Keep the ORIGINAL deadline across both phases.
                        // Admission after Pong prevents in-place native writer
                        // coalescing across this fence.
                        self.advance(&key, controls, seq);
                    }
                    return;
                }
                let Some((topic, key)) = route(&frame.body) else {
                    return;
                };
                let keys: Vec<_> = self
                    .groups
                    .iter()
                    .filter(|(k, g)| {
                        k.computer == computer
                            && k.topic == topic
                            && key.is_none_or(|key| k.key.as_deref() == Some(key))
                            && g.generation == generation
                            && matches!(g.phase, Phase::Active)
                    })
                    .map(|(k, _)| k.clone())
                    .collect();
                for key in keys {
                    self.detail_frame(&key, frame, seq);
                }
            }
            feed::Packet::LegacyBegin { .. } | feed::Packet::LegacyComplete { .. } => {
                if !self.sources.contains_key(computer) && self.sources.len() >= 64 {
                    return;
                }
                self.sources.insert(
                    computer.into(),
                    Source {
                        generation,
                        online: matches!(packet, feed::Packet::LegacyComplete { .. }),
                        watch: false,
                        caps: BTreeMap::new(),
                    },
                );
                self.invalidate(computer, "unsupported_topic", seq);
            }
            feed::Packet::Offline { .. } | feed::Packet::Remove { .. } => {
                if let Some(source) = self.sources.get_mut(computer) {
                    if generation != source.generation {
                        return;
                    }
                    source.online = false;
                }
                self.invalidate(computer, "source_offline", seq);
                if matches!(packet, feed::Packet::Remove { .. }) {
                    self.sources.remove(computer);
                }
            }
            _ => (),
        }
    }
    fn invalidate(&mut self, computer: &str, code: &'static str, seq: &mut u64) {
        let keys: Vec<_> = self
            .groups
            .keys()
            .filter(|k| k.computer == computer)
            .cloned()
            .collect();
        for key in keys {
            if matches!(self.groups[&key].phase, Phase::Retiring) {
                self.groups.remove(&key);
                continue;
            }
            self.groups.get_mut(&key).expect("known group").phase = Phase::Idle(code);
            self.status_for(&key, seq);
        }
    }
    /// A rejected original packet invalidates details too; never turn it into
    /// an accepted catalog FleetEvent or fresh delivery observation.
    pub fn reject(&mut self, event: &fleet::FleetEvent, seq: &mut u64) {
        if let Some((computer, generation)) = event.packet.scope() {
            if let Some(source) = self
                .sources
                .get_mut(computer)
                .filter(|s| s.generation == generation)
            {
                source.online = false;
            }
            self.invalidate(computer, "source_gap", seq);
        }
    }
    fn source_frame(&self, computer: &str, generation: u64, frame: &Frame, seq: &mut u64) {
        for (lease, subscriber) in &self.subscribers {
            if let Some(queue) = subscriber.queue.upgrade().filter(|q| !q.closed()) {
                Self::emit(
                    &self.epoch,
                    seq,
                    &queue,
                    "source",
                    json!({"lease":lease,"computer":computer,"generation":generation,"frame":frame}),
                );
            }
        }
    }
    fn detail_frame(&mut self, key: &Key, frame: &Frame, seq: &mut u64) {
        let group = &self.groups[key];
        for (lease, subscriber) in &mut self.subscribers {
            let Some(queue) = subscriber.queue.upgrade().filter(|q| !q.closed()) else {
                continue;
            };
            for (id, demand) in &mut subscriber.demands {
                if &demand.key != key || queue.closed() {
                    continue;
                }
                let Some(next) = demand.seq.checked_add(1) else {
                    queue.close();
                    break;
                };
                demand.seq = next;
                Self::emit(
                    &self.epoch,
                    seq,
                    &queue,
                    "detail",
                    json!({"lease":lease,"id":id,"computer":key.computer,
                    "generation":group.generation,"revision":group.revision,"detail_seq":next,"frame":frame}),
                );
            }
        }
    }
    pub(super) fn source_info(
        &self,
        store: &Store,
        request: &Value,
        computer: &str,
        generation: u64,
        part: Part,
    ) -> Value {
        let read = || -> Result<Value> {
            let source = self
                .sources
                .get(computer)
                .ok_or_else(|| anyhow::anyhow!("missing source"))?;
            ensure!(
                source.generation == generation && generation > 0,
                "stale source"
            );
            let column = match part {
                Part::Hello => "hello",
                Part::Meta => "meta",
            };
            let sql=format!("SELECT {column},complete,active FROM desktop_catalog_sources WHERE computer=?1 AND epoch=?2 AND generation=?3");
            let record: Option<(Option<Value>, bool, bool)> = store
                .db
                .query_row(
                    &sql,
                    params![computer, self.epoch, generation as i64],
                    |row| {
                        let value = row.get_ref(0)?;
                        let json = match value {
                            rusqlite::types::ValueRef::Null => None,
                            rusqlite::types::ValueRef::Text(bytes)
                                if bytes.len() <= zerus_watch_protocol::MAX_FRAME_BYTES =>
                            {
                                Some(
                                    serde_json::from_slice(bytes)
                                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                                )
                            }
                            _ => return Err(rusqlite::Error::InvalidQuery),
                        };
                        Ok((json, row.get(1)?, row.get(2)?))
                    },
                )
                .optional()?;
            let (data, complete, active) =
                record.ok_or_else(|| anyhow::anyhow!("missing source record"))?;
            // Legacy listing metadata is not a native Hello.
            let data = if matches!(part, Part::Hello) && !source.watch {
                None
            } else {
                data
            };
            let value = json!({"request_id":request["request_id"],"accepted":true,"lease":request["lease"],
                "computer":computer,"generation":generation,"part":column,"native":source.watch,
                "online":source.online && active,"complete":source.online && active && complete,"value":data,"durable":false});
            // Include the complete desktop envelope in the original byte cap.
            protocol::encode(&super::super::frame(
                &self.epoch,
                u64::MAX,
                "response",
                value.clone(),
            ))?;
            Ok(value)
        };
        read().unwrap_or_else(|_|json!({"request_id":request["request_id"],"accepted":false,"code":"detail_source_unavailable"}))
    }
}
