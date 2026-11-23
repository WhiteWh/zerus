//! Protocol 2 framing. Each producer has a bounded queue; the writer takes
//! one frame per ready ID before returning to an ID with more output.
use super::*;
use std::collections::VecDeque;

pub(super) const CHUNK: usize = 64 * 1024;
const QUEUED: usize = 2;

pub(super) fn streams(hello: &Value) -> bool {
    hello["hello"] == 1 && hello["protocol"] == 2 && hello["streams"] == true
}

pub(super) fn terminal(frame: &Value) -> bool {
    frame.get("code").is_some() || frame.get("error").is_some()
}

pub(super) fn watch_args(args: &[String]) -> bool {
    let args = if args.first().is_some_and(|arg| arg == "--expect-machine") {
        if args.get(1).is_none_or(|id| id.is_empty()) {
            return false;
        }
        &args[2..]
    } else {
        args
    };
    args.first().is_some_and(|arg| arg == "watch") && args.iter().any(|arg| arg == "--json")
}

#[derive(Default)]
struct State {
    queues: HashMap<u64, VecDeque<Value>>,
    ready: VecDeque<u64>,
    closed: bool,
}

#[derive(Clone, Default)]
pub(super) struct Output(Arc<(Mutex<State>, Condvar)>);

impl Output {
    pub(super) fn put(&self, id: u64, frame: Value) -> io::Result<()> {
        let (state, changed) = &*self.0;
        let mut state = state.lock().unwrap();
        while !state.closed && state.queues.get(&id).is_some_and(|q| q.len() >= QUEUED) {
            state = changed.wait(state).unwrap();
        }
        if state.closed {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let queue = state.queues.entry(id).or_default();
        let was_empty = queue.is_empty();
        queue.push_back(frame);
        if was_empty {
            state.ready.push_back(id);
        }
        changed.notify_all();
        Ok(())
    }

    fn next(&self) -> Option<Value> {
        let (state, changed) = &*self.0;
        let mut state = state.lock().unwrap();
        while state.ready.is_empty() && !state.closed {
            state = changed.wait(state).unwrap();
        }
        let id = state.ready.pop_front()?;
        let queue = state.queues.get_mut(&id).unwrap();
        let frame = queue.pop_front();
        if queue.is_empty() {
            state.queues.remove(&id);
        } else {
            state.ready.push_back(id);
        }
        changed.notify_all();
        frame
    }

    pub(super) fn close(&self) {
        let (state, changed) = &*self.0;
        state.lock().unwrap().closed = true;
        changed.notify_all();
    }

    fn writer(&self, mut writer: impl Write) -> io::Result<()> {
        while let Some(frame) = self.next() {
            if let Err(error) = write_frame(&mut writer, &frame) {
                self.close();
                return Err(error);
            }
        }
        Ok(())
    }
}

/// Ordinary local clients still receive the original single answer. The
/// reassembly is bounded independently of the peer's frame declarations.
#[derive(Default)]
pub(super) struct Answer {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Answer {
    pub(super) fn frame(&mut self, frame: Value) -> io::Result<Option<Value>> {
        if frame.get("chunk").is_some() {
            let data = decode_chunk(&frame["chunk"])?;
            if self.stdout.len() + self.stderr.len() + data.len() > (MAX_FRAME - 1024) / 4 * 3 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "peer answer is too large",
                ));
            }
            match frame["channel"].as_str() {
                Some("stdout") => self.stdout.extend(data),
                Some("stderr") => self.stderr.extend(data),
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unknown peer output channel",
                    ))
                }
            }
            Ok(None)
        } else if terminal(&frame) {
            if frame.get("stdout").is_some() || frame.get("error").is_some() {
                return Ok(Some(frame));
            }
            Ok(Some(json!({"id":frame["id"],"code":frame["code"],
                "stdout":STANDARD.encode(&self.stdout),"stderr":STANDARD.encode(&self.stderr)})))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid peer answer",
            ))
        }
    }
}

