use super::*;
use std::{collections::BTreeMap, time::Duration};

const OWNER: u32 = 1000;

fn image() -> SystemImage {
    SystemImage {
        device: 1,
        inode: 2,
        uid: 0,
        mode: 0o100555,
        len: 100,
        modified: (1, 2),
        changed: (3, 4),
    }
}
fn node(pid: u32, parent: u32, path: &str) -> Snapshot {
    Snapshot {
        pid,
        parent,
        group: pid,
        session: 6,
        status: 2,
        command: [0; 16],
        flags: 0,
        uids: [OWNER; 3],
        gids: [20; 3],
        unique: Unique {
            executable: [1; 16],
            birth: pid as u64 * 100,
            parent_birth: parent as u64 * 100,
            version: 7,
        },
        image: path.into(),
        system_image: None,
    }
}
fn init() -> Snapshot {
    let mut value = node(1, 0, "/sbin/launchd");
    value.group = 1;
    value.session = 1;
    value.uids = [0; 3];
    value.gids = [0; 3];
    value.system_image = Some(image());
    value
}
fn login() -> Snapshot {
    let mut value = node(6, 5, "/usr/bin/login");
    value.uids = [0, OWNER, 0];
    value.system_image = Some(image());
    value
}
fn terminal() -> BTreeMap<u32, Snapshot> {
    [
        node(9, 8, "/private/example/hgs"),
        node(8, 7, "/usr/bin/python3"),
        node(7, 6, "/bin/zsh"),
        login(),
        node(
            5,
            1,
            "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal",
        ),
        init(),
    ]
    .into_iter()
    .map(|v| (v.pid, v))
    .collect()
}
fn run(values: &BTreeMap<u32, Snapshot>, markers: &[u32]) -> Result<Vec<u32>> {
    walk(
        9,
        OWNER,
        Instant::now() + Duration::from_secs(2),
        |pid| {
            values
                .get(&pid)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("unreadable short info"))
        },
        |v| {
            ensure!(
                !markers.contains(&v.pid) && !super::super::agent_image(&v.image),
                "agent-descended frontend request refused"
            );
            Ok(v.pid)
        },
    )
}

#[test]
fn terminal_login_bridge_continues_and_launchd_parented_gui_uses_the_same_walk() {
    assert_eq!(run(&terminal(), &[]).unwrap(), [9, 8, 7, 5]);
    let dock = [
        node(9, 8, "/private/example/hgs"),
        node(8, 1, "/private/example/hgs-tray"),
        init(),
    ]
    .into_iter()
    .map(|v| (v.pid, v))
    .collect();
    assert_eq!(run(&dock, &[]).unwrap(), [9, 8]);
}

#[test]
fn only_exact_protected_system_login_and_launchd_have_foreign_roles() {
    assert_eq!(classify(&login(), OWNER).unwrap(), Role::Login);
    assert_eq!(classify(&init(), OWNER).unwrap(), Role::Init);
    let changes: &[fn(&mut Snapshot)] = &[
        |v| v.uids[1] = OWNER + 1,
        |v| v.uids[0] = OWNER + 1,
        |v| v.uids[2] = OWNER,
        |v| v.session += 1,
        |v| v.group += 1,
        |v| v.image = "/private/example/login".into(),
        |v| v.image = "/usr/libexec/sshd-session".into(),
        |v| v.system_image = None,
        |v| v.system_image.as_mut().unwrap().uid = OWNER,
        |v| v.system_image.as_mut().unwrap().mode |= 0o020,
        |v| v.system_image.as_mut().unwrap().mode = 0o120777,
        |v| v.status = 5,
        |v| v.unique.birth = 0,
    ];
    for change in changes {
        let mut value = login();
        change(&mut value);
        assert!(classify(&value, OWNER).is_err(), "{value:?}");
    }
    for change in [
        (|v: &mut Snapshot| v.pid = 2) as fn(&mut Snapshot),
        |v| v.parent = 2,
        |v| v.uids[1] = OWNER,
        |v| v.gids[2] = 20,
        |v| v.image = "/private/example/launchd".into(),
        |v| v.unique.parent_birth = 1,
    ] {
        let mut value = init();
        change(&mut value);
        assert!(classify(&value, OWNER).is_err(), "{value:?}");
    }
}

#[test]
fn agent_marker_above_login_and_agent_as_gui_parent_are_both_refused() {
    let values = terminal();
    assert!(run(&values, &[5])
        .unwrap_err()
        .to_string()
        .contains("agent-descended"));
    let mut values = values;
    values.get_mut(&5).unwrap().image = "/private/example/claude".into();
    assert!(run(&values, &[])
        .unwrap_err()
        .to_string()
        .contains("agent-descended"));
    let gui = [
        node(9, 8, "/private/example/hgs"),
        node(8, 7, "/private/example/hgs-tray"),
        node(7, 1, "/private/example/codex"),
        init(),
    ]
    .into_iter()
    .map(|v| (v.pid, v))
    .collect();
    assert!(run(&gui, &[])
        .unwrap_err()
        .to_string()
        .contains("agent-descended"));
}

