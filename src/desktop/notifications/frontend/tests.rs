use super::*;
use std::{
    os::unix::fs::{symlink, PermissionsExt},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
struct Fixture {
    _root: tempfile::TempDir,
    prefix: PathBuf,
    cli: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let prefix = root
            .path()
            .canonicalize()
            .unwrap()
            .join("installation with spaces");
        fs::create_dir_all(prefix.join("bin")).unwrap();
        fs::set_permissions(&prefix, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(prefix.join("bin"), fs::Permissions::from_mode(0o755)).unwrap();
        let cli = prefix.join("bin/hgs");
        fs::write(&cli, b"private service-selection fixture").unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
        fs::copy("/bin/cat", prefix.join("bin/hgs-tray")).unwrap();
        Self {
            _root: root,
            prefix,
            cli,
        }
    }
    fn select(&self) -> SelectedFrontend {
        // Data-only install layout. No genuine installed service/GUI claim.
        SelectedFrontend::capture(
            self.cli.clone(),
            self.prefix.join("bin/hgs-tray"),
            "a".repeat(64),
            Origin::Fixture,
        )
        .unwrap()
    }
    fn wrapper(&self) {
        fs::create_dir_all(self.prefix.join("share/hgs")).unwrap();
        fs::create_dir_all(self.prefix.join("libexec")).unwrap();
        fs::set_permissions(
            self.prefix.join("share/hgs"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::set_permissions(
            self.prefix.join("libexec"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::write(
            self.prefix.join("share/hgs/local-webkit.json"),
            b"{\"version\":1}",
        )
        .unwrap();
        let wrapper = self.prefix.join("libexec/hgs-tray-launch");
        // Private exec fixture only; never runs Python, WebKit or the GUI.
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexec \"{}\"\n",
                self.prefix.join("bin/hgs-tray").display()
            ),
        )
        .unwrap();
        fs::set_permissions(wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
fn spawn(path: &Path) -> Child {
    Command::new(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}
fn stop(mut child: Child) {
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
}
fn observed(selection: &SelectedFrontend, child: &mut Child) -> ChildObservation {
    let until = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(value) = selection.observe_original_child(child).unwrap() {
            return value;
        }
        assert!(
            Instant::now() < until,
            "original fixture child did not exec"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn exact_held_image_survives_directory_churn_but_refuses_replacement() {
    let f = Fixture::new();
    let selection = f.select();
    assert_eq!(selection.image(), f.prefix.join("bin/hgs-tray"));
    assert_eq!(selection.entry(), selection.image());
    fs::write(f.prefix.join("other"), b"unrelated").unwrap();
    fs::write(f.prefix.join("bin/other"), b"unrelated").unwrap();
    selection.revalidate().unwrap();
    let replacement = f.prefix.join("bin/replacement");
    fs::copy("/bin/cat", &replacement).unwrap();
    fs::rename(replacement, selection.image()).unwrap();
    assert!(selection.revalidate().is_err());
}
#[test]
fn file_mutation_and_foreign_permissions_refuse_without_a_new_selection() {
    let f = Fixture::new();
    let selection = f.select();
    fs::write(selection.image(), b"changed").unwrap();
    assert!(selection.revalidate().is_err());
    fs::set_permissions(selection.image(), fs::Permissions::from_mode(0o777)).unwrap();
    assert!(SelectedFrontend::capture(
        f.cli.clone(),
        f.prefix.join("bin/hgs-tray"),
        "a".repeat(64),
        Origin::Fixture
    )
    .is_err());
}
#[test]
fn symlink_image_or_service_path_and_missing_sibling_never_find_a_fallback() {
    let f = Fixture::new();
    fs::remove_file(f.prefix.join("bin/hgs-tray")).unwrap();
    assert!(SelectedFrontend::capture(
        f.cli.clone(),
        f.prefix.join("bin/hgs-tray"),
        "a".repeat(64),
        Origin::Fixture
    )
    .is_err());
    symlink("/bin/cat", f.prefix.join("bin/hgs-tray")).unwrap();
    assert!(SelectedFrontend::capture(
        f.cli.clone(),
        f.prefix.join("bin/hgs-tray"),
        "a".repeat(64),
        Origin::Fixture
    )
    .is_err());
    fs::remove_file(f.prefix.join("bin/hgs-tray")).unwrap();
    fs::copy("/bin/cat", f.prefix.join("bin/hgs-tray")).unwrap();
    fs::rename(&f.cli, f.prefix.join("bin/original-service")).unwrap();
    symlink(f.prefix.join("bin/original-service"), &f.cli).unwrap();
    assert!(SelectedFrontend::capture(
        f.cli.clone(),
        f.prefix.join("bin/hgs-tray"),
        "a".repeat(64),
        Origin::Fixture
    )
    .is_err());
}
#[test]
fn maintained_wrapper_and_its_selection_are_part_of_the_same_custody() {
    let f = Fixture::new();
    let stock = f.select();
    f.wrapper();
    assert!(
        stock.revalidate().is_err(),
        "new local launcher selection is not ignored"
    );
    let selected = f.select();
    assert_eq!(selected.entry(), f.prefix.join("libexec/hgs-tray-launch"));
    let mut child = spawn(selected.entry());
    let observation = observed(&selected, &mut child);
    assert_eq!(observation.pid(), child.id());
    assert!(!observation.birth().is_empty());
    selected.revalidate_child(&mut child, &observation).unwrap();
    stop(child);
    fs::write(
        f.prefix.join("share/hgs/local-webkit.json"),
        b"changed selection",
    )
    .unwrap();
    assert!(selected.revalidate().is_err());
}
#[test]
fn original_child_is_required_and_cached_exit_cannot_adopt_a_pid() {
    let f = Fixture::new();
    let selected = f.select();
    let mut foreign = spawn(Path::new("/bin/cat"));
    assert!(selected
        .observe_original_child(&mut foreign)
        .unwrap()
        .is_none());
    stop(foreign);
    let mut child = spawn(selected.entry());
    let observation = observed(&selected, &mut child);
    selected.revalidate_child(&mut child, &observation).unwrap();
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    assert!(selected.observe_original_child(&mut child).is_err());
    assert!(selected.revalidate_child(&mut child, &observation).is_err());
}
#[test]
fn installation_directory_replacement_and_wrapper_mutation_refuse() {
    let f = Fixture::new();
    f.wrapper();
    let selection = f.select();
    fs::write(selection.entry(), b"changed wrapper").unwrap();
    assert!(selection.revalidate().is_err());
    let selected = f.select();
    fs::rename(f.prefix.join("bin"), f.prefix.join("old-bin")).unwrap();
    fs::create_dir(f.prefix.join("bin")).unwrap();
    fs::copy(f.prefix.join("old-bin/hgs"), &f.cli).unwrap();
    fs::copy(
        f.prefix.join("old-bin/hgs-tray"),
        f.prefix.join("bin/hgs-tray"),
    )
    .unwrap();
    assert!(selected.revalidate().is_err());
}

#[test]
fn service_and_frontend_can_have_distinct_maintained_prefixes() {
    let f = Fixture::new();
    let service = f._root.path().join("separate-service/hgs");
    fs::create_dir_all(service.parent().unwrap()).unwrap();
    fs::copy(&f.cli, &service).unwrap();
    let selected = SelectedFrontend::capture(
        service.clone(),
        f.prefix.join("bin/hgs-tray"),
        "a".repeat(64),
        Origin::Fixture,
    )
    .unwrap();
    assert_eq!(selected.service_image(), service);
    assert_eq!(selected.configuration(), "a".repeat(64));
    assert_eq!(selected.image(), f.prefix.join("bin/hgs-tray"));
    selected.revalidate().unwrap();
}

#[test]
fn private_group_requires_exact_nonempty_names_and_no_listed_members() {
    assert!(private_group_record(c"fixture", c"fixture", true));
    assert!(!private_group_record(c"fixture", c"shared", true));
    assert!(!private_group_record(c"fixture", c"fixture", false));
    assert!(!private_group_record(c"", c"", true));
    assert!(!private_group(unsafe { libc::getegid() }.wrapping_add(1)));
}

#[test]
fn group_writable_installation_uses_only_the_actual_effective_private_group() {
    let f = Fixture::new();
    f.wrapper();
    // Explicit modes reproduce umask 002 without changing process-global umask.
    for path in [
        f.prefix.clone(),
        f.prefix.join("bin"),
        f.prefix.join("share/hgs"),
        f.prefix.join("libexec"),
        f.cli.clone(),
        f.prefix.join("bin/hgs-tray"),
        f.prefix.join("libexec/hgs-tray-launch"),
    ] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o775)).unwrap();
    }
    fs::set_permissions(
        f.prefix.join("share/hgs/local-webkit.json"),
        fs::Permissions::from_mode(0o664),
    )
    .unwrap();
    let selected = SelectedFrontend::capture(
        f.cli.clone(),
        f.prefix.join("bin/hgs-tray"),
        "a".repeat(64),
        Origin::Fixture,
    );
    // NSS belongs to the host: on a user-private-group account this is a real
    // positive; shared effective groups must retain the same refusal in CI.
    if private_group(unsafe { libc::getegid() }) {
        let selected = selected.unwrap();
        selected.revalidate().unwrap();
        assert_eq!(selected.entry(), f.prefix.join("libexec/hgs-tray-launch"));
        fs::set_permissions(f.prefix.join("bin"), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(selected.revalidate().is_err());
    } else {
        assert!(selected.is_err(), "shared effective group is not private");
    }
    fs::set_permissions(&f.prefix, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(SelectedFrontend::capture(
        f.cli.clone(),
        f.prefix.join("bin/hgs-tray"),
        "a".repeat(64),
        Origin::Fixture
    )
    .is_err());
}
