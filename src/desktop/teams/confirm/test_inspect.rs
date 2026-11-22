use super::types::Binding;
use crate::{
    config::Config,
    desktop::teams::child::{Ended, FIXTURE_SPAWN},
    state::listing_fixture as producer,
};
use serde_json::{json, Value};
use std::process::{Command, Stdio};

const CHILD: &str = "desktop::teams::confirm::test_inspect::fixture_child";

/// Save the member from the process whose birth is recorded, then reap it
/// without running supervisor completion or creating an exit archive.
pub(super) fn save_then_exit(config: &Config, b: &Binding, name: &str) -> Value {
    let mut command = fixture(config, json!({"binding": b, "name": name}));
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    checked(child.wait_with_output().unwrap());
    let record = producer::saved_record(name).unwrap();
    assert_eq!(record["pid"], pid);
    assert!(!record["process_start"].as_str().unwrap().is_empty());
    record
}

pub(super) fn use_cli_inspection() {
    FIXTURE_SPAWN.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(|config, args, stdin| {
            assert!(stdin.is_none());
            assert_eq!(args[0], "--expect-machine");
            assert_eq!(args[2], "inspect");
            cli_inspection(config, args)
        }));
    });
}

fn cli_inspection(config: &Config, args: &[String]) -> Ended {
    let output = checked(fixture(config, json!({"inspect": args})).output().unwrap());
    let (_, reply) = output.split_once("CLI output begins\n").unwrap();
    let (stdout, code) = reply.split_once("CLI exit code: ").unwrap();
    let code = code.lines().next().unwrap().parse().unwrap();
    Ended::Exited {
        code,
        output: stdout.as_bytes().to_vec(),
    }
}

fn fixture(config: &Config, input: Value) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    let tmux = config.state.join("fixture-tmux");
    crate::platform::private_dir(&tmux).unwrap();
    command
        .args([
            "--exact",
            CHILD,
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("TEST_TEAMS_INSPECTION", input.to_string())
        .env("HGS_STATE_DIR", &config.state)
        .env("HGS_CONFIG_DIR", &config.dir)
        .env("TMUX_TMPDIR", tmux)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("HGS_SESSION")
        .env_remove("HGS_SESSION_ID")
        .env_remove("HGS_RUN_ID")
        .stdin(Stdio::null());
    command
}

fn checked(output: std::process::Output) -> String {
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success() && text.contains("1 passed"),
        "{text}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    text
}

#[test]
#[ignore = "run by the private inspection fixtures"]
fn fixture_child() {
    let input: Value =
        serde_json::from_str(&std::env::var("TEST_TEAMS_INSPECTION").unwrap()).unwrap();
    if let Some(binding) = input.get("binding") {
        let b: Binding = serde_json::from_value(binding.clone()).unwrap();
        let mut record = producer::launched_record(
            &b.agent,
            input["name"].as_str().unwrap(),
            &b.launch,
            &b.session,
            &b.run,
            &b.folder,
            &b.account,
        );
        record["worktree_owner"] = json!({"id": b.nonce});
        if !b.conversation.is_empty() {
            producer::confirmed(&mut record, &b.conversation);
        }
        producer::save(&mut record).unwrap();
    } else {
        let argv = serde_json::from_value(input["inspect"].clone()).unwrap();
        println!("CLI output begins");
        let code = match crate::cli::dispatch_at(argv, std::time::Instant::now()) {
            Ok(code) => code,
            Err(error) => {
                eprintln!("{}", error.message);
                error.code
            }
        };
        println!("CLI exit code: {code}");
    }
}
