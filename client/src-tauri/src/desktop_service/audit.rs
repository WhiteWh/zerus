//! Bounded read-only Settings journal requests; no approval worker or write path.
use super::*;
use serde::{Deserialize, Serialize};
use zerus_client_core::desktop::approval_audit as wire;
use zerus_client_core::{approvals, desktop::approval_audit::Request};

pub(crate) const UNAVAILABLE: &str =
    zerus_client_core::source!("The approval journal is unavailable. Refresh to try again.");
const CHANGED: &str =
    zerus_client_core::source!("The approval journal changed. Refresh to read it again.");
const SAFE_INTEGER: i64 = (1_i64 << 53) - 1;
pub(in crate::desktop_service) mod help;

/// Display read identity, carried by this page rather than a shared GUI cache.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    configuration: String,
    epoch: String,
    generation: u64,
    journal: String,
    through: i64,
    host: String,
    session: String,
}

#[derive(Debug, Serialize)]
pub struct Page {
    #[serde(flatten)]
    pub page: approvals::Page,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
}

struct Read {
    service: Arc<Service>,
    configuration: String,
    epoch: String,
    generation: u64,
    stop: Arc<AtomicBool>,
    routing: Arc<zerus_client_core::desktop::relay::Connection>,
}
impl Read {
    fn new(service: Option<Arc<Service>>) -> Result<Self, &'static str> {
        let service = service.ok_or(UNAVAILABLE)?;
        let snapshot = service.snapshot();
        if !service.shared.allowed || !snapshot.connected {
            return Err(UNAVAILABLE);
        }
        let configuration = service
            .shared
            .configuration
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or(UNAVAILABLE)?;
        let stop = service
            .shared
            .cancellation
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or(UNAVAILABLE)?;
        let routing = service
            .shared
            .relay
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or(UNAVAILABLE)?;
        Ok(Self {
            service,
            configuration,
            epoch: snapshot.epoch.ok_or(UNAVAILABLE)?,
            generation: snapshot.generation.ok_or(UNAVAILABLE)?,
            stop,
            routing,
        })
    }
    fn current(&self) -> bool {
        let shared = &self.service.shared;
        let snapshot = self.service.snapshot();
        !self.stop.load(Ordering::Acquire)
            && !shared.stopped.load(Ordering::Acquire)
            && snapshot.connected
            && snapshot.epoch.as_deref() == Some(&self.epoch)
            && snapshot.generation == Some(self.generation)
            && shared
                .configuration
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_deref()
                == Some(&self.configuration)
            && shared
                .cancellation
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .is_some_and(|stop| Arc::ptr_eq(stop, &self.stop))
            && shared
                .relay
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .is_some_and(|routing| Arc::ptr_eq(routing, &self.routing))
    }
    fn check(&self, scope: &Scope) -> Result<(), &'static str> {
        if !self.current()
            || scope.configuration != self.configuration
            || scope.epoch != self.epoch
            || scope.generation != self.generation
            || !(0..=SAFE_INTEGER).contains(&scope.through)
        {
            return Err(CHANGED);
        }
        Ok(())
    }
    async fn query(&self, request: Request) -> Result<Value, &'static str> {
        if !request.valid() {
            return Err(UNAVAILABLE);
        }
        if !self.current() {
            return Err(CHANGED);
        }
        // One bounded read admission spans metadata, probe and query. Any original
        // child/pipe or metadata job retains its clone after an uncertain return.
        let resources: Arc<dyn Send + Sync> = Arc::new(
            self.service
                .shared
                .rpc
                .admit(Role::Fleet)
                .map_err(|_| UNAVAILABLE)?,
        );
        let image = match help::capture(self, resources.clone()).await {
            Ok(image) => image,
            Err(error) => {
                self.service.shared.audit_help.clear();
                return Err(error);
            }
        };
        if !self.current() {
            return Err(CHANGED);
        }
        let cached = image
            .as_ref()
            .is_some_and(|image| self.service.shared.audit_help.matches(self, image));
        if !cached {
            self.service.shared.audit_help.clear();
            let bytes = self
                .command(
                    vec!["--help".into()],
                    None,
                    resources.clone(),
                    image.clone(),
                )
                .await?;
            if !std::str::from_utf8(&bytes)
                .is_ok_and(|help| help.lines().any(|line| line.trim() == wire::HELP_MARKER))
            {
                return Err(UNAVAILABLE);
            }
            if let Some(image) = &image {
                self.service.shared.audit_help.remember(self, image.clone());
            } else {
                self.service.shared.audit_help.clear();
            }
        }
        if !self.current() {
            return Err(CHANGED);
        }
        let bytes = self
            .command(
                vec!["desktop".into(), "request".into(), "--json".into()],
                Some(serde_json::to_string(&request).map_err(|_| UNAVAILABLE)?),
                resources,
                image,
            )
            .await
            .map_err(|_| UNAVAILABLE)?;
        let reply: Value = serde_json::from_slice(&bytes).map_err(|_| UNAVAILABLE)?;
        if !self.current() {
            return Err(CHANGED);
        }
        if !request.matches(&reply) {
            return Err(UNAVAILABLE);
        }
        if reply["accepted"] != true {
            return Err(if reply["code"] == "approval_audit_scope_changed" {
                CHANGED
            } else {
                UNAVAILABLE
            });
        }
        Ok(reply)
    }
    async fn command(
        &self,
        args: Vec<String>,
        input: Option<String>,
        resources: Arc<dyn Send + Sync>,
        image: Option<Arc<help::Image>>,
    ) -> Result<Vec<u8>, &'static str> {
        command_accounted(
            &self.service.shared,
            self.routing.clone(),
            zerus_client_core::Invocation {
                args,
                stdin: input.map(String::into_bytes),
                timeout: RPC_WAIT,
                request_id: None,
            },
            self.stop.clone(),
            wire::REPLY_BYTES,
            resources,
            image,
        )
        .await
        .map_err(|_| UNAVAILABLE)
    }
}

