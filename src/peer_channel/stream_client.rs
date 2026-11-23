//! Local watch transport. No stdin is consumed until the broker accepts it.
use super::*;
use crate::host::pipe::PipeStream;

pub(super) fn watch(config: &Config, alias: &str, args: &[String]) -> Option<Result<i32, String>> {
    let stream = match start(config, alias, args)? {
        Ok(stream) => stream,
        Err(error) => return Some(Err(error)),
    };
    Some(
        watch_io(stream, io::stdin(), io::stdout(), io::stderr())
            .map_err(|error| format!("{alias}: {error}")),
    )
}

fn start(config: &Config, alias: &str, args: &[String]) -> Option<Result<PipeStream, String>> {
    if !multiplex::watch_args(args)
        || std::env::var_os("HGS_PEER_CHANNEL").is_some_and(|v| v == "0")
        || failed_recently(config, alias, None)
    {
        return None;
    }
    let name = pipe_name(config, alias).ok()?;
    let deadline = Instant::now() + START;
    let mut probe = connect(config, &name, alias, true, deadline)?;
    // A v1 broker safely forwards this missing-argv probe to a v1 serve,
    // which refuses it before spawning anything. Never probe with a watch.
    let supported = {
        let mut probe = DeadlineStream {
            stream: &mut probe,
            deadline,
        };
        write_frame(&mut probe, &json!({"channel":"capabilities"})).ok()?;
        read_frame(&mut probe)
            .ok()
            .flatten()
            .is_some_and(|hello| multiplex::streams(&hello))
    };
    probe.shutdown();
    if !supported {
        return None;
    }
    let mut stream = connect(config, &name, alias, false, deadline)?;
    let accepted = {
        let mut timed = DeadlineStream {
            stream: &mut stream,
            deadline,
        };
        if write_frame(&mut timed, &json!({"args":args,"stream":true})).is_err() {
            return Some(Err(format!(
                "link to {alias} dropped while submitting watch"
            )));
        }
        match read_frame(&mut timed) {
            Ok(Some(frame)) if frame["unsupported_stream"] == true => return None,
            Ok(Some(frame)) if frame["stream"] == true => true,
            _ => false,
        }
    };
    if !accepted {
        return Some(Err(format!("link to {alias} dropped while starting watch")));
    }
    Some(Ok(stream))
}

fn watch_io(
    mut stream: PipeStream,
    mut input: impl Read + Send + 'static,
    mut stdout: impl Write,
    mut stderr: impl Write,
) -> io::Result<i32> {
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut to_broker = stream.try_clone()?;
    thread::spawn(move || {
        let mut buffer = vec![0; multiplex::CHUNK];
        loop {
            let size = match input.read(&mut buffer) {
                Ok(size) => size,
                Err(_) => break,
            };
            let frame = if size == 0 {
                json!({"input":null})
            } else {
                json!({"input":STANDARD.encode(&buffer[..size])})
            };
            if write_frame(&mut to_broker, &frame).is_err() || size == 0 {
                break;
            }
        }
    });
    let result = (|| loop {
        let frame = read_frame(&mut stream)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "peer watch connection ended")
        })?;
        if let Some(error) = frame["error"].as_str() {
            return Err(io::Error::other(error.to_owned()));
        }
        if let Some(code) = frame["code"].as_i64() {
            return Ok(code as i32);
        }
        let data = multiplex::decode_chunk(&frame["chunk"])?;
        let writer: &mut dyn Write = match frame["channel"].as_str() {
            Some("stdout") => &mut stdout,
            Some("stderr") => &mut stderr,
            _ => return Err(io::ErrorKind::InvalidData.into()),
        };
        writer.write_all(&data)?;
        writer.flush()?;
    })();
    stream.shutdown();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(root: &std::path::Path) -> Config {
        Config {
            home: root.to_string_lossy().into_owned(),
            dir: root.join("config"),
            state: root.join("state"),
            selves: vec!["example-local".into()],
            peers: vec!["example-peer".into()],
            base_peers: vec![],
            peers_from_environment: false,
            machines: std::collections::BTreeMap::new(),
            tab: false,
            tab_colors: String::new(),
        }
    }

    #[test]
    fn native_pipe_negotiates_and_v1_never_receives_a_watch() {
        use crate::host::{pipe::PipeListener, token::Identity};
        for mode in ["v1-broker", "v1-peer", "v2", "lost-after-submission"] {
            let root = tempfile::tempdir().unwrap();
            let config = config(root.path());
            let name = pipe_name(&config, "example-peer").unwrap();
            let listener =
                Arc::new(PipeListener::bind(&name, &Identity::current().unwrap()).unwrap());
            let owned = listener.clone();
            let server = thread::spawn(move || {
                let mut probe = owned.accept().unwrap();
                probe
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let frame = read_frame(&mut probe).unwrap().unwrap();
                assert_eq!(frame, json!({"channel":"capabilities"}));
                assert!(
                    frame.get("args").is_none(),
                    "v1 probe must never submit watch argv"
                );
                let hello = match mode {
                    "v1-broker" => json!({"error":"this request cannot run on the peer"}),
                    "v1-peer" => json!({"hello":1,"protocol":2,"streams":false}),
                    _ => json!({"hello":1,"protocol":2,"streams":true}),
                };
                write_frame(&mut probe, &hello).unwrap();
                probe.shutdown();
                if mode.starts_with("v1") {
                    return;
                }
                let mut stream = owned.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let request = read_frame(&mut stream).unwrap().unwrap();
                assert_eq!(request, json!({"args":["watch","--json"],"stream":true}));
                if mode == "lost-after-submission" {
                    return;
                }
                write_frame(&mut stream, &json!({"stream":true})).unwrap();
                let input = read_frame(&mut stream).unwrap().unwrap();
                assert_eq!(bytes(&input["input"]), b"{\"type\":\"subscribe\"}\n");
                assert!(read_frame(&mut stream).unwrap().unwrap()["input"].is_null());
                for data in [
                    b"{\"type\":\"hello\"}\n".as_slice(),
                    b"{\"type\":\"heartbeat\"}\n",
                ] {
                    write_frame(
                        &mut stream,
                        &json!({"id":7,"channel":"stdout","chunk":STANDARD.encode(data)}),
                    )
                    .unwrap();
                }
                write_frame(
                    &mut stream,
                    &json!({"id":7,"channel":"stderr","chunk":STANDARD.encode(b"diagnostic")}),
                )
                .unwrap();
                write_frame(&mut stream, &json!({"id":7,"code":0})).unwrap();
            });
            let started = start(&config, "example-peer", &["watch".into(), "--json".into()]);
            if mode.starts_with("v1") {
                assert!(started.is_none());
            } else if mode == "lost-after-submission" {
                assert!(started
                    .expect("unknown submission never allows fallback")
                    .is_err());
            } else {
                let mut out = Vec::new();
                let mut err = Vec::new();
                let stream = started.unwrap().unwrap();
                assert_eq!(
                    watch_io(
                        stream,
                        io::Cursor::new(b"{\"type\":\"subscribe\"}\n".to_vec()),
                        &mut out,
                        &mut err
                    )
                    .unwrap(),
                    0
                );
                assert_eq!(out, b"{\"type\":\"hello\"}\n{\"type\":\"heartbeat\"}\n");
                assert_eq!(err, b"diagnostic");
            }
            listener.close();
            server.join().unwrap();
        }
    }
}

