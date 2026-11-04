//! Desktop input idle time and screen lock, written by the desktop service and
//! listed for the mobile relay. Durations are relative, so readers on other
//! machines need no clock agreement. Nothing else about the desktop is recorded.
use crate::{cli, config::Config};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const FILE: &str = "desktop-presence.json";
/// Listings omit older presence: the GUI closed or crashed, or the machine slept.
const STALE_SECONDS: f64 = 120.0;
const MAX_INPUT_BYTES: u64 = 4096;
const MAX_IDLE_SECONDS: f64 = 10.0 * 365.0 * 86400.0;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    idle_seconds: f64,
    locked: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Record {
    idle_seconds: f64,
    locked: bool,
    locked_since: Option<f64>,
    written_at: f64,
}

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub fn dispatch(config: &Config, args: &[String], dry: bool) -> cli::Result<i32> {
    if dry {
        return Err(cli::Error::new(1, "presence does not support --dry-run"));
    }
    if args != ["set", "--json"] {
        return Err(cli::Error::new(1, "usage: hgs presence set --json"));
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| cli::Error::new(1, format!("read presence: {e}")))?;
    let input = parse(crate::state::without_bom(&bytes)).map_err(|e| cli::Error::new(1, format!("{e:#}")))?;
    let _admission = crate::desktop::legacy_presence(config)
        .map_err(|e| cli::Error::new(1, format!("{e:#}")))?;
    write(&config.state, &input, now()).map_err(|e| cli::Error::new(1, format!("{e:#}")))?;
    println!("{}", json!({"ok": true}));
    Ok(0)
}

fn parse(bytes: &[u8]) -> Result<Input> {
    ensure!(
        bytes.len() as u64 <= MAX_INPUT_BYTES,
        "presence input is too large"
    );
    let input: Input = serde_json::from_slice(bytes).context("invalid presence input")?;
    ensure!(
        input.idle_seconds.is_finite() && (0.0..=MAX_IDLE_SECONDS).contains(&input.idle_seconds),
        "idle_seconds must be a non-negative number of seconds"
    );
    Ok(input)
}

/// Consecutive locked writes keep the first lock time; unlocking clears it.
fn next(previous: Option<&Record>, input: &Input, at: f64) -> Record {
    let locked_since = input.locked.then(|| {
        previous
            .filter(|p| p.locked)
            .and_then(|p| p.locked_since)
            .unwrap_or(at)
    });
    Record {
        idle_seconds: input.idle_seconds,
        locked: input.locked,
        locked_since,
        written_at: at,
    }
}

fn read(path: &Path) -> Option<Record> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    crate::platform::no_follow(&mut options);
    #[cfg(windows)]
    crate::platform::refuse_link(&mut options);
    let file = options.open(path).ok()?;
    if !crate::platform::verify_private_file(&file).ok()? {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_INPUT_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_INPUT_BYTES { return None; }
    let record: Record = serde_json::from_slice(&bytes).ok()?;
    (record.idle_seconds.is_finite() && record.idle_seconds >= 0.0 && record.written_at.is_finite())
        .then_some(record)
}

fn write(root: &Path, input: &Input, at: f64) -> Result<Record> {
    fs::create_dir_all(root).context("create the hgs state directory")?;
    let path = root.join(FILE);
    let record = next(read(&path).as_ref(), input, at);
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    file.write_all(&serde_json::to_vec(&record)?)?;
    file.as_file().sync_all()?;
    crate::platform::persist(file, &path).map_err(|e| e.error)?;
    Ok(record)
}

/// The service owns the presence fence around this call. Unknown observations
/// never refresh a previous activity timestamp. In particular, a lower bound
/// since Wayland observation started is not evidence of recent user input.
pub(crate) fn write_sample(
    root: &Path,
    sample: zerus_presence_sampler::Sample,
    at: f64,
) -> Result<bool> {
    let Some((idle_seconds, locked)) = sample_values(sample)? else {
        return Ok(false);
    };
    ensure!(at.is_finite() && at >= 0.0, "invalid presence timestamp");
    write(root, &Input { idle_seconds, locked }, at)?;
    Ok(true)
}

pub(crate) fn sample_values(sample: zerus_presence_sampler::Sample) -> Result<Option<(f64, bool)>> {
    let (Some(idle), Some(locked)) = (sample.idle, sample.locked) else {
        return Ok(None);
    };
    if !idle.since_input {
        return Ok(None);
    }
    let idle_seconds = idle.elapsed.as_secs_f64();
    ensure!(
        idle_seconds <= MAX_IDLE_SECONDS,
        "invalid presence sample"
    );
    Ok(Some((idle_seconds, locked)))
}

