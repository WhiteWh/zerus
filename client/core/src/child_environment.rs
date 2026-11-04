//! Keep a local GUI's private WebKit libraries out of terminals and other applications.
use std::ffi::{OsStr, OsString};
use std::process::Command;

pub const WEBKIT_MARKER: &str = "HGS_LINUX_WEBKIT_ORIGINAL_ENV";

/// A normal child command, with the local Linux launcher's original environment.
/// WebKit creates its own subprocesses and continues to inherit the GUI's stack.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    restore(&mut command);
    command
}

/// Apply after any explicit environment overrides as well as at construction.
pub fn restore(command: &mut Command) {
    #[cfg(target_os = "linux")]
    {
        let marker = match command.get_envs().find(|(key, _)| *key == WEBKIT_MARKER) {
            Some((_, value)) => value.map(OsStr::to_owned),
            None => std::env::var_os(WEBKIT_MARKER),
        };
        if let Some(marker) = marker {
            for (key, value) in overrides(&marker) {
                match value {
                    Some(value) => {
                        command.env(key, value);
                    }
                    None => {
                        command.env_remove(key);
                    }
                }
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = command;
}

/// The same restoration for the Terminal's explicit pre-fork environment.
pub fn restored(base: impl IntoIterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    #[allow(unused_mut)] // Linux alone has a local WebKit override to restore.
    let mut env: Vec<_> = base.into_iter().collect();
    #[cfg(target_os = "linux")]
    if let Some(marker) = env
        .iter()
        .find(|(key, _)| key == WEBKIT_MARKER)
        .map(|(_, value)| value.clone())
    {
        for (key, value) in overrides(&marker) {
            env.retain(|(name, _)| name != key);
            if let Some(value) = value {
                env.push((key.into(), value));
            }
        }
    }
    env
}

#[cfg(target_os = "linux")]
fn overrides(marker: &OsStr) -> [(&'static str, Option<OsString>); 3] {
    use std::os::unix::ffi::OsStringExt;
    #[derive(serde::Deserialize)]
    struct Original {
        version: u8,
        #[serde(rename = "LD_LIBRARY_PATH")]
        libraries: Option<String>,
        #[serde(rename = "WEBKIT_EXEC_PATH")]
        processes: Option<String>,
    }
    let decode = |value: Option<String>| -> Option<OsString> {
        value
            .and_then(|value| {
                if !value.len().is_multiple_of(2) {
                    return None;
                }
                value
                    .as_bytes()
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| {
                        let text = std::str::from_utf8(pair).ok()?;
                        u8::from_str_radix(text, 16).ok()
                    })
                    .collect::<Option<Vec<_>>>()
            })
            .filter(|bytes| !bytes.contains(&0))
            .map(OsString::from_vec)
    };
    let original = marker
        .to_str()
        .filter(|text| text.len() <= 65536)
        .and_then(|text| serde_json::from_str::<Original>(text).ok())
        .filter(|original| original.version == 1);
    let (libraries, processes) = original
        .map(|original| (decode(original.libraries), decode(original.processes)))
        .unwrap_or_default();
    [
        ("LD_LIBRARY_PATH", libraries),
        ("WEBKIT_EXEC_PATH", processes),
        (WEBKIT_MARKER, None),
    ]
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn a_real_child_gets_original_values_and_no_marker() {
        for (libraries, processes, expected) in [
            (None, None, "unset|unset|unset"),
            (Some(""), Some(""), "set:|set:|unset"),
            (
                Some("2f6f726967696e616c2f6c6962"),
                Some("2f6f726967696e616c2f70726f636573736573"),
                "set:/original/lib|set:/original/processes|unset",
            ),
        ] {
            let marker = serde_json::json!({"version":1,"LD_LIBRARY_PATH":libraries,"WEBKIT_EXEC_PATH":processes}).to_string();
            let mut child = Command::new("/bin/sh");
            child.args(["-c", "printf '%s|%s|%s' \"${LD_LIBRARY_PATH+set:}${LD_LIBRARY_PATH-unset}\" \"${WEBKIT_EXEC_PATH+set:}${WEBKIT_EXEC_PATH-unset}\" \"${HGS_LINUX_WEBKIT_ORIGINAL_ENV-unset}\""])
                .env("LD_LIBRARY_PATH", "/private/gtk:/private/webkit")
                .env("WEBKIT_EXEC_PATH", "/private/processes")
                .env(WEBKIT_MARKER, marker);
            restore(&mut child);
            let output = child.output().unwrap();
            assert!(output.status.success());
            assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
        }
    }

    #[test]
    fn terminal_environment_restores_bytes_without_touching_other_values() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let env = restored([
            ("KEEP".into(), "value".into()),
            ("LD_LIBRARY_PATH".into(), "/private".into()),
            ("WEBKIT_EXEC_PATH".into(), "/private/processes".into()),
            (
                WEBKIT_MARKER.into(),
                r#"{"version":1,"LD_LIBRARY_PATH":"2f6c69622fff","WEBKIT_EXEC_PATH":null}"#.into(),
            ),
        ]);
        assert_eq!(
            env,
            vec![
                ("KEEP".into(), "value".into()),
                (
                    "LD_LIBRARY_PATH".into(),
                    OsString::from_vec(b"/lib/\xff".to_vec())
                )
            ]
        );
        assert_eq!(env[1].1.as_bytes(), b"/lib/\xff");
    }

    #[test]
    fn ordinary_environments_are_unchanged_and_bad_markers_drop_the_override() {
        let plain = vec![("LD_LIBRARY_PATH".into(), "/user/lib".into())];
        assert_eq!(restored(plain.clone()), plain);
        assert!(restored([
            ("LD_LIBRARY_PATH".into(), "/private".into()),
            (WEBKIT_MARKER.into(), "broken".into())
        ])
        .is_empty());
    }
}
