//! One watch-v1 fleet per service epoch. The actor alone applies its packets.
//! Source permits cover queued and consumer-held frames, independently of UI
//! speed. Workers belong to computers (at most 64), never individual sessions.
use super::{feed, store::Location};
use crate::{config::Config, platform};
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    process::{Child, Command},
    sync::Arc,
    time::Duration,
};
use zerus_client_core::{source, ComputerId, ComputerStatus, Timing};
use zerus_watch_protocol::{topic, Body, Control};

const POLL_ITEMS: usize = 16;
struct Computer {
    handle: source::Handle,
    wire_generation: u64,
    generation: u64,
    accepting: bool,
    status: ComputerStatus,
    seq: Option<u64>,
}

/// Retain this through Broker::apply, including its SQLite transaction. Dropping
/// it releases source ingress. Synthetic retirements hold no payload permits.
pub struct FleetEvent {
    pub packet: feed::Packet,
    _permit: Option<source::IngressPermit>,
}
pub struct Runtime {
    epoch: String,
    hgs: PathBuf,
    timing: Timing,
    sender: source::Sender,
    receiver: Option<source::Receiver>,
    computers: BTreeMap<ComputerId, Computer>,
    retiring: Vec<source::Handle>,
    pending: VecDeque<feed::Packet>,
    desired: BTreeSet<String>,
    next: u64,
    error: Option<&'static str>,
    stopping: bool,
}

fn spawn(command: &mut Command) -> std::io::Result<Child> {
    #[cfg(unix)]
    {
        platform::spawn_in_turn(command)
    }
    #[cfg(not(unix))]
    {
        command.spawn()
    }
}

impl Runtime {
    #[cfg(test)]
    pub(super) fn empty_for_test(epoch: &str) -> Self {
        let (sender, receiver) = source::channel_with_legacy(
            spawn,
            Arc::new(|| Err(std::io::Error::other("empty fixture never spools"))),
        );
        Self {
            epoch: epoch.into(),
            hgs: PathBuf::new(),
            timing: Timing::default(),
            sender,
            receiver: Some(receiver),
            computers: BTreeMap::new(),
            retiring: Vec::new(),
            pending: VecDeque::new(),
            desired: BTreeSet::new(),
            next: 0,
            error: None,
            stopping: false,
        }
    }
    /// Bind service projection identity to this original handle's wire epoch.
    pub fn controls(&self, computer: &str, generation: u64, controls: &[Control]) -> bool {
        self.computers
            .get(&ComputerId::from(computer))
            .is_some_and(|source| {
                !self.stopping
                    && source.accepting
                    && source.generation == generation
                    && source.handle.controls(source.wire_generation, controls)
            })
    }
    /// Called only after lifecycle ownership, configuration lock and epoch.
    /// No standalone/GUI activation route is provided by this module.
    pub fn start(config: &Config, epoch: &str) -> Result<Self> {
        let location = Location::new(config)?;
        let root = location.root;
        let factory: source::SpoolFactory = Arc::new(move || {
            ensure_private(&root)?;
            let file = tempfile::NamedTempFile::new_in(&root)?;
            if !platform::verify_private_file(file.as_file())? {
                return Err(std::io::Error::other("unsafe fleet spool"));
            }
            Ok(Box::new(file))
        });
        Self::start_with(std::env::current_exe()?, epoch, factory, Timing::default())
    }

    fn start_with(
        hgs: PathBuf,
        epoch: &str,
        factory: source::SpoolFactory,
        timing: Timing,
    ) -> Result<Self> {
        ensure!(
            !epoch.is_empty() && epoch.len() <= 128,
            "invalid fleet epoch"
        );
        let (sender, receiver) = source::channel_with_legacy(spawn, factory);
        let local = sender.spawn(&hgs, &ComputerId::local(), timing.clone())?;
        let mut computers = BTreeMap::new();
        computers.insert(
            ComputerId::local(),
            Computer {
                handle: local,
                wire_generation: 0,
                generation: 0,
                accepting: false,
                status: ComputerStatus::Probing,
                seq: None,
            },
        );
        Ok(Self {
            epoch: epoch.into(),
            hgs,
            timing,
            sender,
            receiver: Some(receiver),
            computers,
            retiring: Vec::new(),
            pending: VecDeque::new(),
            desired: BTreeSet::new(),
            next: 0,
            error: None,
            stopping: false,
        })
    }

