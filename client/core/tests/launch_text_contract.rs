//! The New session form's wire shape, shared with the webview's `LaunchShown`
//! through a checked-in fixture: this test pins the Rust side, and the client
//! test reads the same file as `LaunchShown` and renders every Text in Russian.
use std::path::Path;
use zerus_client_core::launch::{self, Form, Repository, Stage};

fn ready() -> Form {
    Form {
        online: true,
        agent: "claude".into(),
        name: "claude/project/review".into(),
        account_count: 1,
        account_selected: true,
        folder_online: true,
        ..Form::default()
    }
}

fn samples() -> Vec<launch::Shown> {
    let forms = [
        // No computer: the Machines notice, the browse preview.
        Form {
            no_machine: true,
            can_enable_local_sessions: true,
            ..ready()
        },
        Form {
            no_machine: true,
            ..ready()
        },
        // The prefill check, then a folder outside the project.
        Form {
            prefill_pending: true,
            ..ready()
        },
        Form {
            path: "/srv/repo".into(),
            ..ready()
        },
        // A listed folder with Auto: the generated branch tooltip.
        Form {
            path: "/srv/repo".into(),
            listed_folder: true,
            auto_preferred: true,
            automatic_available: true,
            branch: "work-1a2b3c4d".into(),
            automatic_path: "/srv/repo-work-1a2b3c4d".into(),
            ..ready()
        },
        // The account cases and an invalid name.
        Form {
            account_preset: true,
            account_selected: false,
            account_loading: true,
            ..ready()
        },
        Form {
            account_count: 0,
            path: "/srv/repo".into(),
            listed_folder: true,
            ..ready()
        },
        Form {
            name: "bad/name".into(),
            path: "/srv/repo".into(),
            listed_folder: true,
            ..ready()
        },
        // Stages and the repository source.
        Form {
            path: "/srv/repo".into(),
            listed_folder: true,
            stage: Stage::CreatingWorktree,
            ..ready()
        },
        Form {
            repository: Some(Repository {
                busy: true,
                cloning: true,
                ..Repository::default()
            }),
            ..ready()
        },
        Form {
            repository: Some(Repository {
                created: true,
                can_submit: true,
                ..Repository::default()
            }),
            ..ready()
        },
        // Agent-specific terminal tooltips.
        Form {
            agent: "dsh".into(),
            path: "/srv/repo".into(),
            listed_folder: true,
            ..ready()
        },
        Form {
            agent: "opencode".into(),
            implicit_inbox: true,
            path: "/srv/repo".into(),
            ..ready()
        },
    ];
    forms.iter().map(launch::shown).collect()
}

#[test]
fn the_new_session_wire_fixture_is_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/new-session-shown-v1.json");
    let mut current = serde_json::to_string_pretty(&samples()).unwrap() + "\n";
    // Windows renders the sample paths with its own separator (`\srv\repo-…`);
    // the fixture holds no other backslash, so the shape compares as on Unix.
    if cfg!(windows) {
        current = current.replace(r"\\", "/");
    }
    if std::env::var_os("ZERUS_UPDATE_FIXTURES").is_some() {
        std::fs::write(&path, &current).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap_or_default(),
        current,
        "the Shown wire shape changed: rerun with ZERUS_UPDATE_FIXTURES=1 and update LaunchShown in client/src/types.ts"
    );
}

#[test]
fn the_fixture_covers_every_shown_text_kind() {
    let shown = samples();
    assert!(shown.iter().any(|s| s.notice.is_some()));
    assert!(shown.iter().any(|s| s.preview.is_some()));
    assert!(shown.iter().any(|s| s.name_problem.is_some()));
    assert!(shown.iter().any(|s| s
        .worktree_tooltip
        .as_ref()
        .is_some_and(|t| !t.args.is_empty())));
    let starts: std::collections::BTreeSet<_> =
        shown.iter().map(|s| s.start_text.source.clone()).collect();
    assert!(starts.len() >= 4, "{starts:?}");
}
