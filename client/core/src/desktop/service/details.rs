//! Verified, filtered native detail frames. Native seq is deliberately not a
//! continuity counter here: only detail_seq orders this exact route.
use super::valid_computer;
use crate::{ActivityUpdate, CatalogState, Change, ComputerId, OrganizationSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zerus_watch_protocol::{topic, Body, Frame, Subscription};

pub const ABI: &str = "desktop-details-v1";
pub const DEMANDS: usize = 64;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Lease {
    pub abi: String,
    pub epoch: String,
    pub lease: String,
}
impl Lease {
    pub fn parse(value: &Value, epoch: &str) -> Option<Self> {
        let lease: Self = serde_json::from_value(value.clone()).ok()?;
        (lease.abi == ABI
            && lease.epoch == epoch
            && uuid(&lease.lease)
            && uuid(epoch)
            && value["limits"]["demands_per_subscriber"]
                .as_u64()
                .is_some_and(|n| n > 0 && n <= DEMANDS as u64))
        .then_some(lease)
    }
}
fn uuid(s: &str) -> bool {
    crate::creation::canonical_uuid(s)
}
fn text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Key {
    pub computer: String,
    pub topic: String,
    pub key: String,
}
impl Key {
    pub fn new(computer: &str, spec: &Subscription) -> Result<Self, &'static str> {
        if !valid_computer(computer) || spec.refresh.is_some() || spec.path.is_some() {
            return Err("invalid_detail_subscription");
        }
        if let Some(session) = &spec.session {
            if !text(&session.name, 1024)
                || session.run_id.as_deref().is_some_and(|s| !text(s, 1024))
                || session
                    .conversation_id
                    .as_deref()
                    .is_some_and(|s| !text(s, 1024))
            {
                return Err("invalid_detail_identity");
            }
        }
        let valid = match spec.topic.as_str() {
            topic::ACTIVITY => {
                spec.session.is_some()
                    && spec.project.is_none()
                    && spec.interval_ms.is_none()
                    && spec.after.is_none_or(|n| n >= 0)
                    && spec.tail.is_none_or(|n| n <= 100)
                    && !(spec.after.is_some() && spec.tail.is_some())
            }
            topic::PROCESSES => {
                spec.session.is_some()
                    && spec.project.is_none()
                    && spec.after.is_none()
                    && spec.tail.is_none()
                    && spec
                        .interval_ms
                        .is_none_or(|n| (1000..=600_000).contains(&n))
            }
            topic::TASKS => {
                spec.session.is_none()
                    && spec
                        .project
                        .as_deref()
                        .is_some_and(crate::tasks::valid_project)
                    && spec.after.is_none()
                    && spec.tail.is_none()
                    && spec.interval_ms.is_none()
            }
            topic::ORGANIZATION => {
                spec.session.is_none()
                    && spec.project.is_none()
                    && spec.after.is_none()
                    && spec.tail.is_none()
                    && spec.interval_ms.is_none()
            }
            _ => false,
        };
        if !valid {
            return Err("invalid_detail_subscription");
        }
        Ok(Self {
            computer: computer.into(),
            topic: spec.topic.clone(),
            key: spec
                .session
                .as_ref()
                .map(|s| s.key())
                .or_else(|| spec.project.clone())
                .unwrap_or_default(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    pub generation: u64,
    pub revision: u64,
}
pub struct Route {
    pub key: Key,
    pub id: String,
    pub subscription: Subscription,
    version: Option<Version>,
    seq: u64,
    state: String,
    code: String,
    stamp: u64,
    projection: CatalogState,
}
impl Route {
    pub fn new(
        computer: &str,
        id: String,
        subscription: Subscription,
    ) -> Result<Self, &'static str> {
        if !uuid(&id) {
            return Err("invalid_detail_id");
        }
        let key = Key::new(computer, &subscription)?;
        let mut route = Self {
            key,
            id,
            subscription,
            version: None,
            seq: 0,
            state: String::new(),
            code: String::new(),
            stamp: 0,
            projection: CatalogState::new(),
        };
        route.reset();
        Ok(route)
    }
    fn reset(&mut self) {
        self.projection = CatalogState::new();
        match self.key.topic.as_str() {
            topic::ACTIVITY => self.projection.follow(&self.key.key),
            topic::PROCESSES => self.projection.processes_mut().follow(&self.key.key),
            topic::TASKS => self.projection.tasks_mut().follow(&self.key.key),
            topic::ORGANIZATION => self.projection.organization_mut().follow(),
            _ => unreachable!(),
        }
    }
    pub fn version(&self) -> Option<Version> {
        self.version
    }
    pub fn stamp(&self) -> u64 {
        self.stamp
    }
    pub fn code(&self) -> &str {
        &self.code
    }
    /// Invalidate only this view. Queued data loses its admission; this is not
    /// a native end and does not change any other route or computer.
    pub fn unavailable(&mut self, code: &str) -> Result<Change, &'static str> {
        self.stamp = self
            .stamp
            .checked_add(1)
            .ok_or("detail_revision_exhausted")?;
        self.state = "incomplete".into();
        self.code = code.into();
        self.reset();
        Ok(self.connection(false, code))
    }
    pub fn organization_snapshot(&self) -> Option<OrganizationSnapshot> {
        (self.state == "ready")
            .then(|| self.projection.organization().snapshot())
            .flatten()
    }
    fn version_in(value: &Value) -> Result<Version, &'static str> {
        Ok(Version {
            generation: value["generation"]
                .as_u64()
                .ok_or("invalid_detail_generation")?,
            revision: value["revision"]
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or("invalid_detail_revision")?,
        })
    }
    pub fn connection(&self, ready: bool, code: &str) -> Change {
        self.connection_status(ready, false, code)
    }
    pub fn waiting(&self) -> Change {
        self.connection_status(false, true, "awaiting_service")
    }
    fn connection_status(&self, ready: bool, pending: bool, code: &str) -> Change {
        let computer = ComputerId(self.key.computer.clone());
        let code = code.to_owned();
        let message = "The desktop service detail subscription is unavailable. Retry to reconnect."
            .to_owned();
        match self.key.topic.as_str() {
            topic::ACTIVITY => Change::Activity {
                computer,
                key: self.key.key.clone(),
                update: ActivityUpdate::Connection {
                    ready,
                    pending,
                    code,
                },
            },
            topic::PROCESSES => Change::Processes {
                computer,
                key: self.key.key.clone(),
                update: crate::ProcessesUpdate::Failed { code, message },
            },
            topic::TASKS => Change::Tasks {
                computer,
                key: self.key.key.clone(),
                update: crate::TasksUpdate::Failed { code, message },
            },
            topic::ORGANIZATION => Change::Organization {
                computer,
                update: crate::OrganizationUpdate::Failed { code, message },
            },
            _ => unreachable!(),
        }
    }
    /// Route/lease matching happens before dispatch; a foreign route is ignored.
    pub fn apply(&mut self, kind: &str, value: &Value) -> Result<Vec<Change>, &'static str> {
        if value["id"] != self.id || value["computer"] != self.key.computer {
            return Ok(vec![]);
        }
        let version = Self::version_in(value)?;
        if self.version.is_some_and(|old| {
            version.revision < old.revision
                || (version.revision == old.revision && version.generation != old.generation)
        }) {
            return Ok(vec![]);
        }
        let seq = value["detail_seq"]
            .as_u64()
            .ok_or("invalid_detail_sequence")?;
        if kind == "detail_status" {
            let state = value["state"]
                .as_str()
                .filter(|s| {
                    matches!(
                        *s,
                        "starting" | "ready" | "incomplete" | "unsupported" | "ended"
                    )
                })
                .ok_or("invalid_detail_status")?;
            let code = value["code"]
                .as_str()
                .filter(|s| text(s, 128))
                .ok_or("invalid_detail_status")?;
            if value["complete"] != false {
                return Err("invalid_detail_status");
            }
            let replaced = self.version != Some(version);
            if replaced {
                if seq != 0 {
                    return Err("detail_sequence_gap");
                }
                self.version = Some(version);
                self.seq = 0;
                self.reset();
            } else if seq != self.seq {
                return Err("detail_sequence_gap");
            }
            if state == "ready" && version.generation == 0 {
                return Err("invalid_detail_generation");
            }
            if !replaced && state == self.state && code == self.code {
                return Ok(vec![]);
            }
            self.stamp = self
                .stamp
                .checked_add(1)
                .ok_or("detail_revision_exhausted")?;
            self.state = state.into();
            self.code = code.into();
            // A local ended/refusal is transport state, never native completion.
            return Ok(
                if self.key.topic == topic::ACTIVITY || !matches!(state, "ready" | "starting") {
                    vec![self.connection_status(state == "ready", state == "starting", code)]
                } else {
                    vec![]
                },
            );
        }
        if kind != "detail" {
            return Err("invalid_detail_kind");
        }
        if self.version != Some(version) || self.state != "ready" {
            return Err("detail_not_ready");
        }
        if self.seq.checked_add(1) != Some(seq) {
            return Err("detail_sequence_gap");
        }
        let frame = native_frame(&value["frame"])?;
        let (topic, key) = body_route(&frame.body).ok_or("invalid_detail_frame")?;
        if topic != self.key.topic
            || key.is_some_and(|k| k != self.key.key)
            || (key.is_none() && topic != topic::ORGANIZATION)
        {
            return Err("detail_target_changed");
        }
        self.seq = seq;
        Ok(self
            .projection
            .apply(&ComputerId(self.key.computer.clone()), &frame))
    }
}

pub fn native_frame(value: &Value) -> Result<Frame, &'static str> {
    let line = serde_json::to_string(value).map_err(|_| "invalid_detail_frame")?;
    let frame = zerus_watch_protocol::decode(&line).map_err(|_| "invalid_detail_frame")?;
    if frame.v != 1 {
        return Err("invalid_detail_frame");
    }
    Ok(frame)
}
fn body_route(body: &Body) -> Option<(&str, Option<&str>)> {
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
