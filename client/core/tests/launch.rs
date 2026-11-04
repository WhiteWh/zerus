//! New session's rules, against `tray/src/NewSessionDialog.cpp`,
//! `HgsClient::launchDetachedSession`, `LocalSessionHost.h` and
//! `LocalLaunchWarning.h`.
use serde_json::{json, Value};
use std::time::Duration;
use zerus_client_core::actions::Exit;
use zerus_client_core::launch::{self, *};
use zerus_client_core::machine::{LocalHost, MachineMeta};
use zerus_client_core::paths::Style;
use zerus_client_core::ComputerId;

fn launch_of(agent: &str) -> Launch {
    Launch {
        agent: agent.into(),
        target: "/srv/repo".into(),
        name: "fix".into(),
        account: String::new(),
        launch_id: "L-1".into(),
        model: String::new(),
        effort: String::new(),
    }
}

#[test]
fn launch_arguments_follow_launch_detached_session() {
    let local = ComputerId::local();
    let call = launch_invocation(&local, &launch_of("codex"), "");
    assert_eq!(
        call.args,
        [
            "codex",
            "/srv/repo",
            "--new",
            "-n",
            "fix",
            "--launch-id",
            "L-1",
            "-d"
        ]
    );
    assert_eq!(call.timeout, Duration::from_secs(90));

    // DeepSeek and OpenCode do not start detached.
    let dsh = launch_invocation(&local, &launch_of("dsh"), "");
    assert!(!dsh.args.contains(&"-d".to_owned()));

    // OpenCode takes a model and its effort; effort needs the model.
    let mut opencode = launch_of("opencode");
    opencode.account = "acct".into();
    opencode.model = "m/x".into();
    opencode.effort = "high".into();
    assert_eq!(
        launch_invocation(&local, &opencode, "").args,
        [
            "opencode",
            "/srv/repo",
            "--new",
            "-n",
            "fix",
            "--launch-id",
            "L-1",
            "--account",
            "acct",
            "--model",
            "m/x",
            "--effort",
            "high"
        ]
    );
    opencode.model.clear();
    assert!(!launch_invocation(&local, &opencode, "")
        .args
        .iter()
        .any(|arg| arg == "--effort" || arg == "--model"));

    // Another agent never passes a model.
    let mut claude = launch_of("claude");
    claude.model = "m/x".into();
    claude.effort = "high".into();
    assert!(!launch_invocation(&local, &claude, "")
        .args
        .contains(&"--model".to_owned()));

    // A peer's alias comes first, the machine guard after it.
    let peer = launch_invocation(&ComputerId::from("box"), &launch_of("codex"), "m-1");
    assert_eq!(
        &peer.args[..4],
        ["@box", "--expect-machine", "m-1", "codex"]
    );
}

#[test]
fn a_launch_succeeds_on_exit_zero_and_fails_with_its_detail() {
    let exit = |code, stderr: &str| Exit::Code {
        code,
        stdout: b"ignored".to_vec(),
        stderr: stderr.into(),
    };
    assert_eq!(launched(exit(0, "")), Ok(()));
    assert_eq!(launched(exit(1, " busy\n")), Err("busy".into()));
    assert_eq!(launched(exit(3, "")), Err("exit code 3".into()));
    assert_eq!(launched(Exit::TimedOut), Err("timed out".into()));
}

#[test]
fn the_terminal_choice_follows_the_agent() {
    assert!(opens_terminal("codex", true));
    assert!(!opens_terminal("codex", false));
    assert!(!opens_terminal("dsh", true));
    assert!(!opens_terminal("opencode", true));
    assert_eq!(
        terminal_tooltip("dsh"),
        "DeepSeek opens in the workspace's Native UI."
    );
    assert_eq!(
        terminal_tooltip("opencode"),
        "OpenCode runs through its native server and opens in Activity."
    );
    assert_eq!(
        terminal_tooltip("kimi"),
        "Also open an external terminal for this session."
    );
}

fn windows(state: &str, session: Option<u64>) -> MachineMeta {
    MachineMeta {
        platform: Some("windows".into()),
        local_host: Some(LocalHost {
            state: Some(state.into()),
            windows_session: session,
            ..LocalHost::default()
        }),
        ..MachineMeta::default()
    }
}

#[test]
fn the_windows_session_warning_applies_only_to_another_session_here() {
    assert_eq!(
        other_windows_session(&windows("running", Some(2)), false, Some(1)),
        Some(2)
    );
    // The same session, a remote launch, no own session, a stopped host, a
    // host without a session and another platform never ask.
    assert_eq!(
        other_windows_session(&windows("running", Some(1)), false, Some(1)),
        None
    );
    assert_eq!(
        other_windows_session(&windows("running", Some(2)), true, Some(1)),
        None
    );
    assert_eq!(
        other_windows_session(&windows("running", Some(2)), false, None),
        None
    );
    assert_eq!(
        other_windows_session(&windows("stopped", Some(2)), false, Some(1)),
        None
    );
    assert_eq!(
        other_windows_session(&windows("running", None), false, Some(1)),
        None
    );
    let mut linux = windows("running", Some(2));
    linux.platform = Some("linux".into());
    assert_eq!(other_windows_session(&linux, false, Some(1)), None);
    assert_eq!(
        warning_text(2),
        "The session host on this computer runs in another Windows session (2), for example one \
         started over SSH. Agents it starts cannot show windows on this desktop, such as a \
         browser sign-in. Launch anyway?"
    );
    assert_eq!(WARNING_TITLE, "Launch in another Windows session?");
    assert_eq!(LAUNCH_ANYWAY, "Launch anyway");
}

#[test]
fn local_sessions_can_be_enabled_on_a_windows_console_with_a_host() {
    let mut console = windows("stopped", None);
    console.local_sessions = Some(false);
    assert!(can_enable_local_sessions(&console));
    console.local_sessions = None;
    assert!(!can_enable_local_sessions(&console));
    console.local_sessions = Some(false);
    console.local_host = Some(LocalHost::default());
    assert!(!can_enable_local_sessions(&console));
    let mut linux = windows("stopped", None);
    linux.local_sessions = Some(false);
    linux.platform = Some("linux".into());
    assert!(!can_enable_local_sessions(&linux));
}

