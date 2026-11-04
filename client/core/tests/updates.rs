use serde_json::{json, Value};
use zerus_client_core::updates::*;

fn raw() -> Value {
    json!({
        "schemaVersion": 1,
        "product": "zerus",
        "platform": "desktop",
        "channel": "stable",
        "versionName": "0.37.0",
        "releaseId": 407198248,
        "publishedAt": "2026-10-09T12:00:00Z",
        "sourceCommit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "releaseUrl": "https://github.com/ufna/zerus/releases/tag/v0.37.0",
        "packages": {"zerus-ade-bin": "0.37.0-1", "zerus-git": "0.37.0.r10.gaaaaaaa-1"},
        "artifacts": [{
            "kind": "arch-package",
            "name": "zerus-ade-bin-0.37.0-1-x86_64.pkg.tar.zst",
            "url": "https://zerus.dev/downloads/v0.37.0/zerus-ade-bin-0.37.0-1-x86_64.pkg.tar.zst",
            "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "size": 100,
            "os": "linux",
            "arch": "x86_64"
        }]
    })
}

fn stable_raw(version: &str) -> Value {
    let mut value = raw();
    let tag = format!("v{version}");
    let name = format!("zerus-ade-bin-{version}-1-x86_64.pkg.tar.zst");
    value["versionName"] = json!(version);
    value["releaseUrl"] = json!(format!("https://github.com/ufna/zerus/releases/tag/{tag}"));
    value["packages"] = json!({"zerus-ade-bin": format!("{version}-1")});
    value["artifacts"] = json!([{
        "kind": "arch-package", "name": name,
        "url": format!("https://zerus.dev/downloads/{tag}/{name}"),
        "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "size": 100, "os": "linux", "arch": "x86_64"
    }]);
    value
}

fn nightly_raw(version: &str) -> Value {
    let tag = "nightly-123";
    let name = format!("zerus-ade-nightly-bin-{version}-1-x86_64.pkg.tar.zst");
    let mut value = stable_raw(version);
    value["channel"] = json!("nightly");
    value["releaseUrl"] = json!(format!("https://github.com/ufna/zerus/releases/tag/{tag}"));
    value["packages"] = json!({"zerus-ade-nightly-bin": format!("{version}-1")});
    value["artifacts"] = json!([{
        "kind": "arch-package", "name": name,
        "url": format!("https://zerus.dev/downloads/{tag}/{name}"),
        "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "size": 100, "os": "linux", "arch": "x86_64"
    }]);
    value
}