pub(crate) async fn list(
    service: Option<Arc<Service>>,
    host: String,
    session: String,
    before: i64,
    after: i64,
    scope: Option<Scope>,
) -> Result<Page, String> {
    let read = Read::new(service)?;
    let host = host.trim().to_owned();
    let session = session.trim().to_owned();
    if let Some(scope) = &scope {
        read.check(scope)?;
        if scope.host != host || scope.session != session {
            return Err(CHANGED.into());
        }
    } else if before != 0 || after != 0 {
        return Err(CHANGED.into());
    }
    if before < 0 || after < 0 || before > SAFE_INTEGER || after > SAFE_INTEGER {
        return Err(CHANGED.into());
    }
    let request = Request::List {
        request_id: uuid::Uuid::new_v4().to_string(),
        configuration: read.configuration.clone(),
        epoch: read.epoch.clone(),
        journal: scope.as_ref().map(|s| s.journal.clone()),
        through: scope.as_ref().map(|s| s.through),
        host: host.clone(),
        session: session.clone(),
        before: (before > 0).then_some(before),
        after: (after > 0).then_some(after),
    };
    let reply = read.query(request).await?;
    let result = reply["result"].as_object().ok_or(UNAVAILABLE)?;
    let page = approvals::page(result)?;
    let through = reply["result"]["through"].as_i64().ok_or(UNAVAILABLE)?;
    if through > SAFE_INTEGER || page.entries.iter().any(|entry| entry.id > SAFE_INTEGER) {
        return Err(UNAVAILABLE.into());
    }
    let scope = reply["journal"].as_str().map(|journal| Scope {
        configuration: read.configuration,
        epoch: read.epoch,
        generation: read.generation,
        journal: journal.into(),
        through,
        host,
        session,
    });
    Ok(Page { page, scope })
}

pub(crate) async fn detail(
    service: Option<Arc<Service>>,
    id: i64,
    scope: Option<Scope>,
) -> Result<String, String> {
    let read = Read::new(service)?;
    let scope = scope.ok_or(CHANGED)?;
    read.check(&scope)?;
    if id <= 0 || id > scope.through {
        return Err(CHANGED.into());
    }
    let reply = read
        .query(Request::Detail {
            request_id: uuid::Uuid::new_v4().to_string(),
            configuration: read.configuration.clone(),
            epoch: read.epoch.clone(),
            journal: scope.journal,
            id,
        })
        .await?;
    Ok(approvals::detail(
        reply["result"].as_object().ok_or(UNAVAILABLE)?,
    ))
}

#[cfg(all(test, unix))]
mod tests;
