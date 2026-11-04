//! One audit reader on the existing bounded original client I/O worker.
//! Actor admission does no filesystem/SQLite work and never grants effects.
use super::Reader;
use crate::{
    config::Config,
    desktop::{frame, protocol, store::Location, transport, Event, Hello, ABI, TIMEOUT},
    platform,
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    io::Write,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
};
use zerus_client_core::desktop::approval_audit::{self as wire, Request};

pub(in crate::desktop) struct Worker {
    reader: Mutex<Reader>,
    configuration: String,
    epoch: String,
    stopped: Arc<AtomicBool>,
    busy: AtomicBool,
}
impl Worker {
    pub fn new(
        config: &Config,
        location: &Location,
        epoch: &str,
        stopped: Arc<AtomicBool>,
    ) -> Self {
        Self::from_reader(Reader::new(config, location, epoch), stopped)
    }
    fn from_reader(reader: Reader, stopped: Arc<AtomicBool>) -> Self {
        Self {
            configuration: reader.configuration.clone(),
            epoch: reader.epoch.clone(),
            reader: Mutex::new(reader),
            stopped,
            busy: AtomicBool::new(false),
        }
    }
    fn refused(&self, request: &Value, code: &str) -> Value {
        json!({"abi":wire::ABI,"request_id":request["request_id"].as_str().unwrap_or(""),
            "configuration":self.configuration,"service_epoch":self.epoch,
            "accepted":false,"read_only":true,"code":code})
    }
    pub fn unavailable(&self, request: &Value) -> Value {
        self.refused(request, "approval_audit_unavailable")
    }
    /// Only the actor supplies its original epoch/sequence and actual serve
    /// owner. No file/connection lock is taken on this admission path.
    pub fn admit(
        self: &Arc<Self>,
        hello: Hello,
        owner: Arc<platform::FileLock>,
        seq: u64,
    ) -> Reply {
        let result = if hello.abi != ABI || hello.configuration != self.configuration {
            Job::Error(json!({"code":"incompatible_configuration"}))
        } else {
            let value = hello.request.unwrap_or(Value::Null);
            let request = serde_json::from_value::<Request>(value.clone());
            match request {
                Ok(request) if hello.op == "request" && request.valid() => {
                    if request.configuration() != self.configuration
                        || request.epoch() != self.epoch
                    {
                        Job::Value(self.refused(&value, "approval_audit_scope_changed"))
                    } else if self.stopped.load(Ordering::SeqCst)
                        || self
                            .busy
                            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                            .is_err()
                    {
                        Job::Value(self.refused(&value, "approval_audit_unavailable"))
                    } else {
                        Job::Read(Work {
                            worker: self.clone(),
                            _owner: owner,
                            request: Some(value),
                        })
                    }
                }
                _ => Job::Value(self.refused(&value, "invalid_approval_audit_request")),
            }
        };
        Reply {
            epoch: self.epoch.clone(),
            seq,
            job: result,
        }
    }
}
struct Work {
    // Drop the reader before its last original service owner, including when
    // shutdown has already dropped the actor's copy of this Worker.
    worker: Arc<Worker>,
    _owner: Arc<platform::FileLock>,
    request: Option<Value>,
}
impl Work {
    fn read(&mut self) -> Value {
        let request = self.request.take().expect("one original audit query");
        let unavailable = || self.worker.refused(&request, "approval_audit_unavailable");
        if self.worker.stopped.load(Ordering::SeqCst) {
            return unavailable();
        }
        let result = match self.worker.reader.lock() {
            Ok(mut reader) => reader.request(&request),
            Err(_) => return unavailable(),
        };
        if self.worker.stopped.load(Ordering::SeqCst) {
            unavailable()
        } else {
            result
        }
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        // Session closes the original transport first. Busy is an admission
        // bound, never a claim that this owner's kernel resources drained.
        self.worker.busy.store(false, Ordering::SeqCst);
    }
}
enum Job {
    Error(Value),
    Value(Value),
    Read(Work),
}
pub(in crate::desktop) struct Reply {
    epoch: String,
    seq: u64,
    job: Job,
}
impl Reply {
    fn response(&mut self) -> Value {
        let (kind, value) = match &mut self.job {
            Job::Error(value) => ("error", value.clone()),
            Job::Value(value) => ("response", value.clone()),
            Job::Read(work) => ("response", work.read()),
        };
        frame(&self.epoch, self.seq, kind, value)
    }
}
struct Session {
    // The only original connection handle drops before the work/owner loan.
    stream: transport::Stream,
    reply: Option<Reply>,
}
impl Drop for Session {
    fn drop(&mut self) {
        transport::shutdown(&self.stream);
    }
}
pub(in crate::desktop) fn recognizes(hello: &Hello) -> bool {
    hello.op == "request"
        && hello.request.as_ref().is_some_and(|value| {
            matches!(
                value["op"].as_str(),
                Some("approval_audit_list" | "approval_audit_detail")
            )
        })
}
/// Called only after the existing transport's native peer authentication and
/// bounded hello read. It adds no listener, client slot or worker thread.
pub(in crate::desktop) fn client(
    stream: transport::Stream,
    hello: Hello,
    send: mpsc::SyncSender<Event>,
) -> Result<()> {
    let mut session = Session {
        stream,
        reply: None,
    };
    let (answer, receive) = mpsc::sync_channel(1);
    send.try_send(Event::ApprovalAudit(hello, answer))
        .map_err(|_| anyhow::anyhow!("desktop audit admission is full"))?;
    session.reply = Some(
        receive
            .recv_timeout(TIMEOUT)
            .context("desktop audit admission unavailable")?,
    );
    let reply = session
        .reply
        .as_mut()
        .expect("original admission")
        .response();
    let bytes = protocol::encode(&reply)?;
    ensure!(
        bytes.len() <= protocol::FRAME_BYTES,
        "audit response exceeds its bound"
    );
    session.stream.write_all(&bytes)?;
    session.stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests;
