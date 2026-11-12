//! Closed Send helper forms. No renderer/frame can select argv or a key.
use super::*;
use std::{
    process::{Command, Stdio},
    sync::atomic::Ordering,
};
use zerus_client_core::run::writer_custody::Admission;

struct StageCharge {
    _owner: Arc<owner::Charge>,
    settled: Arc<send::Progress>,
}
impl Drop for StageCharge {
    fn drop(&mut self) {
        self.settled.stage_done.store(true, Ordering::Release);
    }
}

fn absent(error: &str, buffer: &str) -> bool {
    let error = error.trim_end_matches(['\r', '\n']).to_ascii_lowercase();
    if error.contains(['\r', '\n']) {
        return false;
    }
    error == format!("no buffer {buffer}")
        || error == format!("unknown buffer: {buffer}")
        || error == "no buffer"
        || error
            .strip_prefix("no server running on ")
            .is_some_and(|path| !path.is_empty())
        || error == "failed to connect to server: no such file or directory"
        || error
            .strip_prefix("error connecting to ")
            .and_then(|tail| tail.strip_suffix(" (no such file or directory)"))
            .is_some_and(|path| !path.is_empty())
}

pub(super) struct Cleanup {
    owner: Arc<owner::Owner>,
    progress: Arc<send::Progress>,
    admission: Option<Admission>,
    until: Instant,
}
impl Cleanup {
    pub(super) fn reserve(
        owner: &Arc<owner::Owner>,
        progress: Arc<send::Progress>,
    ) -> Result<Self> {
        let until = owner
            .until
            .checked_add(Duration::from_secs(2))
            .ok_or("Invalid Send cleanup deadline")?;
        let admission = Admission::reserve(owner.charge(), until)
            .map_err(|e| format!("Send cleanup custody refused: {e:?}"))?;
        Ok(Self {
            owner: owner.clone(),
            progress,
            admission: Some(admission),
            until,
        })
    }
    pub(super) fn finish(mut self) -> Result<()> {
        self.discard()
    }
    fn discard(&mut self) -> Result<()> {
        let Some(admission) = self.admission.take() else {
            return Ok(());
        };
        if !self.progress.stage_started.load(Ordering::Acquire)
            || self.progress.paste_success.load(Ordering::Acquire)
        {
            return Ok(());
        }
        while !self.progress.stage_done.load(Ordering::Acquire) {
            if Instant::now() >= self.until {
                return Err("buffer cleanup uncertain: Stage has not physically settled".into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let buffer = format!("hgs-input-{}", self.owner.control.frame().request_id);
        let mut command = Command::new("tmux");
        command
            .args(["delete-buffer", "-b", &buffer])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = {
            let _turn = crate::platform::spawn_turn();
            if Instant::now() >= self.until {
                return Err("buffer cleanup uncertain: cleanup lease expired".into());
            }
            // This is the sole lease bypass: same keeper and inherited server,
            // exact parent UUID, no actor/key/native/record/analytics write.
            command
                .spawn()
                .map_err(|e| format!("buffer cleanup uncertain: {e}"))?
        };
        let mut exchange = admission.attach(child);
        let _ =
            exchange.close_empty_input(|_, until| until == self.until && Instant::now() < until);
        loop {
            if let Some(report) = exchange.poll() {
                if report.failure.is_some() || !report.input_written {
                    return Err("buffer cleanup uncertain: helper failed".into());
                }
                let error = String::from_utf8_lossy(&report.stderr);
                return if report.exit.success() || absent(&error, &buffer) {
                    Ok(())
                } else {
                    Err("buffer cleanup uncertain: delete-buffer was not confirmed".into())
                };
            }
            if Instant::now() >= self.until {
                exchange.cancel();
                return Err("buffer cleanup uncertain: original helper has not settled".into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = self.discard();
    }
}

enum Form {
    Stage,
    Paste,
    Enter,
}

fn invoke(owner: &Arc<owner::Owner>, form: Form, progress: Arc<send::Progress>) -> Result<()> {
    let phase = match form {
        Form::Stage => send::Phase::Ready,
        Form::Paste => send::Phase::Marked,
        Form::Enter => send::Phase::Pasted,
    };
    send::parent_ready(owner, phase)?;
    match form {
        Form::Stage => {
            context::take_kind(owner, context::Kind::SendReady)?;
        }
        Form::Enter => {
            context::take_kind(owner, context::Kind::SendAfterPaste)?;
        }
        Form::Paste => {} // Exact own marker commit carries the immediately prior SendReady.
    }
    let bytes = if matches!(form, Form::Stage) {
        Some(send::pasted(owner)?)
    } else {
        None
    };
    let admission = if matches!(form, Form::Stage) {
        Admission::reserve(
            Arc::new(StageCharge {
                _owner: owner.charge(),
                settled: progress.clone(),
            }),
            owner.until,
        )
    } else {
        Admission::reserve(owner.charge(), owner.until)
    }
    .map_err(|e| format!("Original Send helper custody refused: {e:?}"))?;
    let record = owner.snapshot()?.0;
    let buffer = format!("hgs-input-{}", owner.control.frame().request_id);
    let args: Vec<String> = match form {
        Form::Stage => vec!["load-buffer".into(), "-b".into(), buffer, "-".into()],
        Form::Paste => [
            "paste-buffer",
            "-p",
            "-r",
            "-d",
            "-b",
            &buffer,
            "-t",
            string(&record, "pane"),
        ]
        .map(str::to_owned)
        .to_vec(),
        Form::Enter => ["send-keys", "-t", string(&record, "pane"), "Enter"]
            .map(str::to_owned)
            .to_vec(),
    };
    let mut command = Command::new("tmux");
    command
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = {
        let _turn = crate::platform::spawn_turn();
        send::parent_ready(owner, phase)?;
        owner.control.commit_boundary()?;
        match form {
            Form::Paste => progress.paste_possible.store(true, Ordering::Release),
            Form::Stage | Form::Enter => {}
        }
        let child = command.spawn().map_err(|e| e.to_string())?;
        if matches!(form, Form::Stage) {
            progress.stage_started.store(true, Ordering::Release);
        }
        child
    };
    let mut exchange = admission.attach(child);
    let check = |_: &mut std::process::Child, until| {
        until == owner.until && send::parent_ready(owner, phase).is_ok()
    };
    if let Some(bytes) = bytes {
        let _ = exchange.send(bytes, check);
    } else {
        let _ = exchange.close_empty_input(check);
    }
    loop {
        if let Some(report) = exchange.poll() {
            // Record the physical -d outcome even when the subsequent source
            // fence expires. Expiry cannot authorize another buffer deletion.
            if matches!(form, Form::Paste) && report.exit.success() {
                progress.paste_success.store(true, Ordering::Release);
            }
            owner.check()?;
            if report.failure.is_some() || !report.input_written || !report.exit.success() {
                return Err(format!(
                    "Original Send helper failed after settlement: {:?}: {}",
                    report.failure,
                    String::from_utf8_lossy(&report.stderr).trim()
                ));
            }
            match form {
                Form::Stage => send::advance(owner, send::Phase::Ready, send::Phase::Staged)?,
                Form::Paste => {
                    progress.paste_success.store(true, Ordering::Release);
                    send::advance(owner, send::Phase::Marked, send::Phase::Pasted)?;
                }
                Form::Enter => {
                    progress.enter_settled.store(true, Ordering::Release);
                    send::advance(owner, send::Phase::Pasted, send::Phase::Entered)?;
                }
            }
            return Ok(());
        }
        if let Err(error) = owner.actor().and_then(|()| owner.check()) {
            exchange.cancel();
            return Err(error);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
pub(super) fn stage(owner: &Arc<owner::Owner>, progress: Arc<send::Progress>) -> Result<()> {
    invoke(owner, Form::Stage, progress)
}
pub(super) fn paste(owner: &Arc<owner::Owner>, progress: Arc<send::Progress>) -> Result<()> {
    invoke(owner, Form::Paste, progress)
}
pub(super) fn enter(owner: &Arc<owner::Owner>, progress: Arc<send::Progress>) -> Result<()> {
    invoke(owner, Form::Enter, progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_absence_never_hides_an_unrecognized_failure() {
        let buffer = "hgs-input-00000000-0000-0000-0000-000000000000";
        for error in [
            format!("no buffer {buffer}\n"),
            format!("unknown buffer: {buffer}\n"),
            "no buffer\n".into(),
            "no server running on /tmp/example/socket\n".into(),
            "error connecting to /tmp/example/socket (No such file or directory)\n".into(),
            "failed to connect to server: No such file or directory\n".into(),
        ] {
            assert!(absent(&error, buffer), "{error}");
        }
        for error in [
            "no buffer other",
            "unknown buffer: other",
            "unknown buffer",
            "warning: unknown buffer: hgs-input-00000000-0000-0000-0000-000000000000",
            "unknown buffer: hgs-input-00000000-0000-0000-0000-000000000000\npermission denied",
            "permission denied",
            "failed to connect to server: Permission denied",
            "error connecting to /tmp/example/socket (Permission denied)",
            "warning: no buffer",
            "no buffer\npermission denied",
        ] {
            assert!(!absent(error, buffer), "{error}");
        }
    }
}
