//! Passive observation of the existing owner's source. Same-user authentication
//! permits display ingestion only; no upstream, freshness or effect authority.
use super::{protocol, store::Store};
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};
use zerus_desktop_catalog::{self as catalog, Applied, Source};
use zerus_watch_protocol::{self as watch, Body};

mod io;
pub(super) use io::{client, proxy};
pub const ABI: &str = "desktop-source-feed-v1";
pub const LINE_BYTES: usize = watch::MAX_FRAME_BYTES + 2048;
pub const INGRESS_BYTES: usize = zerus_client_core::source::INGRESS_BYTES;
pub const INGRESS_ITEMS: usize = zerus_client_core::source::INGRESS_ITEMS;

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
pub enum Packet {
    Keepalive {},
    Watch {
        computer: String,
        generation: u64,
        frame: Box<watch::Frame>,
    },
    LegacyBegin {
        computer: String,
        generation: u64,
        metadata: Value,
        rows: usize,
        pages: usize,
    },
    LegacyPage {
        computer: String,
        generation: u64,
        index: usize,
        #[serde(deserialize_with = "page_items")]
        items: Vec<watch::CatalogItem>,
    },
    LegacyComplete {
        computer: String,
        generation: u64,
    },
    Offline {
        computer: String,
        generation: u64,
    },
    Remove {
        computer: String,
        generation: u64,
    },
}
fn page_items<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<Vec<watch::CatalogItem>, D::Error> {
    struct Items;
    impl<'de> serde::de::Visitor<'de> for Items {
        type Value = Vec<watch::CatalogItem>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("at most 128 catalog items")
        }
        fn visit_seq<S: serde::de::SeqAccess<'de>>(
            self,
            mut sequence: S,
        ) -> std::result::Result<Self::Value, S::Error> {
            let mut items = Vec::new();
            while let Some(item) = sequence.next_element()? {
                if items.len() == catalog::PAGE_ROWS {
                    return Err(serde::de::Error::custom("legacy feed page too large"));
                }
                items.push(item);
            }
            Ok(items)
        }
    }
    decoder.deserialize_seq(Items)
}
impl Packet {
    pub(super) fn scope(&self) -> Option<(&str, u64)> {
        match self {
            Self::Keepalive {} => None,
            Self::Watch {
                computer,
                generation,
                ..
            }
            | Self::LegacyBegin {
                computer,
                generation,
                ..
            }
            | Self::LegacyPage {
                computer,
                generation,
                ..
            }
            | Self::LegacyComplete {
                computer,
                generation,
            }
            | Self::Offline {
                computer,
                generation,
            }
            | Self::Remove {
                computer,
                generation,
            } => Some((computer, *generation)),
        }
    }
}
pub struct Producer {
    pub incarnation: u64,
    alive: AtomicBool,
}
impl Producer {
    pub fn close(&self) {
        self.alive.store(false, Ordering::Release);
    }
    fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
}
pub struct Opened {
    pub producer: Option<Arc<Producer>>,
    pub reply: Value,
}
impl Drop for Opened {
    fn drop(&mut self) {
        // A disconnected admission waiter cannot leave a producer lease behind.
        if let Some(producer) = &self.producer {
            producer.close();
        }
    }
}
pub struct Data {
    pub producer: Arc<Producer>,
    pub packet: Box<Packet>,
    pub reply: mpsc::SyncSender<Value>,
    pub _permit: Permit,
}
#[derive(Default)]
struct Usage {
    bytes: usize,
    items: usize,
    high_bytes: usize,
}
#[derive(Default)]
pub struct Ingress {
    usage: Mutex<Usage>,
    wake: Condvar,
}
pub struct Permit {
    ingress: Arc<Ingress>,
    bytes: usize,
}
impl Drop for Permit {
    fn drop(&mut self) {
        let mut usage = self.ingress.usage.lock().unwrap();
        usage.bytes -= self.bytes;
        usage.items -= 1;
        self.ingress.wake.notify_all();
    }
}
impl Ingress {
    pub fn reserve(
        self: &Arc<Self>,
        bytes: usize,
        producer: &Producer,
        stopped: &AtomicBool,
    ) -> Option<Permit> {
        let bytes = bytes.checked_add(1024)?;
        if bytes > INGRESS_BYTES {
            return None;
        }
        let mut usage = self.usage.lock().unwrap();
        loop {
            if stopped.load(Ordering::Acquire) || !producer.alive() {
                return None;
            }
            if usage.bytes <= INGRESS_BYTES - bytes && usage.items < INGRESS_ITEMS {
                usage.bytes += bytes;
                usage.items += 1;
                usage.high_bytes = usage.high_bytes.max(usage.bytes);
                return Some(Permit {
                    ingress: self.clone(),
                    bytes,
                });
            }
            usage = self
                .wake
                .wait_timeout(usage, Duration::from_millis(50))
                .unwrap()
                .0;
        }
    }
}
struct Entry {
    source: Source,
    complete: bool,
    valid: bool,
}
pub struct Broker {
    epoch: String,
    next: u64,
    producer: Option<Arc<Producer>>,
    sources: BTreeMap<String, Entry>,
    expected: Option<BTreeSet<String>>,
    pub ingress: Arc<Ingress>,
    error: bool,
    retry_at: Instant,
}
impl Broker {
    pub fn new(epoch: &str) -> Self {
        Self {
            epoch: epoch.into(),
            next: 0,
            producer: None,
            sources: BTreeMap::new(),
            expected: None,
            ingress: Arc::default(),
            error: false,
            retry_at: Instant::now(),
        }
    }
    pub fn open(&mut self, request: Option<&Value>) -> Result<Arc<Producer>> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            abi: String,
            producer: String,
            mode: String,
        }
        let request: Request =
            serde_json::from_value(request.context("missing feed role")?.clone())
                .map_err(|_| anyhow::anyhow!("invalid feed role"))?;
        ensure!(
            request.abi == ABI && request.mode == "display_only",
            "incompatible feed role"
        );
        ensure!(
            uuid::Uuid::parse_str(&request.producer)
                .is_ok_and(|id| id.to_string() == request.producer),
            "invalid producer identity"
        );
        self.claim_internal()
    }
    /// Actor-owned producer, after lifecycle activation. No UI transport role
    /// or same-user feed connection confers this service ownership.
    pub(super) fn claim_internal(&mut self) -> Result<Arc<Producer>> {
        ensure!(
            self.producer.is_none(),
            "feed producer already owned or retiring"
        );
        self.next = self
            .next
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .context("feed identities exhausted")?;
        let producer = Arc::new(Producer {
            incarnation: self.next,
            alive: AtomicBool::new(true),
        });
        self.producer = Some(producer.clone());
        Ok(producer)
    }
    pub fn status(&self) -> Value {
        let usage = self.ingress.usage.lock().unwrap();
        let active = self.producer.as_ref().is_some_and(|p| p.alive());
        json!({"abi":ABI,"display_only":true,"producer_connected":self.producer.as_ref().is_some_and(|p|p.alive()),
            "retiring":self.producer.as_ref().is_some_and(|p|!p.alive()),"error":self.error,
            "ingress_bytes":usage.bytes,"ingress_items":usage.items,"high_ingress_bytes":usage.high_bytes,
            "sources":self.sources.iter().map(|(computer,e)|json!({"computer":computer,"incarnation":e.source.incarnation,"generation":e.source.generation,"complete":e.complete && active,"authority":false})).collect::<Vec<_>>()})
    }
    pub fn coverage(&self, computer: &str) -> bool {
        !self.error
            && self.producer.as_ref().is_some_and(|p| p.alive())
            && self
                .sources
                .get(computer)
                .is_some_and(|entry| entry.complete && entry.valid)
    }
    pub fn available(&self, computer: &str) -> bool {
        !self.error
            && self.producer.as_ref().is_some_and(|p| p.alive())
            && self.sources.get(computer).is_some_and(|entry| entry.valid)
    }
    pub fn expected(&self) -> Option<&BTreeSet<String>> {
        self.expected.as_ref()
    }
    pub fn failed(&mut self, producer: &Producer) {
        producer.close();
        if self
            .producer
            .as_ref()
            .is_some_and(|p| p.incarnation == producer.incarnation)
        {
            self.error = true;
        }
    }

    /// Immediately suppress a failed internal observation, even if the batch
    /// ends before its queued Offline retirement can be applied. This only
    /// invalidates the same producer/generation, never a replacement source.
    pub(super) fn invalidate(&mut self, producer: &Producer, packet: &Packet) {
        let Some((computer, generation)) = packet.scope() else {
            return;
        };
        if let Some(entry) = self.sources.get_mut(computer) {
            if entry.source.incarnation == producer.incarnation
                && entry.source.generation == generation
            {
                entry.valid = false;
                entry.complete = false;
            }
        }
    }
    pub fn apply(
        &mut self,
        store: &mut Store,
        producer: &Producer,
        packet: &Packet,
    ) -> Result<Option<Value>> {
        let result = self.apply_inner(store, producer, packet);
        if result.is_err()
            && self
                .producer
                .as_ref()
                .is_some_and(|p| p.incarnation == producer.incarnation)
        {
            if let Some((computer, generation)) = packet.scope() {
                if let Some(entry) = self.sources.get_mut(computer) {
                    if generation >= entry.source.generation {
                        entry.valid = false;
                        entry.complete = false;
                    }
                }
            }
        }
        result
    }
    fn apply_inner(
        &mut self,
        store: &mut Store,
        producer: &Producer,
        packet: &Packet,
    ) -> Result<Option<Value>> {
        ensure!(
            self.producer
                .as_ref()
                .is_some_and(|p| p.incarnation == producer.incarnation && p.alive()),
            "stale feed producer"
        );
        let Some((computer, generation)) = packet.scope() else {
            return Ok(None);
        };
        ensure!(
            computer.len() <= 255 && generation > 0 && generation <= i64::MAX as u64,
            "invalid feed scope"
        );
        let old = self.sources.get(computer);
        let starting = matches!(packet, Packet::LegacyBegin { .. })
            || matches!(packet, Packet::Watch {frame,..} if matches!(frame.body, Body::Hello(_)));
        ensure!(
            old.is_some() || self.sources.len() < catalog::MAX_SOURCES as usize,
            "feed source capacity reached"
        );
        if let Some(old) = old {
            ensure!(generation >= old.source.generation, "stale feed generation");
            ensure!(
                !starting || generation > old.source.generation,
                "feed generation was already admitted"
            );
            ensure!(
                starting || generation == old.source.generation,
                "feed source begin required"
            );
        } else {
            ensure!(starting, "feed source begin required");
        }
        let source = Source {
            computer: computer.into(),
            service_epoch: self.epoch.clone(),
            incarnation: producer.incarnation,
            generation,
        };
        let expected = if computer.is_empty() {
            match packet {
                Packet::Watch { frame, .. } => match &frame.body {
                    Body::Snapshot(s) if s.topic == watch::topic::CATALOG_META => {
                        s.value.as_ref().map(expected_peers).transpose()?.flatten()
                    }
                    _ => None,
                },
                _ => None,
            }
        } else {
            None
        };
        let outcome = match packet {
            Packet::Watch { frame, .. } if starting => {
                catalog::connect(&mut store.db, &source, frame)?;
                Applied::Incomplete
            }
            Packet::Watch { frame, .. } => catalog::apply(&mut store.db, &source, frame)?,
            Packet::LegacyBegin {
                metadata,
                rows,
                pages,
                ..
            } => {
                catalog::legacy::begin(&mut store.db, &source, metadata, *rows, *pages)?;
                Applied::Staging
            }
            Packet::LegacyPage { index, items, .. } => {
                catalog::legacy::page(&mut store.db, &source, *index, items)?
            }
            Packet::LegacyComplete { .. } => catalog::legacy::complete(&mut store.db, &source)?,
            Packet::Offline { .. } => {
                catalog::disconnect_many(&mut store.db, std::slice::from_ref(&source))?;
                Applied::Incomplete
            }
            Packet::Remove { .. } => {
                catalog::remove_source(&mut store.db, &source)?;
                self.sources.remove(computer);
                return Ok(Some(
                    json!({"computer":computer,"incarnation":producer.incarnation,
                    "generation":generation,"removed":true,"complete":false,"display_only":true,
                    "changes_abi":super::catalog_read::CHANGES_ABI,"reset_required":true}),
                ));
            }
            Packet::Keepalive {} => unreachable!(),
        };
        let complete = match outcome {
            Applied::Visible => true,
            Applied::Staging | Applied::Incomplete => false,
            Applied::Unchanged => old.is_some_and(|e| e.complete),
        };
        self.sources.insert(
            computer.into(),
            Entry {
                source,
                complete,
                valid: true,
            },
        );
        self.error = false;
        if let Some(expected) = expected {
            self.expected = Some(expected);
        }
        if computer.is_empty() && matches!(packet, Packet::LegacyComplete { .. }) {
            let metadata: String = store.db.query_row(
                "SELECT meta FROM desktop_catalog_sources WHERE computer=''",
                [],
                |r| r.get(0),
            )?;
            if let Some(expected) = expected_peers(&serde_json::from_str(&metadata)?)? {
                self.expected = Some(expected);
            }
        }
        let metadata = matches!(packet, Packet::Watch { frame, .. }
            if matches!(&frame.body, Body::Snapshot(s) if s.topic==watch::topic::CATALOG_META));
        if outcome == Applied::Unchanged && !metadata {
            return Ok(None);
        }
        let mark = catalog::changes::watermark(&store.db, computer)?;
        Ok(Some(
            json!({"computer":computer,"incarnation":producer.incarnation,"generation":generation,
            "complete":complete,"display_only":true,"changes_abi":super::catalog_read::CHANGES_ABI,
            "projection":mark.scope.projection,"revision":mark.revision.map(|r|r.to_string()),
            "online":mark.online,"last_known":!complete,"reset_required":!complete}),
        ))
    }
    pub fn retire(&mut self, store: &mut Store) -> Result<bool> {
        if self.producer.as_ref().is_none_or(|p| p.alive()) || Instant::now() < self.retry_at {
            return Ok(false);
        }
        let sources: Vec<_> = self.sources.values().map(|e| e.source.clone()).collect();
        if let Err(error) = catalog::disconnect_many(&mut store.db, &sources) {
            self.error = true;
            self.retry_at = Instant::now() + Duration::from_secs(10);
            return Err(error);
        }
        self.sources.clear();
        self.expected = None;
        self.producer = None;
        self.error = false;
        Ok(true)
    }
    pub fn stop(&mut self, store: &mut Store) -> Result<()> {
        if let Some(producer) = &self.producer {
            producer.close();
        }
        self.retry_at = Instant::now();
        self.retire(store)?;
        Ok(())
    }
}

fn expected_peers(metadata: &Value) -> Result<Option<BTreeSet<String>>> {
    let Some(value) = metadata.get("peers") else {
        return Ok(None);
    };
    let peers = value.as_array().context("invalid expected peers")?;
    let mut expected = BTreeSet::from([String::new()]);
    for alias in peers {
        let alias = alias
            .as_str()
            .filter(|a| zerus_client_core::fleet::valid_alias(a))
            .context("invalid expected peer")?;
        expected.insert(alias.to_owned());
        ensure!(
            expected.len() <= catalog::MAX_SOURCES as usize,
            "expected source capacity reached"
        );
    }
    Ok(Some(expected))
}

#[cfg(test)]
mod tests;
