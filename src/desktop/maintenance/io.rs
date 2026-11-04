//! Read-only published-catalog discovery. No download, install or notification.
use super::{spawn, unix_now, Lease};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use zerus_client_core::{
    maintenance::{UpdateConfiguration, UpdateProjection},
    updates::{self, Channel, Feed, Installation},
};

fn find_program(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|p| {
                p.join(if cfg!(windows) {
                    format!("{name}.exe")
                } else {
                    name.into()
                })
            })
            .find(|p| p.is_file())
    })
}
fn run(program: &Path, args: &[&str], cancel: &AtomicBool, lease: Arc<Lease>) -> Option<String> {
    let invocation = zerus_client_core::Invocation {
        args: args.iter().map(|s| (*s).into()).collect(),
        stdin: Some(Vec::new()),
        timeout: Duration::from_secs(5),
        request_id: None,
    };
    let output =
        zerus_client_core::run::run_owned(program, &invocation, &[], cancel, spawn, lease).ok()?;
    if output.len() > 32 * 1024 {
        return None;
    }
    Some(String::from_utf8(output).ok()?.trim().to_owned())
}
fn valid_package(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 120
        && text
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && text
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'@' | b'.' | b'_' | b'+' | b'-'))
}
fn linux_facts(installation: &mut Installation) {
    if !cfg!(target_os = "linux") {
        return;
    }
    let mut raw = Vec::new();
    installation.arch = std::fs::File::open("/etc/os-release")
        .ok()
        .is_some_and(|f| {
            f.take(64 * 1024 + 1).read_to_end(&mut raw).is_ok()
                && raw.len() <= 64 * 1024
                && updates::is_arch_linux(&raw)
        });
    installation.helper = find_program("yay")
        .map(|_| "yay")
        .or_else(|| find_program("paru").map(|_| "paru"))
        .unwrap_or("")
        .into();
}
fn source_installation(executable: &Path) -> Installation {
    let mut result = Installation::default();
    let mut directory = executable.parent().unwrap_or(executable);
    let windows_bundle = cfg!(windows) && directory.join("build-info.json").is_file();
    for _ in 0..20 {
        if !windows_bundle && directory.join(".git").exists() {
            result.kind = "source".into();
            break;
        }
        let Some(parent) = directory.parent() else {
            break;
        };
        directory = parent;
    }
    if cfg!(windows) && result.kind.is_empty() {
        let record = executable
            .parent()
            .unwrap_or(executable)
            .join("installation.json");
        result.kind = if updates::installer_record_path(&record) {
            "installer"
        } else {
            "portable"
        }
        .into();
        if result.kind == "portable" {
            result.package = updates::winget_package(&executable.to_string_lossy());
        }
    }
    if result.kind.is_empty() {
        result.kind = "unknown".into();
    }
    result
}
fn installation(executable: &Path, cancel: &AtomicBool, lease: Arc<Lease>) -> Installation {
    let Ok(canonical) = executable.canonicalize() else {
        let mut unknown = Installation {
            kind: "unknown".into(),
            ..Installation::default()
        };
        linux_facts(&mut unknown);
        return unknown;
    };
    let mut result = if cfg!(target_os = "macos") {
        let brew = updates::homebrew_installation(
            &canonical,
            &[Path::new("/opt/homebrew"), Path::new("/usr/local")],
        );
        if updates::macos_installation(&brew) {
            brew
        } else {
            updates::macos_bundle_installation(&canonical)
        }
    } else {
        Installation::default()
    };
    if cfg!(target_os = "linux") {
        linux_facts(&mut result);
        if let Some(pacman) = find_program("pacman") {
            if let Some(owner) = run(
                &pacman,
                &["-Qqo", "--", &canonical.to_string_lossy()],
                cancel,
                lease.clone(),
            )
            .filter(|s| valid_package(s))
            {
                if let Some(raw) = run(&pacman, &["-Q", "--", &owner], cancel, lease) {
                    if let Some(version) = raw.strip_prefix(&format!("{owner} ")).filter(|v| {
                        !v.is_empty()
                            && v.len() <= 160
                            && v.bytes().next().is_some_and(|c| c.is_ascii_digit())
                            && v.bytes().all(|c| {
                                c.is_ascii_alphanumeric()
                                    || matches!(c, b'.' | b'+' | b'_' | b'~' | b':' | b'-')
                            })
                    }) {
                        result.version = version.into();
                    }
                }
                result.kind = "package".into();
                result.package = owner;
                return result;
            }
        }
    }
    if result.kind.is_empty() {
        let mut source = source_installation(&canonical);
        source.arch = result.arch;
        source.helper = result.helper;
        return source;
    }
    result
}
fn fetch(channel: Channel) -> Result<Feed, &'static str> {
    use ureq::tls::{RootCerts, TlsConfig};
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .max_redirects(0)
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .new_agent();
    let schema = if channel == Channel::Dev { "v2" } else { "v1" };
    let response = agent
        .get(format!(
            "https://zerus.dev/updates/{schema}/desktop/{}.json",
            channel.as_str()
        ))
        .call()
        .map_err(|_| "Check failed. Try again later.")?;
    if response.status() != 200 {
        return Err("Check failed. Try again later.");
    }
    let mut raw = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(updates::CATALOG_LIMIT as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| "Check failed. Try again later.")?;
    if raw.len() > updates::CATALOG_LIMIT {
        return Err("Invalid release catalog.");
    }
    let feed = updates::parse_feed(&raw, channel);
    if !feed.valid {
        return Err("Invalid release catalog.");
    }
    Ok(feed)
}
pub(super) fn update(
    config: &UpdateConfiguration,
    cancel: &AtomicBool,
    lease: Arc<Lease>,
) -> UpdateProjection {
    let mut view = UpdateProjection {
        channel: Some(config.channel),
        automatic: config.automatic,
        ..UpdateProjection::default()
    };
    if cancel.load(Ordering::Acquire) {
        return view;
    }
    view.installation = installation(Path::new(&config.gui.executable), cancel, lease.clone());
    if cancel.load(Ordering::Acquire) {
        return view;
    }
    match fetch(config.channel) {
        Ok(feed) => {
            view.feed = feed;
            view.last_success = unix_now();
            view.ready = true;
        }
        Err(error) => {
            view.error = Some(error.into());
            return view;
        }
    }
    let compared = if view.installation.kind == "package"
        && view.installation.package != "zerus-git"
        && !view.installation.version.is_empty()
    {
        view.feed
            .packages
            .get(&view.installation.package)
            .and_then(|released| {
                find_program("vercmp")
                    .and_then(|p| run(&p, &[released, &view.installation.version], cancel, lease))
            })
            .and_then(|raw| raw.parse::<i32>().ok())
    } else {
        None
    };
    (view.available, view.comparison, view.compared) =
        availability(config, &view.feed, &view.installation, compared);
    view
}
fn availability(
    config: &UpdateConfiguration,
    feed: &Feed,
    installation: &Installation,
    compared: Option<i32>,
) -> (bool, i32, bool) {
    if updates::macos_installation(installation) {
        return (
            updates::macos_update_available(installation, feed),
            0,
            false,
        );
    }
    if config.channel == Channel::Dev {
        return (
            !updates::windows_build(installation)
                && updates::dev_build_is_newer(
                    feed,
                    config.gui.dev_number,
                    &config.gui.source_commit,
                ),
            0,
            false,
        );
    }
    if installation.kind != "package" {
        return (
            if updates::windows_build(installation) {
                updates::portable_update_available(&config.gui.app_version, feed)
            } else {
                config.channel == Channel::Stable
                    && updates::plain_stable_is_newer(&config.gui.app_version, &feed.version)
            },
            0,
            false,
        );
    }
    if installation.package == "zerus-git"
        || installation.version.is_empty()
        || !feed.packages.contains_key(&installation.package)
    {
        return (false, 0, false);
    }
    compared
        .map(|n| (n > 0, n, true))
        .unwrap_or((false, 0, false))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn available_compares_native_gui_build_not_hgs_build() {
        let gui = zerus_client_core::maintenance::GuiIdentity {
            bundle_id: "com.hgdev.hgs-tray".into(),
            executable: std::env::temp_dir()
                .join("hgs-tray")
                .to_string_lossy()
                .into(),
            app_version: "1.0.0".into(),
            dev_number: 10,
            source_commit: "a".repeat(40),
        };
        let mut config = UpdateConfiguration {
            gui,
            channel: Channel::Dev,
            automatic: true,
        };
        let feed = Feed {
            valid: true,
            build_number: 11,
            source_commit: "b".repeat(40),
            version: "1.1.0".into(),
            ..Feed::default()
        };
        assert!(availability(&config, &feed, &Installation::default(), None).0);
        config.gui.dev_number = 12;
        assert!(!availability(&config, &feed, &Installation::default(), None).0);
        let vcs = Installation {
            kind: "package".into(),
            package: "zerus-git".into(),
            ..Installation::default()
        };
        config.channel = Channel::Stable;
        assert!(!availability(&config, &feed, &vcs, Some(1)).0);
    }
}
