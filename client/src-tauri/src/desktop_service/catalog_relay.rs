//! One owned read-only relay per walk, not one process per page. No policy
//! requests, implicit replay, service launch or unbounded request queue.
use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout};
use zerus_client_core::desktop::drain::Charge;

#[derive(Default)]
pub(super) struct Relay {
    session: Option<Session>,
    invalid: bool,
}
struct Admission {
    charge: Option<Charge>,
    exited: bool,
}
impl Drop for Admission {
    fn drop(&mut self) {
        if !self.exited {
            // A failed wait is not an exit proof. Pin this bounded admission
            // instead of allowing an unaccounted replacement relay.
            if let Some(charge) = self.charge.take() {
                std::mem::forget(charge);
            }
        }
    }
}
struct Session {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    error: ChildStderr,
    error_bytes: usize,
    error_open: bool,
    admission: Admission,
    routing: Arc<zerus_client_core::desktop::relay::Connection>,
}
impl Relay {
    pub(super) async fn request(
        &mut self,
        shared: &Arc<Shared>,
        configuration: &str,
        value: Value,
        stop: &Arc<AtomicBool>,
    ) -> Result<Value, &'static str> {
        if !shared.allowed {
            return Err("replay_transport_unavailable");
        }
        if self.invalid {
            return Err("catalog_relay_retired");
        }
        if !matches!(
            value["op"].as_str(),
            Some("catalog_page" | "catalog_changes" | "catalog_close")
        ) {
            return Err("invalid_catalog_request");
        }
        let mut input = serde_json::to_vec(&value).map_err(|_| "invalid_catalog_request")?;
        input.push(b'\n');
        if input.len() > zerus_watch_protocol::CONTROL_LINE_BYTES {
            return Err("service_request_too_large");
        }
        let exchange = async {
            if self.session.is_none() {
                self.session = Some(Session::start(shared, stop).await?);
                let hello = self
                    .session
                    .as_mut()
                    .expect("started relay")
                    .frame()
                    .await?;
                if hello["abi"] != service::RELAY_ABI || hello["configuration"] != configuration {
                    return Err("incompatible_catalog_relay");
                }
            }
            let session = self.session.as_mut().expect("started relay");
            // Preserve paired-installation freshness on every page, even
            // though the original relay process is retained across the walk.
            let routing = session.routing.clone();
            tauri::async_runtime::spawn_blocking(move || routing.executable().map(|_| ()))
                .await
                .map_err(|_| "paired_desktop_cli_unavailable")??;
            if shared.stopped.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
                return Err("stopped");
            }
            session
                .input
                .write_all(&input)
                .await
                .map_err(|_| "catalog_relay_unconfirmed")?;
            let response = session.frame().await?;
            if response["abi"] != service::RELAY_ABI
                || response["request_id"] != value["request_id"]
                || response["value"]["request_id"] != value["request_id"]
            {
                return Err("invalid_catalog_response");
            }
            Ok(response["value"].clone())
        };
        let result = tokio::select! {
            biased;
            _ = cancelled(shared, stop) => Err("stopped"),
            result = tokio::time::timeout(RPC_WAIT, exchange) => result.unwrap_or(Err("catalog_relay_unconfirmed")),
        };
        self.invalid |= result.is_err();
        result
    }
    pub(super) async fn close(&mut self) -> Result<(), &'static str> {
        self.close_until(RPC_WAIT).await
    }
    async fn close_until(&mut self, wait: Duration) -> Result<(), &'static str> {
        let Some(session) = self.session.take() else {
            return Ok(());
        };
        // Timeout bounds the caller, not resource custody. A retained output
        // pipe keeps this task and its admission alive through actual EOF.
        let task = tauri::async_runtime::spawn(session.shutdown());
        tokio::time::timeout(wait, task)
            .await
            .map_err(|_| "catalog_relay_drain_pending")?
            .map_err(|_| "catalog_relay_drain_unavailable")?
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            tauri::async_runtime::spawn(session.shutdown());
        }
    }
}
async fn cancelled(shared: &Shared, stop: &AtomicBool) {
    while !shared.stopped.load(Ordering::Acquire) && !stop.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
impl Session {
    async fn start(shared: &Shared, stop: &AtomicBool) -> Result<Self, &'static str> {
        let charge = shared
            .rpc
            .admit(Role::Fleet)
            .map_err(|_| "service_transport_busy")?;
        let routing = shared
            .relay
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or("service_unavailable")?;
        let selected = routing.clone();
        let hgs = tauri::async_runtime::spawn_blocking(move || {
            selected.executable().map(std::path::Path::to_owned)
        })
        .await
        .map_err(|_| "paired_desktop_cli_unavailable")??;
        if shared.stopped.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
            return Err("stopped");
        }
        let mut command = tokio::process::Command::new(hgs);
        command
            .args(["desktop", "catalog-relay", "--json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        crate::child_environment::restore(command.as_std_mut());
        command.env_remove("HGS_SESSION").env_remove("HGS_RUN_ID");
        #[cfg(windows)]
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        let mut child = command.spawn().map_err(|_| "catalog_relay_unavailable")?;
        let admission = Admission {
            charge: Some(charge),
            exited: false,
        };
        Ok(Self {
            input: child.stdin.take().expect("piped catalog stdin"),
            output: BufReader::new(child.stdout.take().expect("piped catalog stdout")),
            error: child.stderr.take().expect("piped catalog stderr"),
            child,
            admission,
            routing,
            error_bytes: 0,
            error_open: true,
        })
    }
    async fn frame(&mut self) -> Result<Value, &'static str> {
        let output = line(&mut self.output);
        tokio::pin!(output);
        let mut error = [0; 4096];
        loop {
            tokio::select! {
                result = &mut output => return result,
                count = self.error.read(&mut error), if self.error_open => {
                    let count = count.map_err(|_| "catalog_relay_unconfirmed")?;
                    self.error_open = count != 0;
                    self.error_bytes += count;
                    if self.error_bytes > 64 << 10 { return Err("catalog_relay_error_limit"); }
                }
            }
        }
    }
    async fn shutdown(self) -> Result<(), &'static str> {
        let Self {
            mut child,
            input,
            mut output,
            mut error,
            mut admission,
            ..
        } = self;
        drop(input); // The closed CLI command exits on stdin EOF.
        let wait = async {
            let exit = match tokio::time::timeout(Duration::from_millis(100), child.wait()).await {
                Ok(exit) => exit,
                Err(_) => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            admission.exited = exit.is_ok();
            let status = exit.map_err(|_| "catalog_relay_exit_unverified")?;
            if status.success() {
                Ok(())
            } else {
                Err("catalog_relay_unconfirmed")
            }
        };
        // Discard during retirement, using fixed buffers, while retaining the
        // actual original child and both readers. No descendant is stopped.
        let (mut output_sink, mut error_sink) = (tokio::io::sink(), tokio::io::sink());
        let (exit, output, error) = tokio::join!(
            wait,
            tokio::io::copy(&mut output, &mut output_sink),
            tokio::io::copy(&mut error, &mut error_sink)
        );
        exit?;
        output.map_err(|_| "catalog_relay_drain_unavailable")?;
        error.map_err(|_| "catalog_relay_drain_unavailable")?;
        Ok(())
    }
}
async fn line(reader: &mut BufReader<ChildStdout>) -> Result<Value, &'static str> {
    let mut bytes = Vec::new();
    loop {
        let part = reader
            .fill_buf()
            .await
            .map_err(|_| "catalog_relay_unconfirmed")?;
        if part.is_empty() {
            return Err("catalog_relay_unconfirmed");
        }
        let end = part.iter().position(|byte| *byte == b'\n');
        let count = end.map_or(part.len(), |end| end + 1);
        if bytes.len() + count > service::FRAME_BYTES {
            return Err("catalog_relay_frame_limit");
        }
        bytes.extend_from_slice(&part[..count]);
        reader.consume(count);
        if end.is_some() {
            return serde_json::from_slice(&bytes).map_err(|_| "invalid_catalog_response");
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::tests::Fixture;
    use super::*;

    async fn until(check: impl Fn() -> bool) {
        let until = tokio::time::Instant::now() + RPC_WAIT;
        while !check() {
            assert!(tokio::time::Instant::now() < until);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    async fn isolated(config: Value) -> (Fixture, Arc<Service>) {
        let fixture = Fixture::new(config);
        let (service, _) = fixture.start(true);
        until(|| service.snapshot().code == "incompatible_service_cli").await;
        *service.shared.relay.lock().unwrap() = Some(Arc::new(
            zerus_client_core::desktop::relay::Connection::select(&fixture.0.join("hgs")).unwrap(),
        ));
        (fixture, service)
    }
    fn page() -> Value {
        json!({"op":"catalog_page","request_id":uuid::Uuid::new_v4().to_string(),"computer":"","cursor":null})
    }
    #[test]
    fn malformed_or_cancelled_relay_is_retired_without_a_second_request_and_drains() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        tauri::async_runtime::block_on(async {
            for fault in [
                "configuration",
                "identity",
                "oversize",
                "incomplete",
                "stderr",
                "hang",
            ] {
                let (fixture, service) =
                    isolated(json!({"supported":false,"relay_fault":fault})).await;
                let stop = Arc::new(AtomicBool::new(false));
                let configuration = "a".repeat(64);
                let mut relay = Relay::default();
                let cancel = async {
                    if fault == "hang" {
                        until(|| fixture.0.join("relay-held").exists()).await;
                        stop.store(true, Ordering::Release);
                    }
                };
                let (result, _) = tokio::join!(
                    relay.request(&service.shared, &configuration, page(), &stop),
                    cancel
                );
                assert!(result.is_err(), "{fault}");
                assert_eq!(
                    relay
                        .request(&service.shared, &configuration, page(), &stop)
                        .await
                        .unwrap_err(),
                    "catalog_relay_retired"
                );
                let _ = relay.close().await;
                assert_eq!(service.shared.rpc.snapshot().unwrap().admitted, 0);
                let calls = fixture.calls();
                assert_eq!(
                    calls
                        .iter()
                        .filter(|call| call["value"]["op"] == "catalog_page")
                        .count(),
                    usize::from(fault != "configuration")
                );
                service.join().await.unwrap();
            }
        });
    }
    #[test]
    fn original_exit_does_not_release_admission_before_the_actual_output_pipe_eof() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        tauri::async_runtime::block_on(async {
            let (_fixture, service) = isolated(json!({"supported":false})).await;
            let stop = Arc::new(AtomicBool::new(false));
            let configuration = "a".repeat(64);
            let mut relay = Relay::default();
            relay
                .request(&service.shared, &configuration, page(), &stop)
                .await
                .unwrap();
            // A separately owned fixture retains this real OS output pipe.
            // Both original Child handles are waited; no orphan fixture job.
            let mut retained = tokio::process::Command::new("python3")
                .args(["-c", "import sys; sys.stdin.buffer.read()"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let input = retained.stdin.take();
            relay.session.as_mut().unwrap().output =
                BufReader::new(retained.stdout.take().unwrap());
            let closed = relay.close_until(Duration::from_millis(50)).await;
            let admitted = service.shared.rpc.snapshot().unwrap().admitted;
            drop(input);
            retained.wait().await.unwrap();
            until(|| service.shared.rpc.snapshot().unwrap().admitted == 0).await;
            service.join().await.unwrap();
            assert_eq!(closed, Err("catalog_relay_drain_pending"));
            assert_eq!(admitted, 1, "original exit alone is not a full drain");
        });
    }
}
