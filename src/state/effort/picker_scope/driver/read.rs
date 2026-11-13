//! Only fixed existing backend call sites select these forms. The frame has
//! no program, argv, callback or form selector. Each returned Child is attached
//! before any later source refusal, including an argv-capable helper's error.
use super::*;
use std::process::{Command, Output, Stdio};
use zerus_client_core::run::writer_custody::Admission;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::state) enum Call {
    Panes,
    PaneInfo,
    Probe,
    CapturePlain,
    CaptureStyled,
    SessionName,
    ProcessTty,
    ProcessStart,
    Keys,
    Unsupported,
}

fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

fn form(owner: &owner::Owner, call: Call, args: &[String]) -> Result<&'static str> {
    let native = owner.native()?;
    let binding = if native {
        owner.snapshot()?.0
    } else {
        owner.control.frame().binding.clone()
    };
    let target = string(&binding, "pane");
    let original = terminal_name(&binding);
    let pane = |text: &str| text == target;
    let info = || matches!(args,[verb,p,t,selected,_] if verb=="display-message"&&p=="-p"&&t=="-t"&&(pane(selected)||selected==&format!("={original}:")));
    let valid = match call {
        Call::Panes => {
            native
                && args
                    == [
                        "list-panes",
                        "-a",
                        "-F",
                        "#{session_name}\t#{session_id}\t#{pane_id}\t#{pane_dead}\t#{@hgs_run}",
                    ]
        }
        Call::PaneInfo | Call::Probe => native && info(),
        Call::CapturePlain => {
            native
                && matches!(args,[verb,p,t,selected] if verb=="capture-pane"&&p=="-p"&&t=="-t"&&pane(selected))
        }
        Call::CaptureStyled => {
            native
                && matches!(args,[verb,p,e,t,selected] if verb=="capture-pane"&&p=="-p"&&e=="-e"&&t=="-t"&&pane(selected))
        }
        Call::SessionName => {
            !native
                && matches!(args,[verb,p,t,id,format] if verb=="display-message"&&p=="-p"&&t=="-t"&&id.strip_prefix('$').is_some_and(digits)&&format=="#{session_name}")
        }
        Call::ProcessTty => {
            native
                && matches!(args,[p,pid,o,columns] if p=="-p"&&pid==&binding["pid"].to_string()&&o=="-o"&&matches!(columns.as_str(),"tty="|"tty=,pgid=,tpgid=,comm="))
        }
        Call::ProcessStart => {
            matches!(args,[p,pid,o,columns] if p=="-p"&&digits(pid)&&pid.parse::<u32>().is_ok_and(|v|v>0)&&o=="-o"&&columns=="lstart=")
        }
        Call::Keys => {
            native
                && matches!(args,[verb,t,selected,key]
            if verb=="send-keys" && t=="-t" && pane(selected)
            && matches!(key.as_str(),"M-."|"M-,"|"Enter"|"Right"|"Left"|"M-s"|"Down"|"Up"|"Tab"|"s"))
                || native
                    && matches!(args,[verb,t,selected,literal,text]
                if verb=="send-keys" && t=="-t" && pane(selected) && literal=="-l"
                && matches!(text.as_str(),"/effort"|"/model"))
        }
        Call::Unsupported => false,
    };
    if !valid {
        return Err("Helper form is not part of this original Effort phase/target".into());
    }
    Ok(if matches!(call, Call::ProcessTty | Call::ProcessStart) {
        "ps"
    } else {
        "tmux"
    })
}

pub(super) fn route(call: Call, args: &[String]) -> Option<Result<Output>> {
    let held = match context::owner() {
        Ok(None) => return None,
        Ok(Some(owner)) => owner,
        Err(error) => return Some(Err(error)),
    };
    Some(invoke(&held, call, args))
}

pub(super) fn invoke(owner: &Arc<owner::Owner>, call: Call, args: &[String]) -> Result<Output> {
    owner.actor()?;
    let program = match form(owner, call, args) {
        Ok(program) => program,
        Err(error) => return owner.refuse(error),
    };
    invoke_admitted(owner, program, args, call == Call::Keys, call == Call::ProcessStart)
}