pub(super) fn decode_chunk(value: &Value) -> io::Result<Vec<u8>> {
    let text = value
        .as_str()
        .filter(|s| s.len() <= CHUNK.div_ceil(3) * 4)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid peer chunk"))?;
    let decoded = STANDARD
        .decode(text)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid peer chunk"))?;
    if decoded.len() > CHUNK {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(decoded)
}

type Inputs = Arc<Mutex<HashMap<u64, mpsc::SyncSender<Option<Vec<u8>>>>>>;

pub(super) fn serve(program: PathBuf) -> io::Result<()> {
    serve_io(program, io::stdin(), io::stdout())
}

/// Kept independent of global stdin/stdout for isolated broker/serve tests.
fn serve_io(
    program: PathBuf,
    input: impl Read + Send + 'static,
    mut writer: impl Write + Send + 'static,
) -> io::Result<()> {
    write_frame(
        &mut writer,
        &json!({"hello":1,"protocol":2,"streams":true,"version":env!("CARGO_PKG_VERSION")}),
    )?;
    let output = Output::default();
    let writing = output.clone();
    let written = thread::spawn(move || writing.writer(writer));
    let inputs = Inputs::default();
    let running = Arc::new(AtomicUsize::new(0));
    let streaming = Arc::new(AtomicUsize::new(0));
    let (work, jobs) = mpsc::sync_channel::<Value>(PARALLEL);
    let jobs = Arc::new(Mutex::new(jobs));
    for _ in 0..PARALLEL {
        let (jobs, output, program, running) = (
            jobs.clone(),
            output.clone(),
            program.clone(),
            running.clone(),
        );
        thread::spawn(move || loop {
            let Ok(request) = jobs.lock().unwrap().recv() else {
                break;
            };
            let id = request["id"].as_u64().unwrap_or(0);
            if request["chunks"] == true {
                execute(&program, request, &output, None);
            } else {
                let _ = output.put(id, run(&program, &request));
            }
            running.fetch_sub(1, Ordering::SeqCst);
        });
    }
    let (received, frames) = mpsc::sync_channel(2);
    thread::spawn(move || {
        let mut input = input;
        loop {
            let frame = read_frame(&mut input);
            let ended = !matches!(&frame, Ok(Some(_)));
            if received.send(frame).is_err() || ended {
                break;
            }
        }
    });
    let executable = crate::swarm::Executable::current();
    let mut replacement = None;
    let mut check = Instant::now() + Duration::from_secs(5);
    let mut result = Ok(());
    loop {
        if Instant::now() >= check {
            let seen = executable
                .as_ref()
                .and_then(crate::swarm::Executable::replacement);
            if seen.is_some() && seen == replacement {
                break;
            }
            replacement = seen;
            check = Instant::now() + Duration::from_secs(5);
        }
        let request = match frames.recv_timeout(Duration::from_secs(1)) {
            Ok(Ok(Some(frame))) => frame,
            Ok(Ok(None)) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Err(error)) => {
                result = Err(error);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
        };
        let id = request["id"].as_u64().unwrap_or(0);
        if let Some(input) = request.get("input") {
            let mut table = inputs.lock().unwrap();
            let data = if input.is_null() {
                Ok(None)
            } else {
                decode_chunk(input).map(Some)
            };
            if let Some(sender) = table.get(&id) {
                if data.is_err()
                    || sender.try_send(data.unwrap_or(None)).is_err()
                    || input.is_null()
                {
                    // Dropping this sender closes only this watch's stdin.
                    table.remove(&id);
                }
            }
            continue;
        }
        if request["stream"] == true {
            let args = arguments(&request);
            let mut table = inputs.lock().unwrap();
            if !watch_args(&args)
                || streaming.load(Ordering::SeqCst) >= PARALLEL
                || table.contains_key(&id)
            {
                let _ = output.put(
                    id,
                    json!({"id":id,"error":"this watch stream cannot run on the peer"}),
                );
                continue;
            }
            let (send, input) = mpsc::sync_channel(16);
            table.insert(id, send);
            let (output, inputs, program, running) = (
                output.clone(),
                inputs.clone(),
                program.clone(),
                running.clone(),
            );
            let streaming = streaming.clone();
            running.fetch_add(1, Ordering::SeqCst);
            streaming.fetch_add(1, Ordering::SeqCst);
            thread::spawn(move || {
                execute(&program, request, &output, Some(input));
                inputs.lock().unwrap().remove(&id);
                streaming.fetch_sub(1, Ordering::SeqCst);
                running.fetch_sub(1, Ordering::SeqCst);
            });
        } else {
            running.fetch_add(1, Ordering::SeqCst);
            if work.try_send(request).is_err() {
                running.fetch_sub(1, Ordering::SeqCst);
                let _ = output.put(
                    id,
                    json!({"id":id,"error":"the peer request queue is full"}),
                );
            }
        }
    }
    drop(work);
    inputs.lock().unwrap().clear();
    // Accepted ordinary commands settle; an input EOF is never cancellation.
    while running.load(Ordering::SeqCst) != 0 {
        thread::sleep(Duration::from_millis(10));
    }
    output.close();
    let write_result = written
        .join()
        .map_err(|_| io::Error::other("peer output writer failed"))?;
    result.and(write_result)
}

fn arguments(request: &Value) -> Vec<String> {
    request["args"]
        .as_array()
        .map(|args| {
            args.iter()
                .filter_map(|arg| arg.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn execute(
    program: &PathBuf,
    request: Value,
    output: &Output,
    input: Option<mpsc::Receiver<Option<Vec<u8>>>>,
) {
    let id = request["id"].as_u64().unwrap_or(0);
    let args = arguments(&request);
    let fail = |error: String| {
        let _ = output.put(id, json!({"id":id,"error":error}));
    };
    if args.is_empty() || args[0].starts_with("__") {
        fail("this request cannot run on the peer".into());
        return;
    }
    let initial = request["stdin"].as_str().map(|_| bytes(&request["stdin"]));
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(if input.is_some() || initial.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            fail(format!("cannot start hgs: {error}"));
            return;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        thread::spawn(move || {
            if let Some(initial) = initial {
                let _ = stdin.write_all(&initial);
            }
            if let Some(input) = input {
                while let Ok(Some(data)) = input.recv() {
                    if stdin.write_all(&data).and_then(|_| stdin.flush()).is_err() {
                        break;
                    }
                }
            }
        });
    }
    let pipe = |mut reader: Box<dyn Read + Send>, channel: &'static str| {
        let output = output.clone();
        thread::spawn(move || -> io::Result<()> {
            let mut buffer = vec![0; CHUNK];
            loop {
                let size = reader.read(&mut buffer)?;
                if size == 0 {
                    return Ok(());
                }
                output.put(
                    id,
                    json!({"id":id,"channel":channel,"chunk":STANDARD.encode(&buffer[..size])}),
                )?;
            }
        })
    };
    let stdout = pipe(Box::new(child.stdout.take().unwrap()), "stdout");
    let stderr = pipe(Box::new(child.stderr.take().unwrap()), "stderr");
    let status = child.wait();
    let out = stdout
        .join()
        .unwrap_or_else(|_| Err(io::Error::other("stdout reader failed")));
    let err = stderr
        .join()
        .unwrap_or_else(|_| Err(io::Error::other("stderr reader failed")));
    match status.and(out.and(err)) {
        Ok(()) => {
            let code = child
                .try_wait()
                .ok()
                .flatten()
                .and_then(|s| s.code())
                .unwrap_or(1);
            let _ = output.put(id, json!({"id":id,"code":code}));
        }
        Err(error) => fail(format!("hgs did not finish: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_ids_interleave_and_preserve_each_producers_order() {
        let output = Output::default();
        for (id, n) in [(1, 0), (1, 1), (2, 0), (2, 1), (3, 0)] {
            output.put(id, json!({"id":id,"n":n})).unwrap();
        }
        let ids: Vec<_> = (0..5).map(|_| output.next().unwrap()).collect();
        assert_eq!(
            ids,
            vec![
                json!({"id":1,"n":0}),
                json!({"id":2,"n":0}),
                json!({"id":3,"n":0}),
                json!({"id":1,"n":1}),
                json!({"id":2,"n":1})
            ]
        );
    }

    #[test]
    fn a_full_producer_queue_does_not_block_another_stream() {
        let output = Output::default();
        output.put(1, json!({"id":1,"n":0})).unwrap();
        output.put(1, json!({"id":1,"n":1})).unwrap();
        let producer = output.clone();
        let (sending, sent) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = producer.put(1, json!({"id":1,"n":2}));
            sending.send(result).unwrap();
        });
        assert!(sent.recv_timeout(Duration::from_millis(30)).is_err());
        output.put(2, json!({"id":2,"n":0})).unwrap();
        assert_eq!(output.next().unwrap()["id"], 1);
        sent.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
        assert_eq!(output.next().unwrap()["id"], 2);
        assert_eq!(output.next().unwrap()["n"], 1);
        assert_eq!(output.next().unwrap()["n"], 2);
        worker.join().unwrap();
    }

    #[test]
    fn negotiation_requires_both_protocol_and_streams_and_watch_only() {
        assert!(streams(&json!({"hello":1,"protocol":2,"streams":true})));
        for hello in [
            json!({"hello":1}),
            json!({"hello":1,"protocol":2}),
            json!({"hello":1,"streams":true}),
        ] {
            assert!(!streams(&hello));
        }
        for (args, expected) in [
            (vec!["watch", "--json"], true),
            (vec!["--expect-machine", "example", "watch", "--json"], true),
            (vec!["watch"], false),
            (vec!["send", "--json"], false),
            (vec!["--expect-machine"], false),
        ] {
            assert_eq!(
                watch_args(&args.into_iter().map(String::from).collect::<Vec<_>>()),
                expected
            );
        }
    }

    #[test]
    fn interleaved_wire_frames_reassemble_requests_without_touching_watch_bytes() {
        let mut wire = Vec::new();
        let payload = vec![b'x'; CHUNK];
        for frame in [
            json!({"id":1,"channel":"stdout","chunk":STANDARD.encode(&payload)}),
            json!({"id":2,"channel":"stdout","chunk":STANDARD.encode(b"{\"type\":\"hello\"}\n")}),
            json!({"id":1,"channel":"stderr","chunk":STANDARD.encode(b"diagnostic")}),
            json!({"id":3,"code":0,"stdout":STANDARD.encode(b"legacy"),"stderr":""}),
            json!({"id":2,"code":0}),
            json!({"id":1,"code":7}),
        ] {
            write_frame(&mut wire, &frame).unwrap();
        }
        let mut reader = wire.as_slice();
        let mut answers: HashMap<u64, Answer> = HashMap::new();
        let mut completed = HashMap::new();
        while let Some(frame) = read_frame(&mut reader).unwrap() {
            let id = frame["id"].as_u64().unwrap();
            if let Some(done) = answers.entry(id).or_default().frame(frame).unwrap() {
                completed.insert(id, done);
            }
        }
        assert_eq!(bytes(&completed[&1]["stdout"]), payload);
        assert_eq!(bytes(&completed[&1]["stderr"]), b"diagnostic");
        assert_eq!(completed[&1]["code"], 7);
        assert_eq!(bytes(&completed[&2]["stdout"]), b"{\"type\":\"hello\"}\n");
        assert_eq!(bytes(&completed[&3]["stdout"]), b"legacy");
        assert!(decode_chunk(&json!(STANDARD.encode(vec![0; CHUNK + 1]))).is_err());
    }
}
