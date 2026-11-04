//! One native creation intent, then only scoped reads of its original result.
//! The admitted service job survives this reader and the entire GUI exiting.
use super::audit::help::Image;
use super::*;
use std::time::Instant;
use zerus_client_core::{
    creation,
    desktop::{
        creation_clone as wire, relay,
        ui_requests::{Registration, Request},
    },
    run, Invocation,
};

pub(crate) const UNAVAILABLE: &str =
    zerus_client_core::source!("Repository cloning is not available from this desktop service.");

struct Read {
    service: Arc<Service>,
    scope: wire::Scope,
    generation: u64,
    cancel: Arc<AtomicBool>,
    routing: Arc<relay::Connection>,
    image: Arc<Image>,
}
impl Read {
    fn new(service: Option<Arc<Service>>) -> Result<Arc<Self>, String> {
        let service = service.ok_or(creation::READ_FAILED)?;
        let shared = &service.shared;
        let snapshot = service.snapshot();
        if !shared.allowed || !snapshot.connected || shared.stopped.load(Ordering::Acquire) {
            return Err(creation::READ_FAILED.into());
        }
        let configuration = shared
            .configuration
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or(creation::READ_FAILED)?;
        let cancel = shared
            .cancellation
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or(creation::READ_FAILED)?;
        let routing = shared
            .relay
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or(creation::READ_FAILED)?;
        let image = Arc::new(Image::capture(routing.executable()?).ok_or(creation::READ_FAILED)?);
        let value = Arc::new(Self {
            service,
            scope: wire::Scope {
                configuration,
                service_epoch: snapshot.epoch.ok_or(creation::READ_FAILED)?,
            },
            generation: snapshot.generation.ok_or(creation::READ_FAILED)?,
            cancel,
            routing,
            image,
        });
        if !value.current() {
            return Err(creation::READ_FAILED.into());
        }
        Ok(value)
    }
    fn current(&self) -> bool {
        let shared = &self.service.shared;
        let snapshot = self.service.snapshot();
        !self.cancel.load(Ordering::Acquire)
            && !shared.stopped.load(Ordering::Acquire)
            && snapshot.connected
            && snapshot.epoch.as_deref() == Some(self.scope.service_epoch.as_str())
            && snapshot.generation == Some(self.generation)
            && shared
                .configuration
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_deref()
                == Some(self.scope.configuration.as_str())
            && shared
                .cancellation
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .is_some_and(|v| Arc::ptr_eq(v, &self.cancel))
            && shared
                .relay
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .is_some_and(|v| Arc::ptr_eq(v, &self.routing))
    }
    fn exchange(
        self: &Arc<Self>,
        request: Option<Request>,
        input: Value,
        help: bool,
        extra: Option<zerus_client_core::desktop::drain::Charge>,
    ) -> Result<Vec<u8>, String> {
        if !self.current() {
            return Err(creation::READ_FAILED.into());
        }
        let rpc = self
            .service
            .shared
            .rpc
            .admit(Role::Fleet)
            .map_err(|_| creation::READ_FAILED)?;
        let registration = match request {
            Some(request) => Some(
                self.service
                    .shared
                    .responder
                    .as_ref()
                    .ok_or(creation::READ_FAILED)?
                    .prepare_ui_request(uuid::Uuid::new_v4().to_string(), request)
                    .map_err(|_| creation::READ_FAILED)?,
            ),
            None => None,
        };
        let input = if let Some(registration) = &registration {
            serde_json::to_vec(registration.envelope())
        } else if help {
            Ok(Vec::new())
        } else {
            serde_json::to_vec(&input)
        }
        .map_err(|_| creation::READ_FAILED)?;
        if input.len() > zerus_watch_protocol::CONTROL_LINE_BYTES {
            return Err(creation::READ_FAILED.into());
        }
        let helper = self.routing.executable()?;
        if !self.image.current(helper) {
            return Err(creation::READ_FAILED.into());
        }
        let args = if help {
            vec!["--help".into()]
        } else {
            vec![
                "desktop".into(),
                if registration.is_some() {
                    "frontend-request"
                } else {
                    "request"
                }
                .into(),
                "--json".into(),
            ]
        };
        let invocation = Invocation {
            args,
            stdin: Some(input),
            timeout: RPC_WAIT,
            request_id: None,
        };
        let held: Arc<dyn Send + Sync> = Arc::new((
            rpc,
            extra,
            self.clone(),
            registration.as_ref().map(Registration::resources),
        ));
        let result = run::run_owned(
            helper,
            &invocation,
            &[],
            &self.cancel,
            move |command| {
                let child = command.spawn()?;
                if let Some(registration) = registration {
                    let _ = registration.bind_original(&child);
                }
                Ok(child) // Custody is retained even when binding refuses.
            },
            held,
        )
        .map_err(|_| creation::READ_FAILED)?;
        if !self.current()
            || !self.image.current(self.routing.executable()?)
            || result.len() > if help { 64 << 10 } else { wire::REPLY_BYTES }
        {
            return Err(creation::READ_FAILED.into());
        }
        Ok(result)
    }
    fn supported(self: &Arc<Self>) -> Result<bool, String> {
        let bytes = self.exchange(None, Value::Null, true, None)?;
        let help = std::str::from_utf8(&bytes).map_err(|_| creation::READ_FAILED)?;
        for marker in [
            wire::HELP_MARKER,
            zerus_client_core::desktop::ui_requests::HELP_MARKER,
        ] {
            if !help.lines().any(|line| line.trim() == marker) {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// Called only by the native creation host after its ordered journal barrier.
/// `fresh` is consumed once from that host's new-submit set, never from JSON.
pub(crate) fn call(
    service: Option<Arc<Service>>,
    id: &str,
    operation: &Value,
    fresh: bool,
    wait: bool,
    shutdown: &AtomicBool,
) -> creation::Answer {
    let result = (|| -> Result<Value, String> {
        let read = Read::new(service)?;
        if !creation::valid_operation(id, operation) {
            return Err(creation::READ_FAILED.into());
        }
        if !read.supported()? {
            // This exact fresh intent has not reached frontend-request. An
            // older CLI cannot establish the outcome of an existing original.
            if fresh {
                let clone = operation["clone_id"]
                    .as_str()
                    .ok_or(creation::READ_FAILED)?;
                if !creation::canonical_uuid(clone) {
                    return Err(creation::READ_FAILED.into());
                }
                let target = operation["target"].as_str().ok_or(creation::READ_FAILED)?;
                return Ok(json!({"status":"failed", "request_id":clone,
                    "path":target, "detail":UNAVAILABLE}));
            }
            return Err(creation::READ_FAILED.into());
        }
        let plan = wire::Plan::from_operation(id, operation).ok_or(creation::READ_FAILED)?;
        if fresh {
            let until = Instant::now() + RPC_WAIT;
            let charge = loop {
                if shutdown.load(Ordering::Acquire) || !read.current() || Instant::now() >= until {
                    return Err(creation::READ_FAILED.into());
                }
                match read.service.shared.creation_rpc.admit(Role::Fleet) {
                    Ok(charge) => break charge,
                    Err(zerus_client_core::desktop::drain::Refused::Capacity) => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => return Err(creation::READ_FAILED.into()),
                }
            };
            let intent = wire::Intent {
                scope: read.scope.clone(),
                creation_id: id.into(),
                clone_request_id: plan.clone_request_id.clone(),
                revision: operation["revision"]
                    .as_u64()
                    .ok_or(creation::READ_FAILED)?,
                original: serde_json::to_string(operation).map_err(|_| creation::READ_FAILED)?,
            };
            intent.clone().verify().map_err(|_| creation::READ_FAILED)?;
            let bytes = read.exchange(
                Some(Request::CreationClone {
                    intent: Box::new(intent.clone()),
                }),
                Value::Null,
                false,
                Some(charge),
            )?;
            let admission: wire::Admission =
                serde_json::from_slice(&bytes).map_err(|_| creation::READ_FAILED)?;
            if !admission.matches(&intent) {
                return Err(creation::READ_FAILED.into());
            }
            if let Some(result) = admission.receipt.and_then(|r| r.result(&plan)) {
                return Ok(result);
            }
        }
        let until = Instant::now() + Duration::from_secs(650);
        let mut delay = Duration::from_millis(250);
        loop {
            if shutdown.load(Ordering::Acquire) || !read.current() {
                return Err(creation::READ_FAILED.into());
            }
            let query = wire::Query::for_plan(read.scope.clone(), id.into(), &plan)
                .ok_or(creation::READ_FAILED)?;
            let bytes = read.exchange(
                None,
                json!({"op":"creation_clone_result","query":query}),
                false,
                None,
            )?;
            let lookup: wire::Lookup =
                serde_json::from_slice(&bytes).map_err(|_| creation::READ_FAILED)?;
            if !lookup.matches(&query) {
                return Err(creation::READ_FAILED.into());
            }
            let receipt = lookup.receipt.ok_or(creation::READ_FAILED)?; // Missing never authorizes Start.
            if let Some(result) = receipt.result(&plan) {
                return Ok(result);
            }
            if !wait || receipt.drained || Instant::now() >= until {
                return Err(creation::READ_FAILED.into());
            }
            let next = Instant::now() + delay;
            while Instant::now() < next {
                if shutdown.load(Ordering::Acquire) || !read.current() {
                    return Err(creation::READ_FAILED.into());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            delay = (delay * 2).min(Duration::from_secs(5));
        }
    })();
    match result {
        Ok(data) => creation::Answer {
            ok: true,
            data,
            error: String::new(),
        },
        Err(_) => creation::Answer {
            ok: false,
            data: Value::Null,
            error: zerus_client_core::repository::UNCONFIRMED_OPERATION.into(),
        },
    }
}

#[cfg(all(test, unix))]
mod tests;