    /// Bounded and nonblocking. At most one admitted payload is handed to the
    /// actor; discarded stale/status events have a separate fixed work budget.
    pub fn poll(&mut self) -> Option<FleetEvent> {
        self.reap();
        if self.stopping {
            return None;
        }
        if let Some(packet) = self.pending.pop_front() {
            return Some(FleetEvent {
                packet,
                _permit: None,
            });
        }
        self.admit_peers();
        for _ in 0..POLL_ITEMS {
            let item = self.receiver.as_ref()?.recv_timeout(Duration::ZERO).ok()?;
            let (event, permit) = item.into_parts();
            if let Some(packet) = self.convert(event) {
                return Some(FleetEvent {
                    packet,
                    _permit: Some(permit),
                });
            }
        }
        None
    }

    fn convert(&mut self, event: source::Event) -> Option<feed::Packet> {
        let (identity, wire, starting) = match &event {
            source::Event::Frame {
                source,
                generation,
                frame,
                ..
            } => (source, *generation, matches!(frame.body, Body::Hello(_))),
            source::Event::Legacy {
                source,
                generation,
                update,
                ..
            } => (
                source,
                *generation,
                matches!(**update, source::Legacy::Begin { .. }),
            ),
            source::Event::Status {
                source, generation, ..
            } => (source, *generation, false),
        };
        let computer = self.computers.get_mut(&identity.computer)?;
        if computer.handle.identity() != identity {
            return None;
        }
        if wire < computer.wire_generation || (starting && wire == computer.wire_generation) {
            return None;
        }
        let stale = match &event {
            source::Event::Frame { received_at, .. } => {
                received_at.elapsed() >= self.timing.silence
            }
            source::Event::Legacy { observed_at, .. } => {
                observed_at.elapsed() >= self.timing.silence
            }
            _ => false,
        };
        if stale {
            computer.handle.resnapshot(wire);
            let was_accepting = std::mem::replace(&mut computer.accepting, false);
            self.error = Some("fleet_resnapshot_required");
            return was_accepting.then(|| feed::Packet::Offline {
                computer: identity.computer.0.clone(),
                generation: computer.generation,
            });
        }
        if starting {
            if wire <= computer.wire_generation {
                return None;
            }
            let Some(next) = self.next.checked_add(1).filter(|n| *n <= i64::MAX as u64) else {
                self.error = Some("fleet_generation_exhausted");
                computer.handle.request_stop();
                return None;
            };
            self.next = next;
            computer.wire_generation = wire;
            computer.generation = next;
            computer.accepting = true;
            computer.seq = None;
            if matches!(
                self.error,
                Some("fleet_projection_refused" | "fleet_resnapshot_required")
            ) {
                self.error = None;
            }
        } else if wire < computer.wire_generation {
            return None;
        }
        let generation = computer.generation;
        let alias = identity.computer.0.clone();
        if let source::Event::Status { status, .. } = &event {
            computer.status = *status;
            if !matches!(status, ComputerStatus::Live | ComputerStatus::Polled)
                && computer.accepting
            {
                computer.accepting = false;
                return Some(feed::Packet::Offline {
                    computer: alias,
                    generation,
                });
            }
            return None;
        }
        if !computer.accepting || wire != computer.wire_generation {
            return None;
        }
        if let source::Event::Frame { frame, .. } = &event {
            let hole = computer.seq.is_some_and(|seq| frame.seq != seq + 1);
            let invalidated = matches!(&frame.body, Body::Gap(gap) if gap.topic == topic::CATALOG)
                || matches!(&frame.body, Body::Error(error) if error.fatal || error.topic.as_deref() == Some(topic::CATALOG));
            if hole || invalidated {
                computer.accepting = false;
                computer.handle.resnapshot(wire);
                self.error = Some("fleet_resnapshot_required");
                return Some(feed::Packet::Offline {
                    computer: alias,
                    generation,
                });
            }
            computer.seq = Some(frame.seq);
        }
        // Only the selected local stream may change the configured alias set.
        // Its host/machine_id are observations, never aliases or write authority.
        if identity.computer.is_local() {
            let metadata = match &event {
                source::Event::Frame { frame, .. } => match &frame.body {
                    Body::Snapshot(s) if s.topic == topic::CATALOG_META => s.value.as_ref(),
                    _ => None,
                },
                // Legacy metadata is provisional until Complete; peers are
                // adopted from a later watch/meta rather than partial listings.
                _ => None,
            };
            if let Some(peers) = metadata.and_then(|v| v["peers"].as_array()) {
                self.set_peers(peers);
            }
        }
        Some(match event {
            source::Event::Frame { frame, .. } => feed::Packet::Watch {
                computer: alias,
                generation,
                frame,
            },
            source::Event::Legacy { update, .. } => match *update {
                source::Legacy::Begin {
                    metadata,
                    rows,
                    pages,
                    ..
                } => feed::Packet::LegacyBegin {
                    computer: alias,
                    generation,
                    metadata,
                    rows,
                    pages,
                },
                source::Legacy::Page { index, items } => feed::Packet::LegacyPage {
                    computer: alias,
                    generation,
                    index,
                    items,
                },
                source::Legacy::Complete => feed::Packet::LegacyComplete {
                    computer: alias,
                    generation,
                },
            },
            _ => return None,
        })
    }