#[test]
fn unreadable_foreign_and_missing_or_reused_login_edges_never_end_the_walk() {
    let mut values = terminal();
    values.remove(&6);
    assert!(run(&values, &[])
        .unwrap_err()
        .to_string()
        .contains("unreadable"));
    let mut values = terminal();
    values.get_mut(&6).unwrap().image = "/usr/libexec/sshd-session".into();
    values.get_mut(&6).unwrap().uids = [0; 3];
    assert!(run(&values, &[]).is_err());
    let mut values = terminal();
    values.get_mut(&7).unwrap().unique.parent_birth += 1;
    assert!(run(&values, &[])
        .unwrap_err()
        .to_string()
        .contains("child-parent edge"));
    let mut values = terminal();
    values.get_mut(&5).unwrap().unique.birth += 1;
    assert!(run(&values, &[])
        .unwrap_err()
        .to_string()
        .contains("parent identity"));
}

#[test]
fn every_bridge_and_terminal_identity_is_read_again_and_any_change_refuses() {
    for pid in [6, 5, 1] {
        for change in [
            (|v: &mut Snapshot| v.unique.birth += 1) as fn(&mut Snapshot),
            |v| v.unique.version = v.unique.version.wrapping_add(1),
            |v| v.unique.executable[0] ^= 1,
            |v| v.parent = 2,
            |v| v.uids[1] += 1,
            |v| v.gids[2] += 1,
            |v| v.group += 1,
            |v| v.session += 1,
            |v| v.command[0] ^= 1,
            |v| v.flags ^= IDENTITY_FLAGS,
            |v| v.system_image.get_or_insert_with(image).inode += 1,
            |v| v.image.push_str(".changed"),
        ] {
            let values = terminal();
            let mut reads = BTreeMap::<u32, usize>::new();
            let result = walk(
                9,
                OWNER,
                Instant::now() + Duration::from_secs(2),
                |id| {
                    let mut value = values[&id].clone();
                    let count = reads.entry(id).or_default();
                    *count += 1;
                    if id == pid && *count == 2 {
                        change(&mut value);
                    }
                    Ok(value)
                },
                |v| Ok(v.pid),
            );
            assert!(result.unwrap_err().to_string().contains("ancestry changed"));
        }
    }
    let mut reads = BTreeMap::<u32, usize>::new();
    let values = terminal();
    walk(
        9,
        OWNER,
        Instant::now() + Duration::from_secs(2),
        |id| {
            *reads.entry(id).or_default() += 1;
            Ok(values[&id].clone())
        },
        |v| Ok(v.pid),
    )
    .unwrap();
    assert!(reads.values().all(|count| *count == 2));
}

#[test]
fn runtime_flags_and_live_scheduler_state_can_change_without_changing_identity() {
    let values = terminal();
    for pid in values.keys() {
        for bit in 0..32 {
            let flag = 1 << bit;
            if flag & (IDENTITY_FLAGS | TRACED) != 0 {
                continue;
            }
            let mut reads = BTreeMap::<u32, usize>::new();
            let result = walk(
                9,
                OWNER,
                Instant::now() + Duration::from_secs(2),
                |id| {
                    let mut value = values[&id].clone();
                    let count = reads.entry(id).or_default();
                    *count += 1;
                    if id == *pid && *count == 2 {
                        value.flags ^= flag;
                        value.status = 3; // Sleeping instead of runnable.
                    }
                    Ok(value)
                },
                |v| Ok(v.pid),
            );
            assert_eq!(result.unwrap(), [9, 8, 7, 5], "pid {pid}, flag {flag}");
        }
    }
}

#[test]
fn tracing_or_a_zombie_is_an_explicit_refusal_on_the_first_or_final_read() {
    let values = terminal();
    for pid in values.keys() {
        for at in [1, 2] {
            for traced in [true, false] {
                let mut reads = BTreeMap::<u32, usize>::new();
                let result = walk(
                    9,
                    OWNER,
                    Instant::now() + Duration::from_secs(2),
                    |id| {
                        let mut value = values[&id].clone();
                        let count = reads.entry(id).or_default();
                        *count += 1;
                        if id == *pid && *count == at {
                            if traced {
                                value.flags |= TRACED;
                            } else {
                                value.status = 5;
                            }
                        }
                        Ok(value)
                    },
                    |v| Ok(v.pid),
                );
                let error = result.unwrap_err().to_string();
                assert!(error.contains(if traced {
                    "traced macOS frontend ancestor"
                } else {
                    "macOS ancestry identity unavailable"
                }));
            }
        }
    }
}

#[test]
fn missing_init_cycles_expiry_and_long_chains_refuse_without_a_fallback() {
    let mut values = terminal();
    values.remove(&1);
    assert!(run(&values, &[]).is_err());
    let mut values = terminal();
    values.get_mut(&5).unwrap().parent = 9;
    assert!(run(&values, &[]).is_err());
    assert!(walk(
        9,
        OWNER,
        Instant::now(),
        |_| panic!("expired before native read"),
        |_| Ok(())
    )
    .is_err());
    assert!(walk(
        100,
        OWNER,
        Instant::now() + Duration::from_secs(2),
        |pid| Ok(node(pid, pid + 1, "/private/example/own")),
        |_| Ok(())
    )
    .is_err());
}
