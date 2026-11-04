//! Resource use of the client's process tree: the app and its webview processes.
//!
//! `process_tree_metrics(pid)` samples the process `pid` and every process it owns
//! twice, `DEFAULT_WINDOW` apart, and returns RSS and CPU per process plus totals as
//! JSON for the `metrics` command and the measurement report.
//!
//! - Linux: `/proc`. WebKitGTK's web and network processes are descendants of the
//!   app (sometimes through `bwrap`), so the tree is every descendant by parent pid.
//!   RSS comes from `VmRSS`, PSS from `smaps_rollup` (shared pages split between the
//!   processes, so the PSS total does not count shared libraries twice).
//! - macOS: `proc_pid_rusage` (CPU, resident size and physical footprint, the
//!   figure Activity Monitor shows). WebKit's WebContent, Networking and GPU
//!   processes are XPC services launched by launchd, not children of the app;
//!   they are found through their responsible process
//!   (`responsibility_get_pid_responsible_for_pid`, looked up at run time) and,
//!   when that symbol is missing, through parent pids only. When libproc fails,
//!   `ps` is the fallback.
//!
//! Sampling blocks the calling thread for the window: call it off the main thread.

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// CPU is averaged over this window.
pub const DEFAULT_WINDOW: Duration = Duration::from_millis(500);

/// One sample of one process.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcSample {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    /// Cumulative user plus system CPU time, in seconds.
    pub cpu_seconds: f64,
    pub rss_bytes: u64,
    /// Linux: proportional set size; macOS: physical footprint.
    pub extra_bytes: Option<u64>,
}

/// RSS and CPU% of `pid` and the processes it owns, over `DEFAULT_WINDOW`.
pub fn process_tree_metrics(pid: u32) -> Value {
    process_tree_metrics_over(pid, DEFAULT_WINDOW)
}

/// As `process_tree_metrics`, with CPU averaged over `window`.
pub fn process_tree_metrics_over(pid: u32, window: Duration) -> Value {
    let first = match sample_tree(pid) {
        Ok(sample) => sample,
        Err(error) => return json!({ "pid": pid, "error": error }),
    };
    let started = Instant::now();
    std::thread::sleep(window);
    let second = match sample_tree(pid) {
        Ok(sample) => sample,
        Err(error) => return json!({ "pid": pid, "error": error }),
    };
    let elapsed = started.elapsed().as_secs_f64();
    summarize(pid, &first, &second, elapsed)
}

/// A sampled tree: the source that produced it and its processes by pid.
#[derive(Clone, Debug, Default)]
pub struct TreeSample {
    pub source: &'static str,
    pub processes: BTreeMap<u32, ProcSample>,
}

/// Builds the JSON summary from two samples `elapsed` seconds apart.
pub fn summarize(pid: u32, first: &TreeSample, second: &TreeSample, elapsed: f64) -> Value {
    let mut processes = Vec::new();
    let (mut rss, mut extra, mut cpu_seconds, mut cpu_percent) = (0u64, 0u64, 0f64, 0f64);
    let mut extra_known = true;
    for (id, now) in &second.processes {
        let percent = first
            .processes
            .get(id)
            .filter(|before| elapsed > 0.0 && now.cpu_seconds >= before.cpu_seconds)
            .map(|before| (now.cpu_seconds - before.cpu_seconds) / elapsed * 100.0);
        rss += now.rss_bytes;
        cpu_seconds += now.cpu_seconds;
        cpu_percent += percent.unwrap_or(0.0);
        match now.extra_bytes {
            Some(bytes) => extra += bytes,
            None => extra_known = false,
        }
        let mut entry = Map::new();
        entry.insert("pid".into(), json!(now.pid));
        entry.insert("ppid".into(), json!(now.ppid));
        entry.insert("name".into(), json!(now.name));
        entry.insert("role".into(), json!(role(pid, now)));
        entry.insert("rss_bytes".into(), json!(now.rss_bytes));
        entry.insert(extra_key(second.source).into(), json!(now.extra_bytes));
        entry.insert("cpu_seconds".into(), json!(round3(now.cpu_seconds)));
        entry.insert("cpu_percent".into(), json!(percent.map(round3)));
        processes.push(Value::Object(entry));
    }
    let mut total = Map::new();
    total.insert("processes".into(), json!(second.processes.len()));
    total.insert("rss_bytes".into(), json!(rss));
    total.insert(
        extra_key(second.source).into(),
        json!(extra_known.then_some(extra)),
    );
    total.insert("cpu_seconds".into(), json!(round3(cpu_seconds)));
    total.insert("cpu_percent".into(), json!(round3(cpu_percent)));
    json!({
        "pid": pid,
        "source": second.source,
        "window_ms": (elapsed * 1000.0).round() as u64,
        "sampled_at_ms": SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
        "total": Value::Object(total),
        "processes": processes,
    })
}