/// A fixed cancellation, separate from the picker key allow-list. Only the
/// original request and a freshly recognized Claude model panel reach it.
pub(super) fn close_claude_model(
    owner: &Arc<owner::Owner>,
    name: &str,
    request: &Request,
) -> Result<()> {
    owner.actor()?;
    let frame = owner.control.frame();
    if frame.operation == host::frame::Operation::Send {
        if let Err(error) = send::settings_key_ready(owner) {
            return owner.refuse(error);
        }
    }
    if name != string(&frame.binding, "name")
        || !uuid::Uuid::parse_str(&request.request_id)
            .is_ok_and(|id| id.to_string() == frame.request_id)
    {
        return owner.refuse("Model cancellation does not belong to the original request".into());
    }
    if string(&owner.snapshot()?.0, "agent") != "claude" {
        return owner.refuse("Model cancellation requires the original Claude session".into());
    }
    let record = super::checked(name, request, false)?;
    if string(&record, "agent") != "claude" {
        return owner.refuse("Model cancellation requires the original Claude session".into());
    }
    if claude_model_panel(&capture(&record)?).is_none() {
        return Ok(());
    }
    let args = ["send-keys", "-t", string(&record, "pane"), "Escape"].map(str::to_owned);
    invoke_admitted(owner, "tmux", &args, true, false).map(drop)
}

fn invoke_admitted(
    owner: &Arc<owner::Owner>,
    program: &'static str,
    args: &[String],
    keys: bool,
    process_start: bool,
) -> Result<Output> {
    if let Err(error) = owner.check() {
        return owner.refuse(error);
    }
    if keys {
        if let Err(error) =
            transaction::key_ready(owner).and_then(|()| context::take_turn(owner).map(drop))
        {
            return owner.refuse(error);
        }
    }
    let admission = match Admission::reserve(owner.charge(), owner.until) {
        Ok(admission) => admission,
        Err(failure) => {
            return owner.refuse(format!(
                "Original Effort helper custody refused: {failure:?}"
            ))
        }
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if process_start {
        command.env("LC_ALL", "C").env("TZ", "UTC");
    }
    let child = {
        // The frontend is a different process and retains its original finite
        // budget even while this owner waits in std/spawn or a native syscall.
        // Reserve before this turn; no resource or child is recreated later.
        let _turn = crate::platform::spawn_turn();
        owner.actor()?;
        if let Err(error) = owner.check() {
            return owner.refuse(error);
        }
        if keys {
            owner.with_scope(|scope| {
                scope.may_have_sent = true;
                Ok(())
            })?;
            // Mark BEFORE the potentially effectful spawn: argv itself may
            // deliver the key, regardless of later empty-stdin settlement.
            owner.control.commit_boundary()?;
        }
        match command.spawn() {
            Ok(child) => child,
            Err(error) => return owner.refuse(error.to_string()),
        }
    };
    let mut exchange = admission.attach(child);
    let _ = exchange.close_empty_input(|_, until| until == owner.until && owner.check().is_ok());
    loop {
        if let Some(report) = exchange.poll() {
            if let Err(error) = owner.check() {
                return owner.refuse(error);
            }
            if report.failure.is_some() || !report.input_written {
                return owner.refuse(format!(
                    "Original Effort helper failed after settlement: {:?}",
                    report.failure
                ));
            }
            return Ok(Output {
                status: report.exit,
                stdout: report.stdout,
                stderr: report.stderr,
            });
        }
        if let Err(error) = owner.actor().and_then(|()| owner.check()) {
            exchange.cancel();
            // Drop retires the SAME exchange and charged original owner. The
            // body's error returns no physical-settlement or retry authority.
            return owner.refuse(error);
        }
        std::thread::sleep(
            Duration::from_millis(2).min(owner.until.saturating_duration_since(Instant::now())),
        );
    }
}
