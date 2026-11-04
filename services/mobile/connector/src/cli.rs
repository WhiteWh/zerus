//! The command line. Options are spelled exactly: argparse also accepted
//! unambiguous prefixes such as `--conf` (README divergence j). Like argparse,
//! a repeated option keeps its last value, `--config=PATH` works, and any
//! usage error exits 2.
use std::ffi::{OsStr, OsString};

use std::path::PathBuf;

pub const USAGE: &str =
    "usage: zerus-mobile-connector [-h] --config CONFIG [--allow-insecure-localhost] [--check [--json]] [--version]";

pub const HELP: &str =
    "Outbound-only Zerus mobile connector. Native hgs owns agent identity and input validation.

options:
  -h, --help                  show this help message and exit
  --config CONFIG             the private connector.json
  --allow-insecure-localhost  allow plain HTTP to a loopback relay (development)
  --check                     report on the configuration and journal read-only, then exit
  --json                      with --check, print the report as JSON
  --version                   print the version and exit

Workspace stop ABI: service-preflight-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run {
        config: PathBuf,
        allow_insecure_localhost: bool,
    },
    Check {
        config: PathBuf,
        allow_insecure_localhost: bool,
        json: bool,
    },
    Version,
    Help,
}

/// argparse takes a following argument as the option's value unless it looks
/// like an option: it starts with `-`, is not `-` itself or a negative number,
/// and has no space.
fn config_argument(argument: &OsStr, value: &[u8]) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let _ = argument;
        OsStr::from_bytes(value).to_os_string()
    }
    #[cfg(windows)]
    {
        let _ = value;
        // The ASCII prefix is a valid boundary; keep all UTF-16 path units.
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        OsString::from_wide(&argument.encode_wide().skip(9).collect::<Vec<_>>())
    }
}

fn is_value(argument: &OsStr) -> bool {
    let bytes = argument.as_encoded_bytes();
    let negative = |text: &[u8]| {
        let digits = &text[1..];
        let (whole, fraction) = match digits.iter().position(|&b| b == b'.') {
            Some(dot) => (&digits[..dot], Some(&digits[dot + 1..])),
            None => (digits, None),
        };
        match fraction {
            None => !whole.is_empty() && whole.iter().all(u8::is_ascii_digit),
            Some(fraction) => {
                whole.iter().all(u8::is_ascii_digit)
                    && !fraction.is_empty()
                    && fraction.iter().all(u8::is_ascii_digit)
            }
        }
    };
    !bytes.starts_with(b"-") || bytes == b"-" || negative(bytes) || bytes.contains(&b' ')
}

pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut arguments = arguments.into_iter();
    let (mut config, mut allow, mut check, mut json, mut version, mut help) =
        (None, false, false, false, false, false);
    let mut unknown = Vec::new();
    while let Some(argument) = arguments.next() {
        let bytes = argument.as_encoded_bytes();
        if let Some(value) = bytes.strip_prefix(b"--config=") {
            config = Some(PathBuf::from(config_argument(&argument, value)));
            continue;
        }
        match bytes {
            b"--config" => match arguments.next() {
                Some(value) if is_value(&value) => config = Some(PathBuf::from(value)),
                _ => return Err("argument --config: expected one argument".into()),
            },
            b"--allow-insecure-localhost" => allow = true,
            b"--check" => check = true,
            b"--json" => json = true,
            b"--version" => version = true,
            b"-h" | b"--help" => help = true,
            _ => unknown.push(argument.to_string_lossy().into_owned()),
        }
    }
    if help {
        return Ok(Command::Help);
    }
    if !unknown.is_empty() {
        return Err(format!("unrecognized arguments: {}", unknown.join(" ")));
    }
    if version {
        return Ok(Command::Version);
    }
    let config = config.ok_or("the following arguments are required: --config")?;
    if json && !check {
        return Err("argument --json: only with --check".into());
    }
    Ok(if check {
        Command::Check {
            config,
            allow_insecure_localhost: allow,
            json,
        }
    } else {
        Command::Run {
            config,
            allow_insecure_localhost: allow,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(arguments: &[&str]) -> Result<Command, String> {
        parse(arguments.iter().map(OsString::from))
    }

    #[test]
    fn exact_spellings_and_argparse_value_rules() {
        let run = |config: &str, allow| {
            Ok(Command::Run {
                config: config.into(),
                allow_insecure_localhost: allow,
            })
        };
        assert_eq!(parse_strs(&["--config", "a.json"]), run("a.json", false));
        assert_eq!(
            parse_strs(&["--config=a.json", "--allow-insecure-localhost"]),
            run("a.json", true)
        );
        assert_eq!(
            parse_strs(&["--config", "missing", "--config", "b"]),
            run("b", false)
        );
        assert_eq!(parse_strs(&["--config", "-1"]), run("-1", false));
        assert_eq!(parse_strs(&["--config", "-"]), run("-", false));
        assert_eq!(parse_strs(&["--config", "-a b"]), run("-a b", false));
        assert_eq!(parse_strs(&["--config="]), run("", false));
        assert_eq!(
            parse_strs(&["--check", "--config", "c", "--json"]),
            Ok(Command::Check {
                config: "c".into(),
                allow_insecure_localhost: false,
                json: true
            })
        );
        assert_eq!(parse_strs(&["--version"]), Ok(Command::Version));
        assert_eq!(parse_strs(&["--config", "c", "-h"]), Ok(Command::Help));
        for refused in [
            &["--conf", "a"][..],
            &["--allow"],
            &["--config"],
            &["--config", "--check"],
            &[],
            &["--allow-insecure-localhost"],
            &["--config", "a", "--json"],
            &["--config", "a", "x"],
            &["--config", "a", "--"],
            &["--allow-insecure-localhost=1", "--config", "a"],
        ] {
            assert!(parse_strs(refused).is_err(), "{refused:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_pass_through() {
        use std::os::unix::ffi::OsStringExt;
        let mut argument = b"--config=".to_vec();
        argument.extend_from_slice(b"st\xffte.json");
        let parsed = parse([OsString::from_vec(argument)]).unwrap();
        assert_eq!(
            parsed,
            Command::Run {
                config: PathBuf::from(OsString::from_vec(b"st\xffte.json".to_vec())),
                allow_insecure_localhost: false
            }
        );
    }
}