#[test]
fn the_folder_check_lists_the_folder() {
    let local = directories_invocation(&ComputerId::local(), "", false);
    assert_eq!(local.args, ["dirs", "~"]);
    assert_eq!(local.timeout, Duration::from_secs(5));
    let peer = directories_invocation(&ComputerId::from("box"), "/srv", true);
    assert_eq!(peer.args, ["@box", "dirs", "--hidden", "/srv"]);
    assert_eq!(peer.timeout, Duration::from_secs(15));
    let ok = |text: &str| Exit::Code {
        code: 0,
        stdout: text.as_bytes().to_vec(),
        stderr: String::new(),
    };
    assert!(directories(ok(r#"{"path":"/srv","directories":[]}"#)).is_ok());
    for bad in [
        r#"{"path":1,"directories":[]}"#,
        r#"{"path":"/srv"}"#,
        "[]",
        "x",
    ] {
        assert_eq!(
            directories(ok(bad)),
            Err(INVALID_DIRECTORIES.into()),
            "{bad}"
        );
    }
    assert_eq!(
        directories(Exit::Code {
            code: 1,
            stdout: Vec::new(),
            stderr: "gone".into()
        }),
        Err("gone".into())
    );
}

#[test]
fn the_generated_branch_takes_eight_digits_of_a_uuid() {
    assert_eq!(
        generated_branch("1A2B3C4D-5e6f-4000-8000-000000000000"),
        "work-1a2b3c4d"
    );
    assert_eq!(
        generated_branch("0123456789abcdef0123456789abcdef"),
        "work-01234567"
    );
}

fn catalog() -> Value {
    json!({
        "state": "ok",
        "common_dir": "/srv/repo/.git",
        "selected_root": "/srv/repo",
        "path": "/srv/repo/src",
        "worktrees": [
            {"path": "/srv/repo", "kind": "main", "available": true},
            {"path": "/srv/repo-old", "kind": "linked", "available": false},
            {"path": "/srv/bare", "kind": "bare", "available": true}
        ]
    })
}

#[test]
fn a_generated_worktree_needs_a_current_verified_git_checkout() {
    let full = catalog();
    assert!(automatic_worktree_available(
        &full,
        "/srv/repo/src",
        Style::Posix,
        false
    ));
    // A read under way, a stale or failed catalog, no common directory, a
    // relative or nameless root, and folders outside an available non-bare
    // checkout never offer one.
    assert!(!automatic_worktree_available(
        &full,
        "/srv/repo/src",
        Style::Posix,
        true
    ));
    for (key, value) in [
        ("stale", json!(true)),
        ("state", json!("not_git")),
        ("common_dir", json!("")),
        ("selected_root", json!("repo")),
        ("selected_root", json!("/srv/repo/")),
    ] {
        let mut changed = full.clone();
        changed[key] = value;
        assert!(
            !automatic_worktree_available(&changed, "/srv/repo/src", Style::Posix, false),
            "{key}"
        );
    }
    assert!(!automatic_worktree_available(
        &full,
        "/srv/repo-old",
        Style::Posix,
        false
    ));
    assert!(!automatic_worktree_available(
        &full,
        "/srv/bare/x",
        Style::Posix,
        false
    ));
    assert!(!automatic_worktree_available(
        &full,
        "/srv/repository",
        Style::Posix,
        false
    ));

    // A partial catalog trusts Git's own answer for the requested folder,
    // unless the root is listed as missing or bare.
    let mut partial = json!({
        "state": "ok", "partial": true, "common_dir": "/srv/repo/.git",
        "selected_root": "/srv/repo", "path": "/srv/repo/deep", "worktrees": []
    });
    assert!(automatic_worktree_available(
        &partial,
        "",
        Style::Posix,
        false
    ));
    partial["worktrees"] = json!([{"path": "/srv/repo", "kind": "main", "available": false}]);
    assert!(!automatic_worktree_available(
        &partial,
        "",
        Style::Posix,
        false
    ));
    partial["worktrees"] = json!([]);
    partial["path"] = json!("/srv/other");
    assert!(!automatic_worktree_available(
        &partial,
        "",
        Style::Posix,
        false
    ));

    // Windows compares with either separator and ignoring case.
    let windows = json!({
        "state": "ok", "common_dir": "C:/srv/repo/.git", "selected_root": "C:/srv/repo",
        "worktrees": [{"path": "C:/srv/repo", "kind": "main", "available": true}]
    });
    assert!(automatic_worktree_available(
        &windows,
        "c:\\SRV\\Repo\\src",
        Style::Windows,
        false
    ));
    assert_eq!(
        automatic_worktree_path(&windows, "work-1a2b3c4d", Style::Windows),
        "C:/srv/repo-work-1a2b3c4d"
    );
}

#[test]
fn the_generated_worktree_goes_beside_the_selected_root() {
    assert_eq!(
        automatic_worktree_path(&catalog(), "work-1a2b3c4d", Style::Posix),
        "/srv/repo-work-1a2b3c4d"
    );
    let mut top = catalog();
    top["selected_root"] = json!("/repo");
    assert_eq!(
        automatic_worktree_path(&top, "work-1", Style::Posix),
        "/repo-work-1"
    );
    assert_eq!(
        repository_worktree_path("/srv/clone", "work-1"),
        "/srv/clone-work-1"
    );
    assert_eq!(
        worktree_tooltip("work-1").english(),
        "New branch: work-1\nStart from: HEAD"
    );
}

fn project(id: &str, folders: &[(&str, &str)]) -> Project {
    Project {
        id: id.into(),
        name: format!("Project {id}"),
        folders: folders
            .iter()
            .map(|(machine, path)| ProjectFolder {
                machine: (*machine).into(),
                path: (*path).into(),
            })
            .collect(),
    }
}

#[test]
fn a_project_worktree_shares_a_checkout_with_a_project_folder_here() {
    let mut catalog = created_catalog(&catalog(), "/srv/repo-work-1");
    let saved = project("p", &[("", "/srv/repo")]);
    assert!(project_worktree(
        "/srv/repo-work-1",
        &catalog,
        Some(&saved),
        "",
        Style::Posix
    ));
    // Another computer's folder, no project, or an unrelated folder do not.
    assert!(!project_worktree(
        "/srv/repo-work-1",
        &catalog,
        Some(&saved),
        "box",
        Style::Posix
    ));
    assert!(!project_worktree(
        "/srv/repo-work-1",
        &catalog,
        None,
        "",
        Style::Posix
    ));
    assert!(!project_worktree(
        "/srv/elsewhere",
        &catalog,
        Some(&saved),
        "",
        Style::Posix
    ));
    catalog["stale"] = json!(true);
    assert!(!project_worktree(
        "/srv/repo-work-1",
        &catalog,
        Some(&saved),
        "",
        Style::Posix
    ));
}

#[test]
fn a_session_folder_opens_on_the_deepest_project_folder() {
    let saved = project(
        "p",
        &[("", "/srv"), ("", "/srv/repo/"), ("box", "/srv/repo/src")],
    );
    assert_eq!(
        matching_project_folder("/srv/repo/src/x", "", Some(&saved), "", Style::Posix),
        "/srv/repo"
    );
    assert_eq!(
        matching_project_folder("/link/x", "/srv/repo/x", Some(&saved), "", Style::Posix),
        "/srv/repo"
    );
    assert_eq!(
        matching_project_folder("/other", "", Some(&saved), "", Style::Posix),
        "/other"
    );
}

#[test]
fn a_linked_worktree_session_opens_on_its_main_checkout() {
    let catalog = json!({
        "state": "ok", "common_dir": "/srv/repo/.git", "selected_root": "/srv/repo-work-1",
        "worktrees": [
            {"path": "/srv/repo", "kind": "main", "available": true},
            {"path": "/srv/repo-work-1", "kind": "linked", "available": true}
        ]
    });
    let saved = project("p", &[("", "/srv/repo")]);
    assert_eq!(
        prefill_target(
            &catalog,
            "/srv/repo-work-1/src",
            "",
            Some(&saved),
            "",
            Style::Posix
        ),
        Some("/srv/repo".into())
    );
    // A folder outside the root, a main checkout session, two main
    // checkouts and a partial catalog keep the session's folder.
    assert_eq!(
        prefill_target(&catalog, "/srv/other", "", Some(&saved), "", Style::Posix),
        None
    );
    let mut main = catalog.clone();
    main["selected_root"] = json!("/srv/repo");
    assert_eq!(
        prefill_target(&main, "/srv/repo", "", None, "", Style::Posix),
        None
    );
    let mut two = catalog.clone();
    two["worktrees"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path": "/srv/again", "kind": "main", "available": true}));
    assert_eq!(
        prefill_target(&two, "/srv/repo-work-1", "", None, "", Style::Posix),
        None
    );
    let mut partial = catalog.clone();
    partial["partial"] = json!(true);
    assert_eq!(
        prefill_target(&partial, "/srv/repo-work-1", "", None, "", Style::Posix),
        None
    );
}

#[test]
fn an_exact_saved_folder_places_the_session_only_when_unambiguous() {
    let projects = [
        project("ungrouped", &[("", "/srv/inbox")]),
        project("a", &[("", "/srv/a"), ("box", "/srv/b")]),
        project("b", &[("", "/srv/b/"), ("", "/srv/c")]),
    ];
    assert_eq!(
        exact_saved_project(&projects, "", "/srv/a", Style::Posix),
        Some("a")
    );
    // Inbox, an uncleaned saved path, another computer and a prefix do not.
    assert_eq!(
        exact_saved_project(&projects, "", "/srv/inbox", Style::Posix),
        None
    );
    assert_eq!(
        exact_saved_project(&projects, "", "/srv/b", Style::Posix),
        None
    );
    assert_eq!(
        exact_saved_project(&projects, "box", "/srv/b", Style::Posix),
        Some("a")
    );
    assert_eq!(
        exact_saved_project(&projects, "", "/srv/a/x", Style::Posix),
        None
    );
    // Two projects with the folder are ambiguous.
    let twice = [
        project("a", &[("", "/srv/a")]),
        project("b", &[("", "/srv/a")]),
    ];
    assert_eq!(
        exact_saved_project(&twice, "", "/srv/a", Style::Posix),
        None
    );
    // Beyond 512 folders nothing is placed.
    let mut many: Vec<Project> = (0..SAVED_PROJECTS)
        .map(|index| project(&format!("p{index}"), &[("box", "/x")]))
        .collect();
    many.push(project("a", &[("", "/srv/a")]));
    assert_eq!(exact_saved_project(&many, "", "/srv/a", Style::Posix), None);
}

fn context<'a>(projects: &'a [Project]) -> CheckContext<'a> {
    CheckContext {
        style: Style::Posix,
        host: "",
        with_worktree: false,
        automatic_available: false,
        project_explicit: false,
        project: INBOX,
        projects,
        swarm: "S",
        listed_folder: false,
        project_worktree: false,
    }
}

#[test]
fn the_folder_check_confirms_places_and_adds_the_folder() {
    let projects = [project("a", &[("", "/srv/a")]), project("b", &[])];
    let base = context(&projects);
    // The computer must confirm an absolute folder.
    assert_eq!(
        checked(&json!({"path": "w"}), &base),
        Checked::Failed {
            error: FOLDER_UNCONFIRMED.into()
        }
    );
    // A generated worktree still needs a verified catalog.
    let worktree = CheckContext {
        with_worktree: true,
        ..base.clone()
    };
    assert_eq!(
        checked(&json!({"path": "/srv/a"}), &worktree),
        Checked::Failed {
            error: REPOSITORY_UNCONFIRMED.into()
        }
    );
    // An untouched Inbox takes the saved project with exactly this folder.
    assert_eq!(
        checked(&json!({"path": "/srv/a"}), &base),
        Checked::Launch {
            path: "/srv/a".into(),
            project: Some("a".into()),
            add_folder: None
        }
    );
    // This swarm's hint wins; a foreign hint places nothing, never falling
    // back to a saved folder, and the folder is added to Inbox.
    let hint = |swarm: &str, id: &str| json!({"path": "/srv/a", "project_inference": {"schema": 1, "swarm_id": swarm, "project_id": id}});
    assert_eq!(
        checked(&hint("S", "b"), &base),
        Checked::Launch {
            path: "/srv/a".into(),
            project: Some("b".into()),
            add_folder: None
        }
    );
    assert_eq!(
        checked(&hint("T", "b"), &base),
        Checked::Launch {
            path: "/srv/a".into(),
            project: None,
            add_folder: Some("/srv/a".into())
        }
    );
    // An unknown project or Inbox itself places nothing.
    assert!(matches!(
        checked(&hint("S", "gone"), &base),
        Checked::Launch { project: None, .. }
    ));
    // A chosen project always wins; a listed folder or a project worktree
    // is not added again.
    let chosen = CheckContext {
        project_explicit: true,
        project: "b",
        ..base.clone()
    };
    assert_eq!(
        checked(&json!({"path": "/srv/a"}), &chosen),
        Checked::Launch {
            path: "/srv/a".into(),
            project: None,
            add_folder: Some("/srv/a".into())
        }
    );
    let listed = CheckContext {
        listed_folder: true,
        ..chosen.clone()
    };
    assert!(matches!(
        checked(&json!({"path": "/srv/a"}), &listed),
        Checked::Launch {
            add_folder: None,
            ..
        }
    ));
    let member = CheckContext {
        project_worktree: true,
        ..chosen.clone()
    };
    assert!(matches!(
        checked(&json!({"path": "/srv/a"}), &member),
        Checked::Launch {
            add_folder: None,
            ..
        }
    ));
    // With a verified catalog the worktree is created after the check.
    let verified = CheckContext {
        automatic_available: true,
        ..worktree
    };
    assert!(matches!(
        checked(&json!({"path": "/srv/a"}), &verified),
        Checked::CreateWorktree { .. }
    ));
}

fn ready() -> Form {
    Form {
        online: true,
        agent: "codex".into(),
        name: "fix".into(),
        account_count: 1,
        account_selected: true,
        path: "/srv/repo".into(),
        listed_folder: true,
        folder_online: true,
        catalog_worktrees: true,
        auto_preferred: true,
        automatic_available: true,
        branch: "work-1a2b3c4d".into(),
        automatic_path: "/srv/repo-work-1a2b3c4d".into(),
        style: Style::Posix,
        ..Form::default()
    }
}

#[test]
fn start_runs_with_qt_texts_and_reasons() {
    let form = ready();
    let shown = shown(&form);
    assert!(shown.start_enabled);
    assert_eq!(shown.start_text.english(), START_SESSION);
    assert!(shown.automatic);
    assert_eq!(shown.worktree_target, "/srv/repo-work-1a2b3c4d");
    assert_eq!(
        shown
            .worktree_tooltip
            .map(|t| t.english())
            .unwrap_or_default(),
        "New branch: work-1a2b3c4d\nStart from: HEAD"
    );
    assert_eq!(shown.preview, None);
    assert_eq!(shown.notice, None);

    for (change, why) in [
        (
            Form {
                name: String::new(),
                ..ready()
            },
            "name",
        ),
        (
            Form {
                name: "a/b".into(),
                ..ready()
            },
            "name rule",
        ),
        (
            Form {
                account_selected: false,
                ..ready()
            },
            "account",
        ),
        (
            Form {
                account_loading: true,
                ..ready()
            },
            "accounts loading",
        ),
        (
            Form {
                no_machine: true,
                ..ready()
            },
            "machine",
        ),
        (
            Form {
                path: String::new(),
                ..ready()
            },
            "folder",
        ),
        (
            Form {
                folder_online: false,
                ..ready()
            },
            "offline folder",
        ),
        (
            Form {
                prefill_pending: true,
                ..ready()
            },
            "prefill",
        ),
        (
            Form {
                catalog_pending: true,
                ..ready()
            },
            "catalog read with Auto",
        ),
        (
            Form {
                creation_failed: true,
                ..ready()
            },
            "failed creation with Auto",
        ),
        (
            Form {
                stage: Stage::CheckingFolder,
                ..ready()
            },
            "checking",
        ),
        (
            Form {
                stage: Stage::CreatingWorktree,
                ..ready()
            },
            "creating",
        ),
    ] {
        assert!(!launch::shown(&change).start_enabled, "{why}");
    }
    // A shell starts while accounts load; None ignores the catalog read and
    // a failed generated worktree.
    assert!(
        launch::shown(&Form {
            agent: "sh".into(),
            account_loading: true,
            ..ready()
        })
        .start_enabled
    );
    assert!(
        launch::shown(&Form {
            auto_preferred: false,
            catalog_pending: true,
            ..ready()
        })
        .start_enabled
    );
    assert!(
        launch::shown(&Form {
            auto_preferred: false,
            creation_failed: true,
            ..ready()
        })
        .start_enabled
    );
    assert_eq!(
        launch::shown(&Form {
            name: "a/b".into(),
            ..ready()
        })
        .name_problem
        .map(|t| t.english())
        .as_deref(),
        Some("Names cannot contain /, : or . or control characters.")
    );
    assert_eq!(
        launch::shown(&Form {
            stage: Stage::CheckingFolder,
            ..ready()
        })
        .start_text
        .english(),
        "Checking folder…"
    );
    assert_eq!(
        launch::shown(&Form {
            stage: Stage::CreatingWorktree,
            ..ready()
        })
        .start_text
        .english(),
        "Creating worktree…"
    );
}

#[test]
fn a_repository_source_clones_and_starts() {
    let repository = Repository {
        can_submit: true,
        destination: "/srv/clone".into(),
        ..Repository::default()
    };
    let form = Form {
        repository: Some(repository.clone()),
        path: String::new(),
        automatic_available: false,
        ..ready()
    };
    let shown = shown(&form);
    assert!(shown.start_enabled);
    assert_eq!(shown.start_text.english(), "Clone and start");
    // Auto stays available for a repository: the clone is a Git checkout.
    assert!(shown.automatic && shown.auto_enabled);
    assert_eq!(shown.worktree_target, "/srv/clone-work-1a2b3c4d");
    assert!(!shown.worktree_options_enabled);
    let text = |repository: Repository| {
        launch::shown(&Form {
            repository: Some(repository),
            ..form.clone()
        })
        .start_text
        .english()
    };
    assert_eq!(
        text(Repository {
            busy: true,
            cloning: true,
            ..repository.clone()
        }),
        "Cloning…"
    );
    assert_eq!(
        text(Repository {
            busy: true,
            ..repository.clone()
        }),
        "Checking…"
    );
    assert_eq!(
        text(Repository {
            created: true,
            ..repository.clone()
        }),
        "Use checkout and start"
    );
    // A new project needs its name; an offline computer cannot clone.
    let unnamed = Form {
        repository: Some(Repository {
            new_project: true,
            new_project_name: " ".into(),
            ..repository.clone()
        }),
        ..form.clone()
    };
    assert!(!launch::shown(&unnamed).start_enabled);
    assert!(
        !launch::shown(&Form {
            online: false,
            ..form.clone()
        })
        .start_enabled
    );
    // Cancel waits for a clone, not for a check.
    assert!(
        !launch::shown(&Form {
            repository: Some(Repository {
                busy: true,
                cloning: true,
                ..repository.clone()
            }),
            ..form.clone()
        })
        .cancel_enabled
    );
    assert!(
        launch::shown(&Form {
            repository: Some(Repository {
                busy: true,
                ..repository
            }),
            ..form.clone()
        })
        .cancel_enabled
    );
    assert_eq!(start(&form, false, false), Start::Clone);
}

#[test]
fn previews_and_notices_use_qt_texts_in_qt_order() {
    let preview = |form: Form| {
        launch::shown(&form)
            .preview
            .map(|t| t.english())
            .unwrap_or_default()
    };
    assert_eq!(
        preview(Form {
            prefill_pending: true,
            ..ready()
        }),
        "Checking the session's main working folder…"
    );
    assert_eq!(
        preview(Form {
            path: String::new(),
            ..ready()
        }),
        "Browse to choose a folder on this computer."
    );
    assert_eq!(
        preview(Form {
            implicit_inbox: true,
            listed_folder: false,
            ..ready()
        }),
        "A matching project is detected automatically.\nOtherwise this session stays in Inbox."
    );
    let outside = launch::shown(&Form {
        listed_folder: false,
        ..ready()
    });
    assert_eq!(
        outside.preview.map(|t| t.english()).unwrap_or_default(),
        "This folder is outside the project.\nStarting here will add it to this project."
    );
    assert!(outside.outside_project);
    assert!(
        !launch::shown(&Form {
            listed_folder: false,
            project_worktree: true,
            ..ready()
        })
        .outside_project
    );

    let notice = |form: Form| launch::shown(&form).notice.map(|t| t.english());
    assert_eq!(
        notice(Form {
            folder_online: false,
            ..ready()
        })
        .as_deref(),
        Some("This computer is unavailable. Choose another folder or reconnect it.")
    );
    assert_eq!(
        notice(Form {
            account_count: 0,
            folder_online: false,
            ..ready()
        })
        .as_deref(),
        Some("Restore or add an account in Agent accounts to start this agent.")
    );
    assert_eq!(
        notice(Form {
            account_preset: true,
            account_selected: false,
            account_loading: true,
            account_count: 0,
            ..ready()
        })
        .as_deref(),
        Some("Loading this session's account…")
    );
    assert_eq!(
        notice(Form {
            account_preset: true,
            account_selected: false,
            ..ready()
        })
        .as_deref(),
        Some("This session's account is unavailable. Choose another account.")
    );
    assert_eq!(
        notice(Form {
            no_machine: true,
            account_preset: true,
            account_selected: false,
            ..ready()
        })
        .as_deref(),
        Some("Add a remote machine in Machines to start sessions from this computer.")
    );
    assert_eq!(
        notice(Form {
            no_machine: true,
            can_enable_local_sessions: true,
            ..ready()
        })
        .as_deref(),
        Some("Enable local sessions for this computer in Machines, or add a remote machine.")
    );
    assert_eq!(
        PROJECT_CHANGED,
        "Project changed. Select the checkout again before starting."
    );
}

// AGENTS.md: inspecting the form or opening the dialog never creates a
// worktree; a generated worktree is created only after Start session, and
// only for a verified Git folder; ordinary non-Git folders are supported and
// the client never runs git init; direct-folder launch and the
// custom/existing worktree choices stay available.
#[test]
fn agents_md_only_start_creates_a_worktree_and_only_for_a_verified_git_folder() {
    // shown() only answers; Start is the one step that acts.
    assert_eq!(
        start(&ready(), false, false),
        Start::CheckFolder {
            with_worktree: true
        }
    );
    // Without a verified catalog Auto is neither checked nor enabled, and
    // Start checks the folder and launches directly.
    let plain = Form {
        automatic_available: false,
        catalog_worktrees: false,
        ..ready()
    };
    let shown = shown(&plain);
    assert!(shown.start_enabled && !shown.automatic && !shown.auto_enabled);
    assert_eq!(shown.worktree_target, "");
    assert_eq!(
        start(&plain, false, false),
        Start::CheckFolder {
            with_worktree: false
        }
    );
    // A disabled or submitted Start does nothing.
    assert_eq!(
        start(
            &Form {
                name: String::new(),
                ..ready()
            },
            false,
            false
        ),
        Start::Nothing
    );
    assert_eq!(start(&ready(), true, true), Start::Nothing);
    assert_eq!(start(&ready(), true, false), Start::Background);
    // The folder check refuses a worktree the catalog no longer verifies.
    assert!(matches!(
        checked(
            &json!({"path": "/srv/repo"}),
            &CheckContext {
                with_worktree: true,
                automatic_available: false,
                ..context(&[])
            }
        ),
        Checked::Failed { .. }
    ));
}

#[test]
fn agents_md_ordinary_folders_start_and_nothing_runs_git_init() {
    let not_git = json!({"state": "not_git", "worktrees": []});
    assert!(!automatic_worktree_available(
        &not_git,
        "/srv/notes",
        Style::Posix,
        false
    ));
    let form = Form {
        path: "/srv/notes".into(),
        automatic_available: false,
        catalog_worktrees: false,
        ..ready()
    };
    assert!(launch::shown(&form).start_enabled);
    assert_eq!(
        start(&form, false, false),
        Start::CheckFolder {
            with_worktree: false
        }
    );
    let calls = [
        launch_invocation(&ComputerId::local(), &launch_of("codex"), ""),
        directories_invocation(&ComputerId::local(), "/srv/notes", false),
    ];
    for call in calls {
        assert!(
            !call.args.iter().any(|arg| arg == "git" || arg == "init"),
            "{:?}",
            call.args
        );
    }
}

#[test]
fn agents_md_direct_launch_and_worktree_choices_stay_available() {
    let shown = shown(&ready());
    assert!(shown.none_enabled, "None starts directly in the folder");
    assert!(shown.choose_existing_enabled, "Choose existing…");
    assert!(shown.create_manually_enabled, "Create manually…");
    assert!(shown.worktree_options_enabled);
    assert_eq!(NONE_TOOLTIP, "Start directly in the working folder.");
    assert_eq!(
        AUTO_TOOLTIP,
        "Create a generated branch and worktree when you start the session."
    );
    assert_eq!(
        (CHOOSE_EXISTING, CREATE_MANUALLY),
        ("Choose existing…", "Create manually…")
    );
    // With None preferred the folder starts directly, even in a Git checkout.
    let none = Form {
        auto_preferred: false,
        ..ready()
    };
    assert!(!launch::shown(&none).automatic);
    assert_eq!(
        start(&none, false, false),
        Start::CheckFolder {
            with_worktree: false
        }
    );
    // A catalog read under way holds both until it answers.
    let pending = launch::shown(&Form {
        catalog_pending: true,
        ..ready()
    });
    assert!(!pending.choose_existing_enabled && !pending.create_manually_enabled);
}

fn choices() -> Choices {
    Choices {
        host: "box".into(),
        machine: "box.example".into(),
        agent: "codex".into(),
        account: "a".into(),
        name: "fix".into(),
        project: "p".into(),
        project_name: "Project".into(),
        swarm: "S".into(),
        path: "/srv/repo".into(),
        auto_preferred: true,
        automatic_available: true,
        branch: "work-1".into(),
        catalog: catalog(),
        automatic_path: "/srv/repo-work-1".into(),
        model: "m/x".into(),
        effort: "high".into(),
        open_terminal: true,
        ..Choices::default()
    }
}

#[test]
fn the_background_plan_follows_qt() {
    let plan = plan(&choices());
    let value = serde_json::to_value(&plan).unwrap();
    assert_eq!(value["cmd"], "codex");
    assert_eq!(value["tag"], "fix");
    assert_eq!(value["group"], "p");
    assert_eq!(value["group_name"], "Project");
    assert_eq!(value["target"], "/srv/repo");
    assert_eq!(value["source"], "/srv/repo");
    assert_eq!(value["auto"], true);
    assert_eq!(value["destination"], "/srv/repo-work-1");
    assert_eq!(value["add_folder"], true);
    assert_eq!(value["terminal"], true);
    // Only OpenCode keeps a model, and effort only with it.
    assert_eq!(
        (value["model"].as_str(), value["effort"].as_str()),
        (Some(""), Some(""))
    );
    assert!(value.get("url").is_none() && value.get("predecessor").is_none());

    let opencode = launch::plan(&Choices {
        agent: "opencode".into(),
        ..choices()
    });
    assert_eq!(
        (opencode.model.as_str(), opencode.effort.as_str()),
        ("m/x", "high")
    );
    assert!(!opencode.terminal);

    // None, or no verified catalog, plans no generated worktree.
    let direct = launch::plan(&Choices {
        automatic_available: false,
        ..choices()
    });
    assert!(!direct.auto && direct.destination.is_none());

    // A finished clone starts in its checkout, already in the project.
    let clone = launch::plan(&Choices {
        repository: Some(RepositoryChoice {
            url: "https://example.com/r.git".into(),
            parent: "/srv".into(),
            name: "r".into(),
            destination: "/srv/r".into(),
            created_path: "/srv/r".into(),
            new_project: true,
            project_name: " New ".into(),
        }),
        predecessor: "old".into(),
        ..choices()
    });
    assert_eq!(clone.target, "/srv/r");
    assert_eq!(clone.clone_done, Some(true));
    assert_eq!(clone.acquisition_confirmed, Some(true));
    assert!(!clone.add_folder);
    assert!(clone.destination.is_none());
    assert_eq!(clone.project_name.as_deref(), Some("New"));
    assert_eq!(clone.predecessor.as_deref(), Some("old"));
}

#[test]
fn a_failed_creation_reopens_on_its_folder() {
    assert_eq!(
        restored_folder(&json!({"source": "/srv/repo", "target": "/srv/x", "auto": true})),
        ("/srv/repo".into(), true)
    );
    assert_eq!(
        restored_folder(
            &json!({"source": "/srv/repo", "target": "/srv/repo-work-1", "auto": true, "worktree_done": true})
        ),
        ("/srv/repo-work-1".into(), false)
    );
    assert_eq!(
        restored_folder(&json!({"target": "/srv/r", "clone_done": true})),
        ("/srv/r".into(), false)
    );
}

#[test]
fn the_account_is_remembered_per_computer_and_agent() {
    let mut accounts = Value::Null;
    remember_account(&mut accounts, "", "codex", "a");
    remember_account(&mut accounts, "box", "claude", "b");
    remember_account(&mut accounts, "", "claude", "");
    assert_eq!(
        accounts,
        json!({"": {"codex": "a"}, "box": {"claude": "b"}})
    );
}

fn ok(value: Value) -> Exit {
    Exit::Code {
        code: 0,
        stdout: value.to_string().into_bytes(),
        stderr: String::new(),
    }
}

fn failed(stderr: &str) -> Exit {
    Exit::Code {
        code: 1,
        stdout: Vec::new(),
        stderr: stderr.into(),
    }
}

// G4: LocalSessionHost::ownWindowsSession, which other_windows_session takes as `own`.
#[test]
fn this_process_has_a_windows_session_only_on_windows() {
    let own = own_windows_session();
    if cfg!(windows) {
        assert!(own.is_some());
    } else {
        assert_eq!(own, None);
    }
    assert_eq!(
        own_windows_session(),
        own,
        "a process never changes its session"
    );
}

// Q1: HgsClient::requestOpenCodeModels, requestOpenCodeFavorite and
// requestAccounts {"start", "native-opencode", "--json"}.
#[test]
fn the_opencode_calls_follow_hgs_client() {
    let mac = ComputerId::from("mac");
    let models = opencode_models_invocation(&mac, "/workspace/example");
    assert_eq!(
        models.args,
        [
            "@mac",
            "account",
            "models",
            "native-opencode",
            "--location",
            "/workspace/example",
            "--json"
        ]
    );
    assert_eq!(models.timeout, Duration::from_secs(30));
    let on = opencode_favorite_invocation(&ComputerId::local(), "fixture/plain", true, "req-1");
    assert_eq!(
        on.args,
        [
            "account",
            "favorite",
            "native-opencode",
            "--model",
            "fixture/plain",
            "--favorite",
            "on",
            "--request-id",
            "req-1",
            "--json"
        ]
    );
    assert_eq!(on.timeout, Duration::from_secs(25));
    assert_eq!(
        opencode_favorite_invocation(&mac, "fixture/plain", false, "req-2").args[..8],
        [
            "@mac",
            "account",
            "favorite",
            "native-opencode",
            "--model",
            "fixture/plain",
            "--favorite",
            "off"
        ]
    );
    let start = opencode_start_invocation(&mac);
    assert_eq!(
        start.args,
        ["@mac", "account", "start", "native-opencode", "--json"]
    );
    assert_eq!(start.timeout, Duration::from_secs(60));
    assert_eq!(
        opencode_start_invocation(&ComputerId::local()).args,
        ["account", "start", "native-opencode", "--json"]
    );
    assert_eq!(OPENCODE_CATALOG_LIMIT, 5 * 1024 * 1024);
}

#[test]
fn the_opencode_answers_follow_qt() {
    let catalog = json!({"catalog_state": "ready", "model_options": []});
    assert_eq!(opencode_models(ok(catalog.clone())), Ok(catalog));
    for bad in [json!([]), json!("x"), json!(1)] {
        assert_eq!(
            opencode_models(ok(bad)),
            Err("Invalid OpenCode model catalog response".into())
        );
    }
    assert_eq!(
        opencode_models(Exit::Code {
            code: 0,
            stdout: b"not json".to_vec(),
            stderr: String::new()
        }),
        Err(INVALID_OPENCODE_CATALOG.into())
    );
    assert_eq!(
        opencode_models(failed(" offline \n")),
        Err("offline".into())
    );
    assert_eq!(
        OPENCODE_TOO_LARGE,
        "The OpenCode model catalog is too large"
    );
    // The receipt names this exact change.
    let receipt = json!({"status": "applied", "request_id": "req-1", "model": "fixture/plain", "favorite": true});
    assert_eq!(
        opencode_favorite(ok(receipt.clone()), "req-1", "fixture/plain", true),
        Ok(receipt.clone())
    );
    for (key, value) in [
        ("status", json!("queued")),
        ("request_id", json!("req-2")),
        ("model", json!("fixture/other")),
        ("favorite", json!(false)),
        ("favorite", json!("on")),
    ] {
        let mut changed = receipt.clone();
        changed[key] = value;
        assert_eq!(
            opencode_favorite(ok(changed), "req-1", "fixture/plain", true),
            Err("Invalid favorite response".into()),
            "{key}"
        );
    }
    assert_eq!(
        opencode_favorite(ok(json!([])), "req-1", "fixture/plain", true),
        Err(INVALID_FAVORITE.into())
    );
    assert_eq!(
        opencode_favorite(
            failed("Account catalog is busy. Retry after the current operation finishes.\n"),
            "req-1",
            "fixture/plain",
            true
        ),
        Err("Account catalog is busy. Retry after the current operation finishes.".into())
    );
    // The start answers with the catalog, an object.
    assert!(opencode_started(ok(json!({"model_options": []}))).is_ok());
    assert_eq!(
        opencode_started(ok(json!([]))),
        Err("Invalid account catalog response".into())
    );
    assert_eq!(opencode_started(Exit::TimedOut), Err("timed out".into()));
}

// OpenCodeModels::errorText (OpenCodeModels.cpp:148-162).
#[test]
fn an_older_cli_usage_line_asks_to_update_zerus() {
    let update = "Update Zerus on mac to search all models and star favorites.";
    assert_eq!(opencode_update_text("mac"), update);
    assert_eq!(
        opencode_error_text(
            "usage: hgs account ls | add ID --provider codex|claude|kimi --label NAME",
            "mac"
        ),
        update
    );
    // hgs prefixes its errors, and ssh warnings can come first.
    let warned = "Warning: Permanently added 'mac' to the list of known hosts.\nhgs: hgs: usage: hgs account models";
    assert_eq!(opencode_error_text(warned, "mac"), update);
    // Only the first 16 lines of the first 4000 characters count.
    let late = format!("{}usage: hgs account models", "line\n".repeat(16));
    assert_eq!(opencode_error_text(&late, "mac"), late);
    let far = format!("{}\nusage: hgs account models", "x".repeat(4000));
    assert_eq!(opencode_error_text(&far, "mac"), far);
    assert_eq!(
        opencode_error_text("Account catalog is busy.", "pc"),
        "Account catalog is busy."
    );
}

// Q2: HgsClient::createDirectory (HgsClient.cpp:1810-1870).
#[test]
fn folder_creation_probes_pins_the_parent_and_creates_with_one_identity() {
    let peer = ComputerId::from("mac");
    let probe = directory_create_probe(&peer);
    assert_eq!(probe.args, ["@mac", "--help"]);
    assert_eq!(probe.timeout, Duration::from_secs(15));
    assert_eq!(
        directory_create_probe(&ComputerId::local()).timeout,
        Duration::from_secs(5)
    );
    let help = |text: &str| Exit::Code {
        code: 0,
        stdout: text.as_bytes().to_vec(),
        stderr: String::new(),
    };
    assert_eq!(
        directory_create_supported(help("hgs dirs --create <path> --request-id ID"), &peer),
        Ok(())
    );
    assert_eq!(
        directory_create_supported(help("usage: hgs dirs PATH"), &peer),
        Err("Update hgs on mac to create folders. You can still choose existing folders.".into())
    );
    assert_eq!(
        directory_create_supported(help("usage"), &ComputerId::local()),
        Err(
            "Update hgs on this computer to create folders. You can still choose existing folders."
                .into()
        )
    );
    assert_eq!(
        directory_create_supported(failed("ssh: no route"), &peer),
        Err("ssh: no route".into())
    );
    // The parent of the requested folder, as Qt splits it.
    assert_eq!(
        directory_parent("/srv/repo/new", Style::Posix),
        ("/srv/repo".into(), "new".into())
    );
    assert_eq!(
        directory_parent("/new", Style::Posix),
        ("/".into(), "new".into())
    );
    assert_eq!(
        directory_parent("new", Style::Posix),
        (".".into(), "new".into())
    );
    assert_eq!(
        directory_parent("C:\\work\\new", Style::Windows),
        ("C:/work".into(), "new".into())
    );
    assert_eq!(
        directory_parent("C:\\work\\new", Style::Posix),
        (".".into(), "C:\\work\\new".into())
    );
    // The canonical parent is pinned before anything is created.
    let parent = directories_invocation(&peer, "/srv/repo", false);
    assert_eq!(parent.args, ["@mac", "dirs", "/srv/repo"]);
    assert_eq!(
        directory_parent_confirmed(
            ok(json!({"path": "/srv/real", "directories": []})),
            Style::Posix
        ),
        Ok("/srv/real".into())
    );
    for bad in [
        json!({"path": "srv", "directories": []}),
        json!({"path": "/srv"}),
        json!([]),
    ] {
        assert_eq!(
            directory_parent_confirmed(ok(bad), Style::Posix),
            Err(
                "The computer did not confirm the parent folder. Open it before creating a folder."
                    .into()
            )
        );
    }
    assert_eq!(
        directory_parent_confirmed(failed("gone"), Style::Posix),
        Err("gone".into())
    );
    let create = directory_create_invocation(&peer, "/srv/repo/new", "req-1");
    assert_eq!(
        create.args,
        [
            "@mac",
            "dirs",
            "--create",
            "/srv/repo/new",
            "--request-id",
            "req-1"
        ]
    );
    assert_eq!(create.timeout, Duration::from_secs(15));
    assert_eq!(
        directory_create_invocation(&ComputerId::local(), "/r/n", "req-1").timeout,
        Duration::from_secs(5)
    );
}

#[test]
fn a_created_folder_must_match_its_request_and_pinned_parent() {
    let receipt = json!({"status": "created", "request_id": "req-1", "requested_path": "/srv/repo/new",
                         "path": "/srv/real/new", "parent": "/srv/real"});
    let created = |data: Value| {
        directory_created(
            ok(data),
            "req-1",
            "/srv/repo/new",
            "/srv/real",
            "new",
            Style::Posix,
        )
    };
    assert_eq!(created(receipt.clone()), Ok(receipt.clone()));
    let unconfirmed =
        "Folder creation could not be confirmed. Open the path to check it before trying again.";
    for (key, value) in [
        ("status", json!("exists")),
        ("request_id", json!("req-2")),
        ("requested_path", json!("/srv/real/new")),
        ("path", json!("/srv/real/other")),
        ("path", json!("new")),
        ("parent", json!("/srv/repo")),
    ] {
        let mut changed = receipt.clone();
        changed[key] = value;
        assert_eq!(created(changed), Err(unconfirmed.into()), "{key}");
    }
    assert_eq!(created(json!([])), Err(unconfirmed.into()));
    assert_eq!(
        directory_created(
            failed("permission denied"),
            "req-1",
            "/srv/repo/new",
            "/srv/real",
            "new",
            Style::Posix
        ),
        Err(
            "permission denied\nOpen the path to check whether the folder exists before trying again."
                .into()
        )
    );
    // Windows compares with either separator and ignoring case.
    let windows = json!({"status": "created", "request_id": "req-1", "requested_path": "C:\\Work\\New",
                         "path": "c:\\work\\new", "parent": "C:/Work"});
    assert!(directory_created(
        ok(windows),
        "req-1",
        "C:\\Work\\New",
        "C:/Work",
        "New",
        Style::Windows
    )
    .is_ok());
}

// The three derived fields come from the command, so the view may leave them out.
#[test]
fn the_form_takes_its_derived_fields_as_optional() {
    let mut value = serde_json::to_value(Form::default()).unwrap();
    for key in ["automatic_available", "automatic_path", "project_worktree"] {
        value.as_object_mut().unwrap().remove(key);
    }
    let form: Form = serde_json::from_value(value).unwrap();
    assert!(!form.automatic_available && form.automatic_path.is_empty() && !form.project_worktree);
}

// requestAccounts {"ls"} (HgsClient.cpp 607-620) with failureDetail
// (466-479): the dialog shows "Could not load accounts: %1" with it.
#[test]
fn an_account_catalog_failure_reads_as_qts_failure_detail() {
    let hgs = std::path::Path::new("/opt/example/hgs");
    let catalog = json!({"profiles": [], "revision": "r"});
    assert_eq!(accounts_reply(hgs, ok(catalog.clone())), Ok(catalog));
    assert_eq!(accounts_reply(hgs, Exit::TimedOut), Err("timed out".into()));
    assert_eq!(
        accounts_reply(hgs, Exit::StartFailed("No such file or directory".into())),
        Err("cannot start '/opt/example/hgs': No such file or directory".into())
    );
    assert_eq!(
        accounts_reply(
            hgs,
            Exit::Code {
                code: 3,
                stdout: Vec::new(),
                stderr: " \n".into()
            }
        ),
        Err("exit code 3".into())
    );
    let long = "e".repeat(5000);
    assert_eq!(
        accounts_reply(hgs, failed(&format!("  {long}\n"))),
        Err("e".repeat(4000))
    );
    for bad in [json!([]), json!({"profiles": {}}), json!({"revision": "r"})] {
        assert_eq!(
            accounts_reply(hgs, ok(bad)),
            Err("Invalid account catalog response".into())
        );
    }
    assert_eq!(
        accounts_reply(hgs, ok(json!({"error": "Account catalog is busy."}))),
        Err("Account catalog is busy.".into()),
        "a refusal on success keeps its text"
    );
}