    fn set_peers(&mut self, peers: &[Value]) {
        let mut desired = BTreeSet::new();
        for value in peers {
            let Some(alias) = value
                .as_str()
                .filter(|alias| zerus_client_core::fleet::valid_alias(alias))
            else {
                self.error = Some("invalid_fleet_peer");
                return;
            };
            if desired.len() == source::SOURCES - 1 && !desired.contains(alias) {
                self.error = Some("fleet_source_capacity");
                return;
            }
            desired.insert(alias.to_owned());
        }
        self.desired = desired;
        let removed: Vec<_> = self
            .computers
            .keys()
            .filter(|id| !id.is_local() && !self.desired.contains(&id.0))
            .cloned()
            .collect();
        for id in removed {
            let old = self.computers.remove(&id).expect("selected computer");
            old.handle.request_stop();
            if old.generation > 0 {
                self.queue_retirement(feed::Packet::Remove {
                    computer: id.0,
                    generation: old.generation,
                });
            }
            self.retiring.push(old.handle);
        }
        self.error = None;
    }

    fn admit_peers(&mut self) {
        for alias in &self.desired {
            let id = ComputerId(alias.clone());
            if self.computers.contains_key(&id) {
                continue;
            }
            // source::Sender additionally charges pending, cancelled workers.
            let Ok(handle) = self.sender.spawn(&self.hgs, &id, self.timing.clone()) else {
                break;
            };
            self.computers.insert(
                id,
                Computer {
                    handle,
                    wire_generation: 0,
                    generation: 0,
                    accepting: false,
                    status: ComputerStatus::Probing,
                    seq: None,
                },
            );
        }
    }

    /// A failed actor apply retires only that exact source. Queued frames of
    /// its old generation cannot restore coverage while resnapshot reconnects.
    pub fn reject(&mut self, event: &FleetEvent) {
        let Some((alias, generation)) = event.packet.scope() else {
            return;
        };
        if matches!(event.packet, feed::Packet::Remove { .. }) {
            self.error = Some("fleet_removal_refused");
            self.queue_retirement(feed::Packet::Offline {
                computer: alias.into(),
                generation,
            });
            return;
        }
        let Some(computer) = self.computers.get_mut(&ComputerId(alias.into())) else {
            return;
        };
        if computer.generation != generation || !computer.accepting {
            return;
        }
        computer.accepting = false;
        computer.handle.resnapshot(computer.wire_generation);
        self.queue_retirement(feed::Packet::Offline {
            computer: alias.into(),
            generation,
        });
        self.error = Some("fleet_projection_refused");
    }