/// `desktop_presence` for `hgs ls --json --local`, aged to `at`. Records written
/// more than two minutes ago, or that far in the future, are not listed.
pub fn listing(root: &Path, at: f64) -> Option<Value> {
    let record = read(&root.join(FILE))?;
    let age = at - record.written_at;
    if !(-STALE_SECONDS..=STALE_SECONDS).contains(&age) {
        return None;
    }
    let age = age.max(0.0);
    let locked_seconds = record
        .locked
        .then(|| record.locked_since.map_or(age, |since| (at - since).max(0.0)));
    Some(json!({
        "idle_seconds": record.idle_seconds + age,
        "locked": record.locked,
        "locked_seconds": locked_seconds,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use zerus_presence_sampler::{Idle, IdleSource, Sample};

    fn sample(seconds: u64, since_input: bool, locked: Option<bool>) -> Sample {
        Sample {
            idle: Some(Idle {
                elapsed: Duration::from_secs(seconds),
                source: IdleSource::WaylandInputLowerBound,
                since_input,
            }),
            locked,
        }
    }

    #[test]
    fn unknown_or_observation_only_input_never_renews_presence() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!write_sample(dir.path(), Sample::default(), 100.0).unwrap());
        assert!(!write_sample(dir.path(), sample(0, true, None), 100.0).unwrap());
        assert!(!dir.path().join(FILE).exists());
        write(
            dir.path(),
            &Input {
                idle_seconds: 8.0,
                locked: false,
            },
            100.0,
        )
        .unwrap();
        let original = fs::read(dir.path().join(FILE)).unwrap();
        for input in [
            sample(1, false, Some(false)),
            sample(600, false, Some(true)),
            Sample {
                idle: None,
                locked: Some(true),
            },
        ] {
            assert!(!write_sample(dir.path(), input, 110.0).unwrap());
            assert_eq!(fs::read(dir.path().join(FILE)).unwrap(), original);
        }
        assert_eq!(
            listing(dir.path(), 221.0),
            None,
            "unknown samples must not extend the original TTL"
        );
    }

    #[test]
    fn service_samples_preserve_lock_age_and_input_contract() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write_sample(dir.path(), sample(7, true, Some(true)), 100.0).unwrap());
        assert!(write_sample(dir.path(), sample(37, true, Some(true)), 130.0).unwrap());
        assert_eq!(
            listing(dir.path(), 140.0).unwrap(),
            json!({"idle_seconds":47.0,"locked":true,"locked_seconds":40.0})
        );
        let original = fs::read(dir.path().join(FILE)).unwrap();
        assert!(write_sample(
            dir.path(),
            sample(MAX_IDLE_SECONDS as u64 + 1, true, Some(false)),
            140.0
        )
        .is_err());
        assert_eq!(fs::read(dir.path().join(FILE)).unwrap(), original);
        assert!(write_sample(dir.path(), sample(0, true, Some(false)), 150.0).unwrap());
        assert_eq!(
            listing(dir.path(), 150.0).unwrap()["locked_seconds"],
            Value::Null
        );
    }


    #[test]
    fn consecutive_locked_writes_keep_the_lock_time() {
        let dir = tempfile::tempdir().unwrap();
        let input = |idle_seconds, locked| Input { idle_seconds, locked };
        assert_eq!(write(dir.path(), &input(10.0, true), 100.0).unwrap().locked_since, Some(100.0));
        assert_eq!(write(dir.path(), &input(40.0, true), 130.0).unwrap().locked_since, Some(100.0));
        assert_eq!(write(dir.path(), &input(0.0, false), 160.0).unwrap().locked_since, None);
        assert_eq!(write(dir.path(), &input(1.0, true), 190.0).unwrap().locked_since, Some(190.0));
    }

    #[test]
    fn listings_age_values_and_omit_stale_or_future_records() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(listing(dir.path(), 100.0), None);
        write(dir.path(), &Input { idle_seconds: 5.0, locked: false }, 100.0).unwrap();
        assert_eq!(
            listing(dir.path(), 130.0),
            Some(json!({"idle_seconds": 35.0, "locked": false, "locked_seconds": null}))
        );
        assert_eq!(listing(dir.path(), 220.0).unwrap()["idle_seconds"], 125.0);
        assert_eq!(listing(dir.path(), 220.5), None);
        // A wall clock moved backwards: presence written "in the future" is unknown.
        assert_eq!(listing(dir.path(), -30.0), None);
        write(dir.path(), &Input { idle_seconds: 0.0, locked: true }, 300.0).unwrap();
        assert_eq!(
            listing(dir.path(), 310.0),
            Some(json!({"idle_seconds": 10.0, "locked": true, "locked_seconds": 10.0}))
        );
    }

    #[test]
    fn input_accepts_only_bounded_idle_seconds_and_a_lock_flag() {
        assert!(parse(br#"{"idle_seconds":0,"locked":false}"#).is_ok());
        assert!(parse(br#"{"idle_seconds":12.5,"locked":true}"#).is_ok());
        let bad: [&[u8]; 7] = [
            br#"{"idle_seconds":-1,"locked":false}"#,
            br#"{"idle_seconds":1}"#,
            br#"{"idle_seconds":"1","locked":false}"#,
            br#"{"idle_seconds":1,"locked":false,"extra":1}"#,
            br#"{"idle_seconds":1e12,"locked":false}"#,
            b"[]",
            b"{",
        ];
        for input in bad {
            assert!(parse(input).is_err(), "{}", String::from_utf8_lossy(input));
        }
    }

    #[test]
    fn oversized_and_special_presence_leaves_never_supply_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        write(dir.path(), &Input { idle_seconds: 0.0, locked: false }, 100.0).unwrap();
        fs::write(&path, vec![b' '; MAX_INPUT_BYTES as usize + 1]).unwrap();
        assert_eq!(listing(dir.path(), 100.0), None);
        #[cfg(unix)]
        {
            fs::remove_file(&path).unwrap();
            let foreign = dir.path().join("foreign");
            fs::write(&foreign, b"preserved").unwrap();
            std::os::unix::fs::symlink(&foreign, &path).unwrap();
            assert_eq!(listing(dir.path(), 100.0), None);
            assert_eq!(fs::read(&foreign).unwrap(), b"preserved");
            fs::remove_file(&path).unwrap();
            use std::os::unix::ffi::OsStrExt;
            let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            assert_eq!(listing(dir.path(), 100.0), None);
        }
    }

    #[test]
    fn unreadable_records_are_replaced_and_never_listed() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(FILE), b"{\"damaged\":").unwrap();
        assert_eq!(listing(dir.path(), 10.0), None);
        let record = write(dir.path(), &Input { idle_seconds: 2.0, locked: true }, 10.0).unwrap();
        assert_eq!(record.locked_since, Some(10.0));
    }
}
