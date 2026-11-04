use anyhow::{ensure, Result};
use serde::Deserialize;
use zerus_watch_protocol::{topic, SessionRef, Subscription};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Key {
    pub computer: String,
    pub topic: String,
    pub key: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Session {
    name: String,
    run_id: Option<String>,
    conversation_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Spec {
    topic: String,
    session: Option<Session>,
    project: Option<String>,
    after: Option<i64>,
    tail: Option<u32>,
    interval_ms: Option<u64>,
    refresh: Option<bool>,
}
fn text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
impl Spec {
    pub fn native(self) -> Result<(Subscription, bool)> {
        let mut native = Subscription {
            topic: self.topic.clone(),
            project: self.project,
            after: self.after,
            tail: self.tail,
            interval_ms: self.interval_ms,
            ..Subscription::default()
        };
        if let Some(session) = self.session {
            ensure!(text(&session.name, 1024), "invalid session");
            ensure!(
                session.run_id.as_deref().is_none_or(|s| text(s, 1024))
                    && session
                        .conversation_id
                        .as_deref()
                        .is_none_or(|s| text(s, 1024)),
                "invalid identity"
            );
            native.session = Some(SessionRef {
                name: session.name,
                run_id: session.run_id,
                conversation_id: session.conversation_id,
            });
        }
        match native.topic.as_str() {
            topic::ACTIVITY => {
                ensure!(
                    native.session.is_some()
                        && native.project.is_none()
                        && native.interval_ms.is_none()
                        && self.refresh.is_none()
                        && native.after.is_none_or(|n| n >= 0)
                        && native.tail.is_none_or(|n| n <= 100),
                    "invalid activity demand"
                );
                if native.after.is_none() {
                    native.tail = Some(native.tail.unwrap_or(100));
                } else {
                    ensure!(native.tail.is_none(), "ambiguous activity demand");
                }
            }
            topic::PROCESSES => {
                ensure!(
                    native.session.is_some()
                        && native.project.is_none()
                        && native.after.is_none()
                        && native.tail.is_none()
                        && native
                            .interval_ms
                            .is_none_or(|n| (1000..=600_000).contains(&n)),
                    "invalid processes demand"
                );
                native.interval_ms = Some(native.interval_ms.unwrap_or(10_000));
            }
            topic::TASKS => ensure!(
                native.session.is_none()
                    && native.project.as_deref().is_some_and(|s| text(s, 128))
                    && native.after.is_none()
                    && native.tail.is_none()
                    && native.interval_ms.is_none()
                    && self.refresh.is_none(),
                "invalid tasks demand"
            ),
            topic::ORGANIZATION => ensure!(
                native.session.is_none()
                    && native.project.is_none()
                    && native.after.is_none()
                    && native.tail.is_none()
                    && native.interval_ms.is_none()
                    && self.refresh.is_none(),
                "invalid organization demand"
            ),
            _ => anyhow::bail!("unsupported detail topic"),
        }
        Ok((native, self.refresh.unwrap_or(false)))
    }
}
impl Key {
    pub fn new(computer: &str, spec: &Subscription) -> Self {
        Self {
            computer: computer.into(),
            topic: spec.topic.clone(),
            key: spec
                .session
                .as_ref()
                .map(SessionRef::key)
                .or_else(|| spec.project.clone()),
        }
    }
    pub fn limit(&self) -> usize {
        match self.topic.as_str() {
            topic::ACTIVITY | topic::PROCESSES | topic::TASKS => 16,
            topic::ORGANIZATION => 1,
            _ => 0,
        }
    }
}