fn extra_key(source: &str) -> &'static str {
    if source == "procfs" {
        "pss_bytes"
    } else {
        "footprint_bytes"
    }
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// What a process is for, from its name: the app itself, a webview content
/// process, the network process, the GPU process or another child.
pub fn role(root: u32, process: &ProcSample) -> &'static str {
    if process.pid == root {
        return "app";
    }
    let name = process.name.to_ascii_lowercase();
    if name.contains("webcontent")
        || name.contains("webkitwebproces")
        || name.contains("webprocess")
    {
        "web"
    } else if name.contains("network") {
        "network"
    } else if name.contains("gpu") {
        "gpu"
    } else {
        "child"
    }
}

/// `root` and every process whose parent chain reaches it.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
pub fn descendants(root: u32, parents: &BTreeMap<u32, u32>) -> BTreeSet<u32> {
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (&pid, &ppid) in parents {
        if pid != ppid {
            children.entry(ppid).or_default().push(pid);
        }
    }
    let mut tree = BTreeSet::new();
    let mut queue = vec![root];
    while let Some(pid) = queue.pop() {
        if tree.insert(pid) {
            if let Some(next) = children.get(&pid) {
                queue.extend(next.iter().copied());
            }
        }
    }
    tree
}

#[cfg(target_os = "linux")]
pub fn sample_tree(root: u32) -> Result<TreeSample, String> {
    linux::sample_tree(root)
}

