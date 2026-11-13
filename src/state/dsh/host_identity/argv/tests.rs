//! The shape a live host's argument vector must carry (zerus-ppd26.21).
use super::*;

fn argv(parts: &[&str]) -> Vec<Vec<u8>> {
    parts.iter().map(|part| part.as_bytes().to_vec()).collect()
}

const PATCH: &str = "/state/dsh/hosts/0123456789abcdef/hgs.patch.json";

fn shape(prefix: &[&str], patch: &str, profile: &str, extra: &[&str]) -> Vec<Vec<u8>> {
    let mut parts = prefix.to_vec();
    parts.extend(["--profile", profile]);
    if extra.first() == Some(&"--from-default-profile") {
        parts.extend(["--from-default-profile", "web"]);
    }
    parts.push("--patch");
    parts.push(patch);
    parts.extend(["--no-open", "--host", "127.0.0.1", "--port", "0"]);
    argv(&parts)
}

#[test]
fn the_writers_exact_shape_matches_after_a_node_or_launcher_prefix() {
    let state = Path::new("/state");
    assert!(host(
        &shape(&["node", "/opt/dsh/dsh.js"], PATCH, "hgs", &[]),
        state
    ));
    assert!(host(&shape(&["dsh"], PATCH, "hgs", &[]), state));
    assert!(host(
        &shape(
            [
                "systemd-run",
                "--user",
                "--scope",
                "--quiet",
                "--collect",
                "--expand-environment=no",
                "--unit",
                "hgs-dsh-1",
                "--",
                "dsh"
            ]
            .as_slice(),
            PATCH,
            "hgs",
            &[],
        ),
        state
    ));
    assert!(host(
        &shape(
            &["node", "/opt/dsh/dsh.js"],
            PATCH,
            "hgs",
            &["--from-default-profile"]
        ),
        state
    ));
    // The launcher may add its own arguments before `dsh`'s.
    assert!(host(
        &shape(
            &["node", "/opt/dsh/dsh.js", "--max-old-space-size=4096"],
            PATCH,
            "hgs",
            &[]
        ),
        state
    ));
}

#[test]
fn another_state_root_or_a_similar_command_line_never_matches() {
    let state = Path::new("/state");
    assert!(!host(
        &shape(
            &["dsh"],
            "/elsewhere/dsh/hosts/0123456789abcdef/hgs.patch.json",
            "hgs",
            &[]
        ),
        state
    ));
    assert!(!host(
        &shape(
            &["dsh"],
            "/state/other/hosts/0123456789abcdef/hgs.patch.json",
            "hgs",
            &[]
        ),
        state
    ));
    assert!(!host(&shape(&["dsh"], PATCH, "other", &[]), state));
    assert!(!host(&shape(&["dsh"], PATCH, "hgs", &[])[..9], state));
    // A bare process named dsh, or one with a similar path in another token,
    // must not count.
    assert!(!host(&argv(&["dsh"]), state));
    assert!(!host(&argv(&["dsh", "--version"]), state));
    assert!(!host(&argv(&["dsh", "--patch", PATCH, "--no-open"]), state));
    assert!(!host(&argv(&["dsh", &format!("--data={PATCH}")]), state));
    assert!(!host(
        &argv(&["dsh", "/state/dsh/hosts/0123456789abcdef/hgs.patch.json"]),
        state
    ));
    // The patch file name, the id's writer form and the tail are exact.
    for patch in [
        "/state/dsh/hosts/0123456789abcdef/not-hgs.patch.json",
        "/state/dsh/hosts/0123456789abcdef/xhgs.patch.json",
        "/state/dsh/hosts/0123456789abcde/hgs.patch.json",
        "/state/dsh/hosts/0123456789abcdeg/hgs.patch.json",
        "/state/dsh/hosts/0123456789ABCDEF/hgs.patch.json",
        "/state/dsh/hosts/../0123456789abcdef/hgs.patch.json",
        "/state/dsh/hosts/hgs.patch.json",
    ] {
        assert!(!host(&shape(&["dsh"], patch, "hgs", &[]), state), "{patch}");
    }
    let mut trailing = shape(&["dsh"], PATCH, "hgs", &[]);
    trailing.push(b"--resume".to_vec());
    assert!(
        !host(&trailing, state),
        "a different command line that embeds the shape"
    );
    let mut other_host = shape(&["dsh"], PATCH, "hgs", &[]);
    *other_host.last_mut().unwrap() = b"127.0.0.2".to_vec();
    assert!(!host(&other_host, state));
    let mut other_port = shape(&["dsh"], PATCH, "hgs", &[]);
    *other_port.last_mut().unwrap() = b"1".to_vec();
    assert!(!host(&other_port, state));
}

#[test]
fn the_reader_returns_only_argument_tokens() {
    // A real read of this process's own vector: the test binary is there and
    // no NUL padding leaks into a token.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let own = read(std::process::id()).expect("the test process's own argv");
        assert!(!own.is_empty());
        assert!(own.iter().all(|token| !token.contains(&0)));
    }
    assert_eq!(
        super::tokens(b"a\0b\0\0"),
        vec![b"a".to_vec(), b"b".to_vec()]
    );
}
