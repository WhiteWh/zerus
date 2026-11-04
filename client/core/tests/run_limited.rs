//! `run_limited`: the standard output cap of reads. The child is `python3 -c`,
//! which writes exactly the bytes a test asks for.
#![cfg(unix)]

use std::path::Path;
use std::time::{Duration, Instant};
use zerus_client_core::{run_limited, run_with_env, Exit, Invocation, TooLarge};

/// An odd size, so the cap falls inside a read chunk.
const CAP: usize = 64 * 1024 + 3;

fn writes(bytes: usize, then: &str) -> Invocation {
    Invocation {
        args: vec![
            "-c".into(),
            format!(
                "import sys, time\nsys.stderr.write('note')\nsys.stderr.flush()\n\
                 sys.stdout.write('x' * {bytes})\nsys.stdout.flush()\n{then}"
            ),
        ],
        stdin: None,
        timeout: Duration::from_secs(20),
        request_id: None,
    }
}

fn python() -> &'static Path {
    Path::new("python3")
}

#[cfg(target_os = "linux")]
#[test]
fn a_gui_command_restores_the_local_webkit_environment_before_starting_hgs() {
    let marker =
        r#"{"version":1,"LD_LIBRARY_PATH":"2f6f726967696e616c2f6c6962","WEBKIT_EXEC_PATH":null}"#;
    let inv = Invocation {
        args: vec!["-c".into(), "import json, os; print(json.dumps([os.getenv('LD_LIBRARY_PATH'), os.getenv('WEBKIT_EXEC_PATH'), os.getenv('HGS_LINUX_WEBKIT_ORIGINAL_ENV')]))".into()],
        stdin: None,
        timeout: Duration::from_secs(5),
        request_id: None,
    };
    let exit = run_with_env(
        python(),
        &inv,
        &[
            ("LD_LIBRARY_PATH", "/private/gtk:/private/webkit"),
            ("WEBKIT_EXEC_PATH", "/private/processes"),
            (zerus_client_core::child_environment::WEBKIT_MARKER, marker),
        ],
    );
    let Exit::Code {
        code: 0, stdout, ..
    } = exit
    else {
        panic!("{exit:?}")
    };
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&stdout).unwrap(),
        serde_json::json!(["/original/lib", null, null])
    );
}

fn stdout_len(exit: &Exit) -> usize {
    match exit {
        Exit::Code {
            code: 0, stdout, ..
        } => stdout.len(),
        other => panic!("not a clean exit: {other:?}"),
    }
}

#[test]
fn exactly_the_cap_is_whole_and_one_byte_more_is_too_large() {
    let whole = run_limited(python(), &writes(CAP, ""), &[], CAP).expect("within the cap");
    assert_eq!(stdout_len(&whole), CAP);
    let over = run_limited(python(), &writes(CAP + 1, ""), &[], CAP);
    assert_eq!(
        over,
        Err(TooLarge {
            cap: CAP,
            stderr: "note".into()
        })
    );
}

#[test]
fn a_child_that_keeps_writing_is_ended_once_it_passes_the_cap() {
    let started = Instant::now();
    let over = run_limited(python(), &writes(CAP * 4, "time.sleep(30)"), &[], CAP);
    assert!(matches!(over, Err(TooLarge { cap: CAP, .. })), "{over:?}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "ended at once, not at the 20 s timeout: {:?}",
        started.elapsed()
    );
}

#[test]
fn without_a_cap_everything_is_kept_as_before() {
    let big = 3 << 20;
    assert_eq!(
        stdout_len(&run_with_env(python(), &writes(big, ""), &[])),
        big
    );
    let unlimited = run_limited(python(), &writes(big, ""), &[], usize::MAX).unwrap();
    assert_eq!(stdout_len(&unlimited), big);
}