#[cfg(target_os = "macos")]
pub fn sample_tree(root: u32) -> Result<TreeSample, String> {
    macos::sample_tree(root).or_else(|error| {
        ps::sample_tree(root).map_err(|fallback| format!("libproc: {error}; ps: {fallback}"))
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn sample_tree(_root: u32) -> Result<TreeSample, String> {
    Err("process metrics are not implemented on this platform".into())
}

#[cfg(target_os = "linux")]
pub mod linux {
    use super::{descendants, ProcSample, TreeSample};
    use std::collections::BTreeMap;
    use std::fs;
    use std::io;
    use std::path::Path;

    /// USER_HZ: the unit of utime and stime in /proc/<pid>/stat. It is part of
    /// the kernel ABI and 100 on every architecture Linux supports today.
    const CLOCK_TICKS: f64 = 100.0;

    /// Parsed fields of /proc/<pid>/stat.
    #[derive(Debug, PartialEq)]
    pub struct Stat {
        pub name: String,
        pub ppid: u32,
        pub cpu_ticks: u64,
    }

    /// Parses /proc/<pid>/stat. The name sits in parentheses and may itself
    /// contain spaces and parentheses, so the fields start after the last ')'.
    pub fn parse_stat(text: &str) -> Option<Stat> {
        let open = text.find('(')?;
        let close = text.rfind(')')?;
        let name = text.get(open + 1..close)?.to_string();
        let rest: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
        // Field n of proc(5) is rest[n - 3]: state is 3, ppid 4, utime 14, stime 15.
        let ppid = rest.get(1)?.parse().ok()?;
        let utime: u64 = rest.get(11)?.parse().ok()?;
        let stime: u64 = rest.get(12)?.parse().ok()?;
        Some(Stat {
            name,
            ppid,
            cpu_ticks: utime + stime,
        })
    }

    /// The value in kB of `key` in a /proc status-like file, in bytes.
    pub fn kb_field(text: &str, key: &str) -> Option<u64> {
        text.lines().find_map(|line| {
            let value = line.strip_prefix(key)?.strip_prefix(':')?;
            let kb: u64 = value.split_whitespace().next()?.parse().ok()?;
            Some(kb * 1024)
        })
    }

    /// errno "No such process" on Linux, which a vanished process's files give.
    const ESRCH: i32 = 3;

    /// A process's proportional set size, as `smaps_rollup` gives it.
    #[derive(Debug, PartialEq)]
    pub enum Pss {
        Bytes(u64),
        /// The process exited between the listing and the read.
        Vanished,
        /// The size cannot be read (permission or another failure).
        Unknown,
    }

    /// Reads `smaps_rollup`: a file without a `Pss` line (an exited process
    /// not yet reaped has no memory left) counts as 0; a missing file or
    /// process is a process that vanished.
    pub fn pss(read: io::Result<String>) -> Pss {
        match read {
            Ok(text) => Pss::Bytes(kb_field(&text, "Pss").unwrap_or(0)),
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(ESRCH) =>
            {
                Pss::Vanished
            }
            Err(_) => Pss::Unknown,
        }
    }

    fn parents(proc: &Path) -> BTreeMap<u32, u32> {
        let mut parents = BTreeMap::new();
        let Ok(entries) = fs::read_dir(proc) else {
            return parents;
        };
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            if let Some(stat) = fs::read_to_string(proc.join(pid.to_string()).join("stat"))
                .ok()
                .as_deref()
                .and_then(parse_stat)
            {
                parents.insert(pid, stat.ppid);
            }
        }
        parents
    }

    /// One process under `proc`; None when it vanished meanwhile.
    fn sample(proc: &Path, pid: u32) -> Option<ProcSample> {
        let dir = proc.join(pid.to_string());
        let stat = parse_stat(&fs::read_to_string(dir.join("stat")).ok()?)?;
        let status = fs::read_to_string(dir.join("status")).unwrap_or_default();
        let pss = match pss(fs::read_to_string(dir.join("smaps_rollup"))) {
            Pss::Bytes(bytes) => Some(bytes),
            Pss::Vanished => return None,
            Pss::Unknown => None,
        };
        Some(ProcSample {
            pid,
            ppid: stat.ppid,
            name: stat.name,
            cpu_seconds: stat.cpu_ticks as f64 / CLOCK_TICKS,
            rss_bytes: kb_field(&status, "VmRSS").unwrap_or(0),
            extra_bytes: pss,
        })
    }

    pub fn sample_tree(root: u32) -> Result<TreeSample, String> {
        sample_tree_at(Path::new("/proc"), root)
    }

    /// [`sample_tree`] over a procfs mounted at `proc`.
    pub fn sample_tree_at(proc: &Path, root: u32) -> Result<TreeSample, String> {
        let root_sample =
            sample(proc, root).ok_or_else(|| format!("process {root} is not readable in /proc"))?;
        let mut processes = BTreeMap::new();
        processes.insert(root, root_sample);
        for pid in descendants(root, &parents(proc)) {
            if pid == root {
                continue;
            }
            // A process may exit between the listing and the read; skip it.
            if let Some(process) = sample(proc, pid) {
                processes.insert(pid, process);
            }
        }
        Ok(TreeSample {
            source: "procfs",
            processes,
        })
    }
}

#[cfg(target_os = "macos")]
pub mod macos {
    use super::{descendants, ProcSample, TreeSample};
    use std::collections::BTreeMap;
    use std::ffi::{c_int, c_void};
    use std::sync::OnceLock;

    type ResponsibleFn = unsafe extern "C" fn(libc::pid_t) -> libc::pid_t;

    /// `responsibility_get_pid_responsible_for_pid` from libSystem, when present.
    fn responsible_fn() -> Option<ResponsibleFn> {
        static FUNCTION: OnceLock<Option<usize>> = OnceLock::new();
        let address = *FUNCTION.get_or_init(|| {
            let name = c"responsibility_get_pid_responsible_for_pid";
            // SAFETY: dlsym with RTLD_DEFAULT and a NUL-terminated name only looks the symbol up.
            let pointer = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
            (!pointer.is_null()).then_some(pointer as usize)
        });
        // SAFETY: the symbol has this C signature in every macOS release that exports it.
        address.map(|address| unsafe { std::mem::transmute::<usize, ResponsibleFn>(address) })
    }

    /// Whether the responsible-process lookup is available (for diagnostics).
    #[cfg_attr(
        target_os = "macos",
        allow(
            dead_code,
            reason = "used by client/perf-probe-mac, which includes this file by path"
        )
    )]
    pub fn responsible_lookup_available() -> bool {
        responsible_fn().is_some()
    }

    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }

    extern "C" {
        // libc marks its binding deprecated in favour of the mach2 crate; this is the same symbol.
        fn mach_timebase_info(info: *mut Timebase) -> c_int;
    }

    /// Nanoseconds per Mach absolute time unit: rusage CPU times are in those units
    /// (1 on Intel, 125/3 on Apple silicon).
    fn timebase() -> f64 {
        static RATIO: OnceLock<f64> = OnceLock::new();
        *RATIO.get_or_init(|| {
            let mut info = Timebase { numer: 0, denom: 0 };
            // SAFETY: mach_timebase_info writes the struct it is given.
            let ok = unsafe { mach_timebase_info(&mut info) } == 0 && info.denom != 0;
            if ok {
                info.numer as f64 / info.denom as f64
            } else {
                1.0
            }
        })
    }

    fn all_pids() -> Result<Vec<u32>, String> {
        // SAFETY: a null buffer asks for the number of processes.
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if count <= 0 {
            return Err("proc_listallpids failed".into());
        }
        let mut pids = vec![0 as libc::pid_t; count as usize + 64];
        let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as c_int;
        // SAFETY: the buffer holds `bytes` bytes of pid_t.
        let filled = unsafe { libc::proc_listallpids(pids.as_mut_ptr() as *mut c_void, bytes) };
        if filled <= 0 {
            return Err("proc_listallpids failed".into());
        }
        pids.truncate(filled as usize);
        Ok(pids
            .into_iter()
            .filter(|&pid| pid > 0)
            .map(|pid| pid as u32)
            .collect())
    }

    fn bsd_info(pid: u32) -> Option<libc::proc_bsdinfo> {
        // SAFETY: proc_bsdinfo is plain data; proc_pidinfo fills at most its size.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as c_int;
        let read = unsafe {
            libc::proc_pidinfo(
                pid as c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut c_void,
                size,
            )
        };
        (read == size).then_some(info)
    }

    fn sample(pid: u32, ppid: u32, name: String) -> Option<ProcSample> {
        // SAFETY: rusage_info_v2 is plain data written by proc_pid_rusage.
        let mut usage: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::proc_pid_rusage(
                pid as c_int,
                libc::RUSAGE_INFO_V2,
                &mut usage as *mut _ as *mut libc::rusage_info_t,
            )
        };
        if result != 0 {
            return None;
        }
        let nanos = (usage.ri_user_time + usage.ri_system_time) as f64 * timebase();
        Some(ProcSample {
            pid,
            ppid,
            name,
            cpu_seconds: nanos / 1e9,
            rss_bytes: usage.ri_resident_size,
            extra_bytes: Some(usage.ri_phys_footprint),
        })
    }

    fn name_of(info: &libc::proc_bsdinfo) -> String {
        let raw = if info.pbi_name[0] != 0 {
            &info.pbi_name[..]
        } else {
            &info.pbi_comm[..]
        };
        let bytes: Vec<u8> = raw
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub fn sample_tree(root: u32) -> Result<TreeSample, String> {
        let pids = all_pids()?;
        let mut infos = BTreeMap::new();
        let mut parents = BTreeMap::new();
        for pid in pids {
            if let Some(info) = bsd_info(pid) {
                parents.insert(pid, info.pbi_ppid);
                infos.insert(pid, info);
            }
        }
        let mut owned = descendants(root, &parents);
        if let Some(responsible) = responsible_fn() {
            for &pid in infos.keys() {
                // SAFETY: the function only reads kernel state for the pid.
                if unsafe { responsible(pid as libc::pid_t) } == root as libc::pid_t {
                    owned.insert(pid);
                }
            }
            // Children of the XPC services (rare) belong to the tree as well.
            for pid in owned.clone() {
                owned.extend(descendants(pid, &parents));
            }
        }
        let root_info = infos
            .get(&root)
            .ok_or_else(|| format!("process {root} is not visible to libproc"))?;
        let mut processes = BTreeMap::new();
        let root_sample = sample(root, root_info.pbi_ppid, name_of(root_info))
            .ok_or_else(|| format!("proc_pid_rusage failed for {root}"))?;
        processes.insert(root, root_sample);
        for pid in owned {
            if pid == root {
                continue;
            }
            if let Some(info) = infos.get(&pid) {
                if let Some(process) = sample(pid, info.pbi_ppid, name_of(info)) {
                    processes.insert(pid, process);
                }
            }
        }
        Ok(TreeSample {
            source: "proc_pidinfo",
            processes,
        })
    }
}