pub(super) fn relay<W: Write + Send + 'static>(
    stream: &mut PipeStream,
    mut request: Value,
    id: u64,
    pending: &Pending,
    in_flight: InFlight,
    to_peer: Arc<Mutex<W>>,
    supported: bool,
) -> io::Result<()> {
    let args = request["args"]
        .as_array()
        .map(|args| {
            args.iter()
                .filter_map(|a| a.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !supported || !multiplex::watch_args(&args) {
        return write_frame(stream, &json!({"unsupported_stream":true}));
    }
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let (sender, events) = mpsc::sync_channel(64);
    pending.lock().unwrap().insert(
        id,
        PendingClient {
            sender: sender.clone(),
            stream: stream.try_clone()?,
        },
    );
    request["id"] = json!(id);
    request["chunks"] = json!(true);
    let cancelled = Arc::new(AtomicBool::new(false));
    let abandoned = cancelled.clone();
    let (inputs, input) = mpsc::sync_channel::<Value>(16);
    let sending = sender.clone();
    thread::spawn(move || {
        let submitted = {
            let mut writer = to_peer.lock().unwrap();
            if abandoned.load(Ordering::SeqCst) {
                return;
            }
            in_flight.lock().unwrap().insert(id);
            write_frame(&mut *writer, &request)
        };
        if submitted.is_err() {
            let _ = sending.try_send(ClientEvent::Answer(
                json!({"error":"the connection to the peer ended"}),
            ));
            return;
        }
        while let Ok(frame) = input.recv() {
            if write_frame(&mut *to_peer.lock().unwrap(), &frame).is_err() {
                break;
            }
        }
        // Closing this watch's input is not a cancellation extension and
        // never cancels an ordinary remote command.
        let _ = write_frame(
            &mut *to_peer.lock().unwrap(),
            &json!({"id":id,"input":null}),
        );
    });
    let mut from_client = stream.try_clone()?;
    let observed = sender.clone();
    thread::spawn(move || {
        while let Ok(Some(mut frame)) = read_frame(&mut from_client) {
            if frame.get("input").is_none() {
                break;
            }
            if !frame["input"].is_null() && multiplex::decode_chunk(&frame["input"]).is_err() {
                break;
            }
            frame["id"] = json!(id);
            if inputs.try_send(frame).is_err() {
                break;
            }
        }
        // Release the local active/pending slot even if the shared writer
        // is blocked. Its independent forwarding thread sends EOF later.
        let _ = observed.try_send(ClientEvent::Closed);
    });
    let result = (|| {
        write_frame(stream, &json!({"stream":true}))?;
        while let Ok(event) = events.recv() {
            match event {
                ClientEvent::Answer(frame) => {
                    let done = multiplex::terminal(&frame);
                    write_frame(stream, &frame)?;
                    if done {
                        return Ok(());
                    }
                }
                ClientEvent::Closed => return Ok(()),
            }
        }
        Err(io::ErrorKind::BrokenPipe.into())
    })();
    cancelled.store(true, Ordering::SeqCst);
    pending.lock().unwrap().remove(&id);
    stream.shutdown();
    result
}