    fn queue_retirement(&mut self, packet: feed::Packet) {
        if let Some(old) = self
            .pending
            .iter_mut()
            .find(|old| old.scope() == packet.scope())
        {
            if matches!(packet, feed::Packet::Remove { .. }) {
                *old = packet;
            }
        } else if self.pending.len() < source::SOURCES {
            self.pending.push_back(packet);
        } else {
            self.error = Some("fleet_retirement_capacity");
        }
    }

    fn reap(&mut self) {
        self.retiring
            .retain_mut(|handle| !handle.join_if_finished());
    }

    /// Nonblocking cancellation plus a join of already finished supervisors.
    /// Call again while draining; false explicitly means shutdown is incomplete.
    pub fn stop(&mut self) -> bool {
        if !self.stopping {
            self.stopping = true;
            self.receiver.take(); // Cancels all original stream endpoints.
            self.pending.clear();
            for (_, computer) in std::mem::take(&mut self.computers) {
                computer.handle.request_stop();
                self.retiring.push(computer.handle);
            }
        }
        self.reap();
        let usage = self.sender.usage();
        self.retiring.is_empty()
            && usage.workers == 0
            && usage.readers == 0
            && usage.connecting == 0
            && usage.items == 0
            && usage.legacy_spools == 0
            && usage.controls == 0
    }

    pub fn status(&self) -> Value {
        let u = self.sender.usage();
        let drained = self.stopping
            && self.retiring.is_empty()
            && u.workers == 0
            && u.readers == 0
            && u.connecting == 0
            && u.items == 0
            && u.legacy_spools == 0;
        let drained = drained && u.controls == 0;
        let ready = !self.stopping
            && self.error.is_none()
            && self.computers.len() == self.desired.len() + 1
            && self.computers.values().all(|c| {
                c.accepting && matches!(c.status, ComputerStatus::Live | ComputerStatus::Polled)
            });
        json!({"epoch":self.epoch,"state":if drained {"drained"} else if self.stopping {"stopping"} else {"running"},
            "drained":drained,"transport_ready":ready,
            "display_only":true,"authority":false,"error":self.error,
            "workers":u.workers,"readers":u.readers,"connecting":u.connecting,
            "ingress_bytes":u.bytes,"ingress_items":u.items,"high_ingress_bytes":u.high_bytes,
            "control_batches":u.controls,"control_bytes":u.control_bytes,
            "retiring":self.retiring.len(),"pending_retirements":self.pending.len(),
            "legacy_spools":u.legacy_spools,"legacy_reserved_bytes":u.legacy_reserved_bytes,
            "computers":self.computers.iter().map(|(id,c)|json!({"computer":id.0,
                "incarnation":c.handle.identity().incarnation,"generation":c.generation,"status":c.status.name(),
                "attempt":c.status.attempt(),"accepting":c.accepting})).collect::<Vec<_>>(),
            "limits":{"sources":source::SOURCES,"connecting":source::CONNECTIONS,
                "ingress_bytes":source::INGRESS_BYTES,"ingress_items":source::INGRESS_ITEMS,
                "control_batches":source::CONTROL_BATCHES,"control_bytes":source::CONTROL_BYTES,
                "pending_retirements":source::SOURCES,
                "poll_items":POLL_ITEMS,"restart_ms":self.timing.restart.map(|d|d.as_millis())}})
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop();
        // Early owner exit must not block the actor or claim that I/O drained.
        // The source's existing guards continue to account any unfinished work.
        for handle in self.retiring.drain(..) {
            handle.detach();
        }
    }
}
fn ensure_private(root: &std::path::Path) -> std::io::Result<()> {
    if !platform::verify_private(root, platform::Private::Directory)? {
        return Err(std::io::Error::other("unsafe fleet spool directory"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