/// `ps` fallback for macOS: parent-pid tree only, `ps`'s decaying %CPU turned
/// into a cumulative CPU time from `time`.
#[cfg(target_os = "macos")]
pub mod ps {
    use super::{descendants, ProcSample, TreeSample};
    use std::collections::BTreeMap;

    /// "[[dd-]hh:]mm:ss[.cc]" to seconds.
    pub fn parse_time(text: &str) -> Option<f64> {
        let (days, rest) = match text.split_once('-') {
            Some((days, rest)) => (days.parse::<f64>().ok()?, rest),
            None => (0.0, text),
        };
        let mut seconds = 0.0;
        for part in rest.split(':') {
            seconds = seconds * 60.0 + part.parse::<f64>().ok()?;
        }
        Some(days * 86400.0 + seconds)
    }

    pub fn sample_tree(root: u32) -> Result<TreeSample, String> {
        let output = crate::child_environment::command("/bin/ps")
            .args(["-A", "-o", "pid=,ppid=,rss=,time=,comm="])
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(format!("ps exited with {}", output.status));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut all = BTreeMap::new();
        let mut parents = BTreeMap::new();
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            let (Some(pid), Some(ppid), Some(rss), Some(time)) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let (Ok(pid), Ok(ppid), Ok(rss)) =
                (pid.parse::<u32>(), ppid.parse::<u32>(), rss.parse::<u64>())
            else {
                continue;
            };
            let name = fields.collect::<Vec<_>>().join(" ");
            let name = name.rsplit('/').next().unwrap_or("").to_string();
            parents.insert(pid, ppid);
            all.insert(
                pid,
                ProcSample {
                    pid,
                    ppid,
                    name,
                    cpu_seconds: parse_time(time).unwrap_or(0.0),
                    rss_bytes: rss * 1024,
                    extra_bytes: None,
                },
            );
        }
        if !all.contains_key(&root) {
            return Err(format!("process {root} is not listed by ps"));
        }
        let processes = descendants(root, &parents)
            .into_iter()
            .filter_map(|pid| all.remove(&pid).map(|p| (pid, p)))
            .collect();
        Ok(TreeSample {
            source: "ps",
            processes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, ppid: u32, name: &str, cpu: f64, rss: u64) -> ProcSample {
        ProcSample {
            pid,
            ppid,
            name: name.into(),
            cpu_seconds: cpu,
            rss_bytes: rss,
            extra_bytes: Some(rss / 2),
        }
    }

    #[test]
    fn descendants_follow_parent_chains_only() {
        let parents = BTreeMap::from([
            (1, 0),
            (10, 1),
            (11, 10),
            (12, 11),
            (20, 1),
            (21, 20),
            (30, 30),
        ]);
        assert_eq!(descendants(10, &parents), BTreeSet::from([10, 11, 12]));
        assert_eq!(descendants(30, &parents), BTreeSet::from([30]));
        assert_eq!(descendants(99, &parents), BTreeSet::from([99]));
    }

    #[test]
    fn roles_come_from_process_names() {
        assert_eq!(role(5, &process(5, 1, "zerus-client", 0.0, 0)), "app");
        assert_eq!(role(5, &process(6, 5, "WebKitWebProces", 0.0, 0)), "web");
        assert_eq!(
            role(5, &process(7, 1, "com.apple.WebKit.WebContent", 0.0, 0)),
            "web"
        );
        assert_eq!(
            role(5, &process(8, 5, "WebKitNetworkPr", 0.0, 0)),
            "network"
        );
        assert_eq!(
            role(5, &process(9, 1, "com.apple.WebKit.GPU", 0.0, 0)),
            "gpu"
        );
        assert_eq!(role(5, &process(10, 5, "bwrap", 0.0, 0)), "child");
    }

    #[test]
    fn summary_totals_and_cpu_percent_over_the_window() {
        let first = TreeSample {
            source: "procfs",
            processes: BTreeMap::from([
                (5, process(5, 1, "app", 1.0, 100)),
                (6, process(6, 5, "WebKitWebProces", 2.0, 300)),
            ]),
        };
        let second = TreeSample {
            source: "procfs",
            processes: BTreeMap::from([
                (5, process(5, 1, "app", 1.1, 110)),
                (6, process(6, 5, "WebKitWebProces", 2.5, 310)),
                (7, process(7, 5, "WebKitNetworkPr", 0.2, 50)),
            ]),
        };
        let value = summarize(5, &first, &second, 0.5);
        assert_eq!(value["source"], "procfs");
        assert_eq!(value["window_ms"], 500);
        assert_eq!(value["total"]["processes"], 3);
        assert_eq!(value["total"]["rss_bytes"], 470);
        assert_eq!(value["total"]["pss_bytes"], 55 + 155 + 25);
        // 0.1 s and 0.5 s of CPU in 0.5 s; the new process has no percent yet.
        assert_eq!(value["total"]["cpu_percent"], 120.0);
        let processes = value["processes"].as_array().unwrap();
        assert_eq!(processes[0]["role"], "app");
        assert_eq!(processes[0]["cpu_percent"], 20.0);
        assert_eq!(processes[1]["role"], "web");
        assert_eq!(processes[2]["cpu_percent"], Value::Null);
    }

    #[test]
    fn a_missing_process_reports_an_error() {
        let value = process_tree_metrics_over(u32::MAX - 1, Duration::from_millis(1));
        assert!(value["error"].is_string(), "{value}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_child_that_vanished_mid_sample_is_skipped_and_only_unknown_pss_nulls_the_total() {
        use linux::{pss, sample_tree_at, Pss};
        use std::io;
        let proc = std::env::temp_dir().join(format!("zerus-fake-proc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&proc);
        let process = |pid: u32, ppid: u32, smaps: Option<&str>| {
            let dir = proc.join(pid.to_string());
            std::fs::create_dir_all(&dir).unwrap();
            let stat = format!("{pid} (proc {pid}) S {ppid} 0 0 0 0 0 0 0 0 0 0 10 5 0 0");
            std::fs::write(dir.join("stat"), stat).unwrap();
            std::fs::write(dir.join("status"), "VmRSS:\t   400 kB\n").unwrap();
            if let Some(text) = smaps {
                std::fs::write(dir.join("smaps_rollup"), text).unwrap();
            }
        };
        process(100, 1, Some("Rss: 400 kB\nPss:   100 kB\n"));
        process(101, 100, Some("Rss: 400 kB\nPss:   50 kB\n"));
        // An hgs child that exited after its stat was read.
        process(102, 100, None);
        // An exited child not reaped yet: no memory left.
        process(103, 100, Some(""));
        let tree = sample_tree_at(&proc, 100).unwrap();
        assert_eq!(
            tree.processes.keys().copied().collect::<Vec<_>>(),
            [100, 101, 103]
        );
        let value = summarize(100, &tree, &tree, 0.5);
        assert_eq!(value["total"]["pss_bytes"], (100 + 50) * 1024);
        // A size that cannot be read keeps the process and makes the total unknown.
        std::fs::create_dir_all(proc.join("101/smaps_rollup.d")).unwrap();
        std::fs::remove_file(proc.join("101/smaps_rollup")).unwrap();
        std::fs::rename(
            proc.join("101/smaps_rollup.d"),
            proc.join("101/smaps_rollup"),
        )
        .unwrap();
        let tree = sample_tree_at(&proc, 100).unwrap();
        assert!(tree.processes.contains_key(&101));
        assert_eq!(
            summarize(100, &tree, &tree, 0.5)["total"]["pss_bytes"],
            Value::Null
        );
        assert_eq!(pss(Err(io::Error::from_raw_os_error(3))), Pss::Vanished);
        assert_eq!(
            pss(Err(io::Error::from(io::ErrorKind::PermissionDenied))),
            Pss::Unknown
        );
        let _ = std::fs::remove_dir_all(&proc);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stat_lines_with_parentheses_in_the_name_parse() {
        let line = "4242 (Web (Content) 2) S 4000 4242 4242 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 9 0 12345 0 0";
        let stat = linux::parse_stat(line).unwrap();
        assert_eq!(
            stat,
            linux::Stat {
                name: "Web (Content) 2".into(),
                ppid: 4000,
                cpu_ticks: 300
            }
        );
        assert_eq!(
            linux::kb_field("Name:\tx\nVmRSS:\t  2048 kB\n", "VmRSS"),
            Some(2048 * 1024)
        );
        assert_eq!(linux::kb_field("VmRSSX: 1 kB\n", "VmRSS"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn own_process_tree_reports_rss_cpu_and_children() {
        let _tree = crate::PROCESS_TREE
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut child = crate::child_environment::command("sleep")
            .arg("5")
            .spawn()
            .expect("spawn sleep");
        // Keep this thread busy for part of the window so the app's CPU is measurable.
        let busy = std::thread::spawn(|| {
            let until = Instant::now() + Duration::from_millis(150);
            let mut x = 0u64;
            while Instant::now() < until {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            }
            x
        });
        let value = process_tree_metrics_over(std::process::id(), Duration::from_millis(300));
        let _ = busy.join();
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(value["source"], "procfs", "{value}");
        assert!(
            value["total"]["rss_bytes"].as_u64().unwrap() > 1 << 20,
            "{value}"
        );
        assert!(value["total"]["pss_bytes"].as_u64().unwrap() > 0, "{value}");
        let processes = value["processes"].as_array().unwrap();
        let app = processes
            .iter()
            .find(|p| p["role"] == "app")
            .expect("app row");
        assert_eq!(app["pid"], std::process::id());
        assert!(app["cpu_percent"].as_f64().unwrap() > 5.0, "{value}");
        assert!(
            processes
                .iter()
                .any(|p| p["pid"] == child.id() && p["name"] == "sleep" && p["role"] == "child"),
            "{value}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ps_times_parse() {
        assert_eq!(ps::parse_time("0:01.50"), Some(1.5));
        assert_eq!(ps::parse_time("1:02:03.00"), Some(3723.0));
        assert_eq!(ps::parse_time("2-00:00:01"), Some(172801.0));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn own_process_tree_on_macos() {
        let _tree = crate::PROCESS_TREE
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let value = process_tree_metrics_over(std::process::id(), Duration::from_millis(100));
        assert_eq!(value["source"], "proc_pidinfo", "{value}");
        assert!(value["total"]["rss_bytes"].as_u64().unwrap() > 0, "{value}");
        assert!(
            value["total"]["footprint_bytes"].as_u64().unwrap() > 0,
            "{value}"
        );
    }
}
