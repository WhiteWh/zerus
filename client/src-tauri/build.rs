#[path = "build_identity.rs"]
mod build_identity;

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned()
    })
}

fn canonical(path: &str) -> String {
    Path::new(path)
        .canonicalize()
        .map_or_else(|_| path.to_owned(), |p| p.to_string_lossy().into_owned())
}

/// `--build-info` and `--version` report Zerus' identity as the Qt GUI does:
/// the product version, the Dev number from `ZERUS_DEV_BUILD_NUMBER`, a local
/// preview label from `ZERUS_LOCAL_PREVIEW_LABEL` and the clean source commit
/// (`tray/cmake/BuildVersion.cmake`).
fn embed_identity() {
    let manifest =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let root = manifest
        .join("../..")
        .canonicalize()
        .expect("the repository root");
    let read = |name: &str| {
        println!("cargo:rerun-if-changed={}", root.join(name).display());
        std::fs::read_to_string(root.join(name)).unwrap_or_else(|error| panic!("{name}: {error}"))
    };
    let version =
        build_identity::release_version(&read("VERSION")).expect("VERSION names MAJOR.MINOR.PATCH");
    let dev_base = build_identity::release_version(&read("DEV_VERSION"))
        .unwrap_or_else(|| panic!("{}", build_identity::DEV_BASE_ERROR));
    build_identity::check_dev_base(&version, &dev_base).unwrap_or_else(|error| panic!("{error}"));
    println!("cargo:rerun-if-env-changed=ZERUS_DEV_BUILD_NUMBER");
    let number =
        build_identity::dev_number(&std::env::var("ZERUS_DEV_BUILD_NUMBER").unwrap_or_default())
            .unwrap_or_else(|error| panic!("{error}"));
    println!("cargo:rerun-if-env-changed=ZERUS_LOCAL_PREVIEW_LABEL");
    let label = build_identity::preview_label(
        &std::env::var("ZERUS_LOCAL_PREVIEW_LABEL").unwrap_or_default(),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let root_text = root.to_string_lossy().into_owned();
    let commit = git(&root, &["rev-parse", "HEAD"])
        .zip(git(&root, &["rev-parse", "--show-toplevel"]))
        .and_then(|(head, top)| build_identity::source_commit(&head, &canonical(&top), &root_text));
    // Qt embeds the commit only when `git status` answers too (`tray/CMakeLists.txt:130-139`).
    let (commit, dirty) = match commit.zip(git(
        &root,
        &["status", "--porcelain", "--untracked-files=normal"],
    )) {
        Some((commit, changes)) => (Some(commit), Some(!changes.is_empty())),
        None => (None, None),
    };
    if let Some(dir) = git(&root, &["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        println!("cargo:rerun-if-changed={dir}/index");
    }
    // A commit moves the branch HEAD names, not HEAD itself: watch its loose
    // ref and packed-refs, so the embedded commit never goes stale.
    let git_path = |name: &str| {
        git(
            &root,
            &["rev-parse", "--path-format=absolute", "--git-path", name],
        )
        .map(PathBuf::from)
    };
    if let Some(loose) =
        git(&root, &["symbolic-ref", "-q", "HEAD"]).and_then(|branch| git_path(&branch))
    {
        println!(
            "cargo:rerun-if-changed={}",
            build_identity::watched(&loose).display()
        );
    }
    if let Some(packed) = git_path("packed-refs").filter(|path| path.exists()) {
        println!("cargo:rerun-if-changed={}", packed.display());
    }
    build_identity::check_numbered(number, commit.as_deref(), dirty)
        .unwrap_or_else(|error| panic!("{error}"));
    build_identity::check_preview(label.as_deref(), number, commit.as_deref(), dirty)
        .unwrap_or_else(|error| panic!("{error}"));
    // Every value is set, empty when absent, so a variable of the same name in
    // the build's own environment never reaches `option_env!`.
    println!("cargo:rustc-env=ZERUS_VERSION={version}");
    println!("cargo:rustc-env=ZERUS_DEV_BASE_VERSION={dev_base}");
    println!(
        "cargo:rustc-env=ZERUS_DEV_BUILD_NUMBER={}",
        number.map(|number| number.to_string()).unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=ZERUS_LOCAL_PREVIEW_LABEL={}",
        label.unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=ZERUS_SOURCE_COMMIT={}",
        commit.unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=ZERUS_SOURCE_DIRTY={}",
        dirty
            .map(|dirty| u8::from(dirty).to_string())
            .unwrap_or_default()
    );
}

fn main() {
    embed_identity();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rerun-if-changed=src/macos_notification_log.c");
        cc::Build::new()
            .file("src/macos_notification_log.c")
            .compile("zerus_macos_notification_log");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Tauri's normal resource is linked only into application binaries.
        // Keep its icon/version there, and link the v6 activation manifest once
        // into every executable: rfd also imports TaskDialogIndirect in tests.
        let attributes = tauri_build::Attributes::new()
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
        tauri_build::try_build(attributes).expect("Tauri Windows resources");
        println!("cargo:rerun-if-changed=windows-controls.manifest");
        let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
            .join("windows-controls.manifest");
        let resource =
            PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("windows-controls.rc");
        let path = manifest.to_string_lossy().replace('\\', "\\\\");
        std::fs::write(
            &resource,
            format!("#pragma code_page(65001)\n1 24 \"{path}\"\n"),
        )
        .expect("write Common Controls resource");
        embed_resource::compile_for_everything(resource, embed_resource::NONE)
            .manifest_required()
            .expect("Common Controls v6 for all Windows targets");
    } else {
        tauri_build::build();
    }
}