fn dev_raw() -> Value {
    let mut value = raw();
    value["schemaVersion"] = json!(2);
    value["channel"] = json!("dev");
    value["channelTag"] = json!("dev");
    value["buildId"] = json!(12345);
    value["buildNumber"] = json!(50);
    value.as_object_mut().unwrap().remove("releaseId");
    value.as_object_mut().unwrap().remove("releaseUrl");
    value["buildUrl"] = json!("https://github.com/ufna/zerus/actions/runs/12345");
    value.as_object_mut().unwrap().remove("packages");
    value["artifacts"] = json!([
        {"kind": "arch-package", "name": "zerus.pkg.tar.zst", "url": "https://zerus.dev/downloads/dev-desktop-12345/zerus.pkg.tar.zst", "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "size": 100, "os": "linux", "arch": "x86_64"},
        {"kind": "source", "name": "zerus-source.tar.gz", "url": "https://zerus.dev/downloads/dev-desktop-12345/zerus-source.tar.gz", "sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc", "size": 100, "os": "any", "arch": "source"}
    ]);
    value
}

fn with_macos(mut value: Value) -> Value {
    let version = value["versionName"].as_str().unwrap();
    let prefix = if value["channel"] == "dev" {
        format!(
            "https://zerus.dev/downloads/dev-desktop-{}/",
            value["buildId"].as_u64().unwrap()
        )
    } else {
        value["releaseUrl"]
            .as_str()
            .unwrap()
            .replace("/tag/", "/download/")
            + "/"
    };
    let archive = format!("zerus-{version}-macos-arm64.zip");
    let image = format!("zerus-{version}-macos-arm64.dmg");
    let downloads = [&archive, &image].map(|name| json!({"name": name, "url": format!("{prefix}{name}"), "size": 100, "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}));
    value["macos"] = json!({
        "asset": archive,
        "minimum_system_version": "14.0",
        "qt_version": "6.11.2",
        "team_id": "ABCDE12345",
        "notarization_id": "0123abcd-4567-89ef-0123-456789abcdef",
        "dmg": {"asset": image, "notarization_id": "1123abcd-4567-89ef-0123-456789abcdef"},
        "downloads": downloads
    });
    value
}

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

#[test]
fn stable_nightly_and_dev_catalogs_are_verified() {
    let stable = parse_feed(&bytes(&raw()), Channel::Stable);
    assert!(stable.valid);
    assert_eq!(stable.version, "0.37.0");
    assert_eq!(stable.packages["zerus-git"], "0.37.0.r10.gaaaaaaa-1");
    assert_eq!(stable.release_id, 407198248);
    let nightly = parse_feed(
        &bytes(&nightly_raw("0.38.0.r10.gaaaaaaa.n5")),
        Channel::Nightly,
    );
    assert!(nightly.valid);
    assert_eq!(nightly.version, "0.38.0.r10.gaaaaaaa.n5");
    let legacy = parse_feed(&bytes(&dev_raw()), Channel::Dev);
    assert!(legacy.valid);
    assert_eq!(legacy.version, "0.37.0-dev.50");
    assert_eq!(legacy.build_number, 50);
    assert_eq!(legacy.release_id, 12345);
    assert_eq!(
        legacy.source_url,
        "https://zerus.dev/downloads/dev-desktop-12345/zerus-source.tar.gz"
    );
    assert_eq!(
        legacy.package_url,
        "https://zerus.dev/downloads/dev-desktop-12345/zerus.pkg.tar.zst"
    );
    assert!(dev_build_is_newer(&legacy, 49, &"d".repeat(40)));
    assert!(!dev_build_is_newer(&legacy, 51, &"d".repeat(40)));
    assert!(!dev_build_is_newer(&legacy, 0, &"a".repeat(40)));
    assert!(dev_build_is_newer(&legacy, 0, ""));
    let mut numbered = dev_raw();
    numbered["versionName"] = json!("0.42.1-dev.50");
    assert_eq!(
        parse_feed(&bytes(&numbered), Channel::Dev).version,
        "0.42.1-dev.50"
    );
    for bad in ["0.42.1-dev.49", "0.42.1-dev.050", "0.42.1-dev.51"] {
        numbered["versionName"] = json!(bad);
        assert!(!parse_feed(&bytes(&numbered), Channel::Dev).valid, "{bad}");
    }
    assert!(!parse_feed(&bytes(&dev_raw()), Channel::Stable).valid);
}

#[test]
fn macos_assets_and_downloads_are_verified() {
    let stable = parse_feed(&bytes(&with_macos(raw())), Channel::Stable);
    assert!(stable.valid);
    assert_eq!(
        stable.macos_zip_url,
        "https://github.com/ufna/zerus/releases/download/v0.37.0/zerus-0.37.0-macos-arm64.zip"
    );
    assert_eq!(
        stable.macos_dmg_url,
        "https://github.com/ufna/zerus/releases/download/v0.37.0/zerus-0.37.0-macos-arm64.dmg"
    );
    let mut dev_value = dev_raw();
    dev_value["versionName"] = json!("0.42.1-dev.50");
    let dev = parse_feed(&bytes(&with_macos(dev_value)), Channel::Dev);
    assert!(dev.valid);
    assert_eq!(
        dev.macos_dmg_url,
        "https://zerus.dev/downloads/dev-desktop-12345/zerus-0.42.1-dev.50-macos-arm64.dmg"
    );
    let mut bad = with_macos(raw());
    bad["macos"]["downloads"][0]["sha256"] = json!("not-a-hash");
    assert!(!parse_feed(&bytes(&bad), Channel::Stable).valid);
    let mut missing = with_macos(raw());
    missing["macos"]["downloads"] = json!([]);
    assert!(!parse_feed(&bytes(&missing), Channel::Stable).valid);
    let mut wrong = with_macos(dev_raw());
    wrong["macos"]["team_id"] = json!("lowercase1");
    assert!(!parse_feed(&bytes(&wrong), Channel::Dev).valid);
}

#[test]
fn malformed_catalogs_are_refused() {
    for key in [
        "schemaVersion",
        "product",
        "platform",
        "channel",
        "releaseUrl",
        "sourceCommit",
        "publishedAt",
    ] {
        let mut bad = raw();
        bad[key] = if key == "schemaVersion" {
            json!(2)
        } else {
            json!("evil")
        };
        assert!(!parse_feed(&bytes(&bad), Channel::Stable).valid, "{key}");
    }
    for key in ["name", "url", "sha256", "size", "os", "arch", "kind"] {
        let mut bad = raw();
        bad["artifacts"][0][key] = if key == "size" {
            json!(1e30)
        } else {
            json!("../evil")
        };
        assert!(!parse_feed(&bytes(&bad), Channel::Stable).valid, "{key}");
    }
    let mut dirty = raw();
    dirty["sourceDirty"] = json!(false);
    assert!(!parse_feed(&bytes(&dirty), Channel::Stable).valid);
    let mut duplicate = raw();
    duplicate["artifacts"] = json!([
        duplicate["artifacts"][0].clone(),
        duplicate["artifacts"][0].clone()
    ]);
    assert!(!parse_feed(&bytes(&duplicate), Channel::Stable).valid);
    assert!(!parse_feed(&vec![b' '; CATALOG_LIMIT + 1], Channel::Stable).valid);
    let mut missing = raw();
    missing.as_object_mut().unwrap().remove("artifacts");
    assert!(!parse_feed(&bytes(&missing), Channel::Stable).valid);
    assert!(!parse_feed(&bytes(&raw()), Channel::Nightly).valid);
    for key in [
        "buildId",
        "buildNumber",
        "channelTag",
        "buildUrl",
        "sourceCommit",
    ] {
        let mut bad = dev_raw();
        bad[key] = json!("bad");
        assert!(!parse_feed(&bytes(&bad), Channel::Dev).valid, "{key}");
    }
    let mut dirty = dev_raw();
    dirty["sourceDirty"] = json!(false);
    assert!(!parse_feed(&bytes(&dirty), Channel::Dev).valid);
}

#[test]
fn installation_rules_match_qt() {
    let package = Installation {
        kind: "package".into(),
        package: "zerus-ade-bin".into(),
        version: "0.36.2-1".into(),
        arch: true,
        helper: "paru".into(),
    };
    assert_eq!(
        update_command(&package, Channel::Stable),
        "paru -Syu zerus-ade-bin"
    );
    assert_eq!(
        update_command(
            &Installation {
                package: "zerus-git".into(),
                ..package.clone()
            },
            Channel::Stable
        ),
        "paru -Syu --devel"
    );
    assert_eq!(
        update_command(
            &Installation {
                kind: "source".into(),
                ..package.clone()
            },
            Channel::Stable
        ),
        "paru -Syu zerus-ade-bin"
    );
    assert_eq!(update_command(&package, Channel::Dev), "");
    let feed = parse_feed(&bytes(&raw()), Channel::Stable);
    assert!(
        comparison_message(&package, &feed, "0.9.0", 1, true, "linux")
            .english()
            .contains("Update available")
    );
    assert!(
        comparison_message(&package, &feed, "0.9.0", -1, true, "linux")
            .english()
            .contains("at least")
    );
    assert!(
        comparison_message(&package, &feed, "0.9.0", 0, false, "linux")
            .english()
            .contains("No matching")
    );
    let cask = Installation {
        kind: "homebrew-cask".into(),
        package: "zerus".into(),
        version: "0.36.2".into(),
        arch: false,
        helper: String::new(),
    };
    assert_eq!(
        update_command(&cask, Channel::Stable),
        "brew upgrade --cask zerus"
    );
    assert_eq!(
        update_command(&cask, Channel::Nightly),
        "brew uninstall --cask zerus && brew install --cask ufna/zerus/zerus@nightly"
    );
    let winget = Installation {
        kind: "portable".into(),
        package: "ufna.Zerus".into(),
        version: String::new(),
        arch: false,
        helper: String::new(),
    };
    assert_eq!(
        update_command(&winget, Channel::Stable),
        "winget upgrade --id ufna.Zerus --exact"
    );
    assert_eq!(update_command(&winget, Channel::Nightly), "");
    let guide = installation_guide(&winget, Channel::Stable, &feed, "0.36.2");
    assert_eq!(
        guide.command.as_ref().map(|t| t.english()).as_deref(),
        Some("winget: winget upgrade --id ufna.Zerus --exact")
    );
    assert_eq!(
        guide.copy_name.as_ref().map(|t| t.english()).as_deref(),
        Some("Copy winget command")
    );
    assert!(guide
        .instructions
        .as_ref()
        .is_some_and(|t| t.english().contains("keep running")));
    let nightly = parse_feed(
        &bytes(&nightly_raw("0.38.0.r10.gaaaaaaa.n5")),
        Channel::Nightly,
    );
    assert!(portable_update_available("0.37.0", &nightly));
    assert!(!portable_update_available("0.38.0", &nightly));
    assert!(stable_has_windows_build("0.42.3"));
    assert!(!stable_has_windows_build("0.42.2"));
    assert_eq!(comparison_message(&winget, &nightly, "0.37.0", 0, false, "windows").english(), "Zerus nightly 0.38.0.r10.gaaaaaaa.n5 is available. Download its Windows zip from the release page.");
    assert_eq!(automatic_interval(Channel::Dev), 15 * 60);
    assert_eq!(automatic_interval(Channel::Stable), 24 * 60 * 60);
    assert!(is_update_token("zerus-update:v2:dev:12345"));
    assert!(!is_update_token("zerus-update:v1:dev:12345"));
    assert!(!is_update_token("zerus-update:v2:stable:12345"));
    assert_eq!(
        update_token(
            Channel::Dev,
            &parse_feed(&bytes(&dev_raw()), Channel::Dev),
            true
        ),
        "zerus-update:v2:dev:12345"
    );
}

#[test]
fn bundle_and_installer_rules_are_checked() {
    let record = br#"{"schema":1,"kind":"installer","product":"Zerus"}"#;
    assert!(installer_record(record));
    assert!(!installer_record(
        br#"{"schema":2,"kind":"installer","product":"Zerus"}"#
    ));
    assert!(!installer_record(&vec![b'x'; INSTALLER_RECORD_LIMIT + 1]));
    assert_eq!(winget_package("C:\\Users\\example\\AppData\\Local\\Microsoft\\WinGet\\Packages\\ufna.Zerus_Microsoft.Winget.Source_8wekyb3d8bbwe\\zerus-windows-x86_64\\hgs-tray.exe"), "ufna.Zerus");
    assert_eq!(winget_package("C:\\Tools\\other\\hgs-tray.exe"), "");
    let directory = std::env::temp_dir().join(format!("zerus-update-test-{}", std::process::id()));
    let contents = directory.join("Contents");
    std::fs::create_dir_all(&contents).unwrap();
    let plist = contents.join("Info.plist");
    std::fs::write(&plist, "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>ZerusReleaseVersion</key><string>0.47.1-dev.1500</string></dict></plist>").unwrap();
    assert_eq!(bundle_release(&plist), "0.47.1-dev.1500");
    let bundle = contents.join("MacOS").join("hgs-tray");
    assert!(bundle_replaced(&bundle, "0.47.1-dev.1499"));
    assert!(!bundle_replaced(&bundle, "0.47.1-dev.1500"));
    assert!(packaged_bundle_replaced(&bundle, "0.47.1-dev.1499"));
    std::fs::write(&plist, "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>CFBundleShortVersionString</key><string>0.47.1</string></dict></plist>").unwrap();
    assert_eq!(bundle_release(&plist), "0.47.1");
    assert_eq!(packaged_bundle_release(&plist), "");
    assert!(!packaged_bundle_replaced(&bundle, "0.47.1-dev.1499"));
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[test]
fn homebrew_and_bundle_installations_require_the_exact_bundle() {
    let directory =
        std::env::temp_dir().join(format!("zerus-update-homebrew-test-{}", std::process::id()));
    let prefix = directory.join("prefix");
    let app = directory.join("Applications/Zerus.app");
    let contents = app.join("Contents");
    let executable = contents.join("MacOS/hgs-tray");
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::write(&executable, "").unwrap();
    let plist = |release: &str| {
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>com.hgdev.hgs-tray</string><key>CFBundleExecutable</key><string>hgs-tray</string><key>CFBundleName</key><string>Zerus</string><key>CFBundleShortVersionString</key><string>0.47.0</string>{release}</dict></plist>")
    };
    std::fs::write(
        contents.join("Info.plist"),
        plist("<key>ZerusReleaseVersion</key><string>0.47.0</string>"),
    )
    .unwrap();
    let owner = prefix.join("Caskroom/zerus/0.47.0");
    std::fs::create_dir_all(&owner).unwrap();
    std::os::unix::fs::symlink(&app, owner.join("Zerus.app")).unwrap();
    let cask = homebrew_installation(&executable, &[&prefix]);
    assert_eq!(cask.kind, "homebrew-cask");
    assert_eq!(cask.package, "zerus");
    assert_eq!(cask.version, "0.47.0");
    let bundle = macos_bundle_installation(&executable);
    assert_eq!(bundle.kind, "macos-bundle");
    assert_eq!(bundle.version, "0.47.0");
    assert!(
        homebrew_installation(&executable, &[&directory.join("other")])
            .kind
            .is_empty()
    );
    std::fs::write(
        contents.join("Info.plist"),
        plist("").replace("com.hgdev.hgs-tray", "com.hgdev.hgs-tray.local"),
    )
    .unwrap();
    assert!(macos_bundle_installation(&executable).kind.is_empty());
    std::fs::remove_dir_all(directory).unwrap();
}
