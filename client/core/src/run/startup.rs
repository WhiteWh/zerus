//! Owned duplex startup helper over the existing runner. One original worker,
//! input writer and both pipe readers stay retained through verified completion.
use super::*;
use crate::desktop::startup::{Frame, Refused, FRAME_BYTES, HELP_MARKER, WAIT};
use std::io::{BufRead, BufReader};

pub struct Bootstrap {
    worker: Option<thread::JoinHandle<Result<(), Refused>>>,
    ready: mpsc::Receiver<String>,
    release: Option<mpsc::SyncSender<String>>,
    configuration: Option<String>,
    resources: Arc<()>,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
    joined: Option<Result<(), Refused>>,
}
impl Bootstrap {
    /// Call once for the claimed canonical production instance, before any GUI
    /// writer startup. The host must not publish its configuration until finish.
    pub fn begin(hgs: &Path, nonce: &str, environment: &[(&str, &str)]) -> Result<Self, Refused> {
        if !cfg!(unix) {
            return Err(Refused::Unavailable);
        }
        if !crate::creation::canonical_uuid(nonce) {
            return Err(Refused::Invalid);
        }
        Self::begin_until(hgs, nonce, environment, Instant::now() + WAIT)
    }
    fn begin_until(
        hgs: &Path,
        nonce: &str,
        environment: &[(&str, &str)],
        deadline: Instant,
    ) -> Result<Self, Refused> {
        let hgs = hgs.to_owned();
        let environment: Vec<(String, String)> = environment
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let nonce = nonce.to_owned();
        let resources = Arc::new(());
        let owned = resources.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let (ready_tx, ready) = mpsc::sync_channel::<String>(1);
        let (release, release_rx) = mpsc::sync_channel::<String>(1);
        let worker = thread::Builder::new()
            .name("zerus-startup-helper".into())
            .spawn(move || {
                let environment: Vec<(&str, &str)> = environment
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                let invocation = |args: Vec<String>| Invocation {
                    args,
                    stdin: None,
                    timeout: deadline.saturating_duration_since(Instant::now()),
                    request_id: None,
                };
                let charge = ChildCharge::new(owned.clone());
                // Exact marker first: an unknown subcommand can start a session on
                // an older selected hgs. No fallback command or retry is permitted.
                let help = run_inner(
                    &hgs,
                    &invocation(vec!["--help".into()]),
                    &environment,
                    &[],
                    256 << 10,
                    Some(&stop),
                    Pipes {
                        readers: Some(owned.clone()),
                        require_complete: true,
                        child_exited: Some(charge.exited.clone()),
                        ..Pipes::default()
                    },
                );
                if matches!(help, Ok(Exit::StartFailed(_))) {
                    charge.confirmed();
                }
                let Ok(Exit::Code {
                    code: 0, stdout, ..
                }) = help
                else {
                    return Err(Refused::Unavailable);
                };
                let help = std::str::from_utf8(&stdout).map_err(|_| Refused::Unavailable)?;
                if !help.lines().any(|line| line.trim() == HELP_MARKER) {
                    return Err(Refused::Unsupported);
                }
                drop(charge);
                if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                    return Err(Refused::TimedOut);
                }
                let input_nonce = nonce.clone();
                let output_nonce = nonce;
                let charge = ChildCharge::new(owned.clone());
                let io_stop = stop.clone();
                let pipes = Pipes {
                    input: Some(Box::new(move |mut input| {
                        let Some(hold) = Frame::encode("hold", &input_nonce, None) else {
                            return false;
                        };
                        if input.write_all(&hold).and_then(|_| input.flush()).is_err() {
                            return false;
                        }
                        let Ok(configuration) = release_rx
                            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        else {
                            return false;
                        };
                        if Instant::now() >= deadline {
                            return false;
                        }
                        let Some(release) =
                            Frame::encode("release", &input_nonce, Some(&configuration))
                        else {
                            return false;
                        };
                        input
                            .write_all(&release)
                            .and_then(|_| input.flush())
                            .is_ok()
                    })),
                    output: Some(Box::new(move |output| {
                        let mut input = BufReader::new(output);
                        let read = |input: &mut BufReader<std::process::ChildStdout>,
                                    op: &str|
                         -> Option<Frame> {
                            if io_stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                                return None;
                            }
                            let mut bytes = Vec::new();
                            input
                                .take((FRAME_BYTES + 1) as u64)
                                .read_until(b'\n', &mut bytes)
                                .ok()?;
                            if bytes.len() > FRAME_BYTES
                                || !bytes.ends_with(b"\n")
                                || Instant::now() >= deadline
                            {
                                return None;
                            }
                            let value: Frame = serde_json::from_slice(&bytes).ok()?;
                            value.checked(op, &output_nonce)?;
                            Some(value)
                        };
                        let Some(first) = read(&mut input, "ready") else {
                            return false;
                        };
                        let configuration = first.configuration().unwrap().to_string();
                        if ready_tx.send(configuration.clone()).is_err() {
                            return false;
                        }
                        let Some(last) = read(&mut input, "released") else {
                            return false;
                        };
                        if last.configuration() != Some(&configuration) {
                            return false;
                        }
                        let mut tail = [0];
                        input.read(&mut tail).is_ok_and(|n| n == 0) && Instant::now() < deadline
                    })),
                    readers: Some(owned.clone()),
                    require_complete: true,
                    retain_all: true,
                    child_exited: Some(charge.exited.clone()),
                    stderr_cap: Some(16 << 10),
                    ..Pipes::default()
                };
                let result = run_inner(
                    &hgs,
                    &invocation(vec!["desktop".into(), "startup".into(), "--json".into()]),
                    &environment,
                    &[],
                    FRAME_BYTES,
                    Some(&stop),
                    pipes,
                );
                if matches!(result, Ok(Exit::StartFailed(_))) {
                    charge.confirmed();
                }
                if matches!(result, Ok(Exit::Code { code: 0, .. })) && Instant::now() < deadline {
                    Ok(())
                } else {
                    Err(Refused::Unknown)
                }
            })
            .map_err(|_| Refused::Unavailable)?;
        Ok(Self {
            worker: Some(worker),
            ready,
            release: Some(release),
            configuration: None,
            resources,
            cancel,
            deadline,
            joined: None,
        })
    }
    /// Before the event loop/writers exist; never call from an active GUI actor.
    pub fn wait_ready(&mut self) -> Result<(), Refused> {
        if self.configuration.is_some() {
            return Err(Refused::Invalid);
        }
        match self
            .ready
            .recv_timeout(self.deadline.saturating_duration_since(Instant::now()))
        {
            Ok(configuration) => self.configuration = Some(configuration),
            Err(_) => {
                // A missing marker refuses ONLY the new path. Confirm the exact
                // help worker and all pipe copies ended first. A started helper's
                // unknown/refused outcome is never legacy compatibility permission.
                while self.worker.as_ref().is_some_and(|h| !h.is_finished())
                    && Instant::now() < self.deadline
                {
                    thread::sleep(POLL);
                }
                if !self.worker.as_ref().is_some_and(|h| h.is_finished()) {
                    return Err(Refused::Unavailable);
                }
                let result = self
                    .worker
                    .take()
                    .unwrap()
                    .join()
                    .unwrap_or(Err(Refused::Unknown));
                // A writer may signal completion just before dropping its final
                // capture. Keep the original budget while that real copy ends;
                // scheduling alone must not turn a valid old CLI into refusal.
                if result == Err(Refused::Unsupported) {
                    while Arc::strong_count(&self.resources) > 1 && Instant::now() < self.deadline {
                        thread::sleep(POLL);
                    }
                }
                let unsupported = result == Err(Refused::Unsupported)
                    && Instant::now() < self.deadline
                    && Arc::strong_count(&self.resources) == 1;
                self.joined = Some(result);
                return Err(if unsupported {
                    Refused::Unsupported
                } else {
                    Refused::Unavailable
                });
            }
        }
        if Instant::now() >= self.deadline {
            return Err(Refused::TimedOut);
        }
        Ok(())
    }
    /// Nonblocking request only. The caller must still poll and join the exact
    /// original worker; delivery or a release reply alone never means finished.
    pub fn release(&mut self) -> Result<(), Refused> {
        let configuration = self.configuration.as_ref().ok_or(Refused::Invalid)?;
        self.release
            .take()
            .ok_or(Refused::Invalid)?
            .try_send(configuration.clone())
            .map_err(|_| Refused::Unknown)
    }
    pub fn try_finish(&mut self) -> Result<Option<String>, Refused> {
        if Instant::now() >= self.deadline {
            return Err(Refused::TimedOut);
        }
        if self.release.is_some() {
            return Err(Refused::Invalid);
        }
        if let Some(worker) = &self.worker {
            if !worker.is_finished() {
                return Ok(None);
            }
            self.joined = Some(
                self.worker
                    .take()
                    .unwrap()
                    .join()
                    .unwrap_or(Err(Refused::Unknown)),
            );
        }
        match self.joined.as_ref().ok_or(Refused::Invalid)? {
            Err(error) => Err(*error),
            Ok(()) if Arc::strong_count(&self.resources) == 1 => {
                self.configuration.take().map(Some).ok_or(Refused::Invalid)
            }
            Ok(()) => Ok(None),
        }
    }
}
impl Drop for Bootstrap {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.release.take();
        // An outer wait/drop cannot turn live pipes or an unjoined worker into
        // completion. One fixed bootstrap per canonical process; no replacement.
        if self.worker.is_some() || Arc::strong_count(&self.resources) > 1 {
            std::mem::forget(self.resources.clone());
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
