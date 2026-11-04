//! One explicit private daemon, including its real authentication GUID.
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::DirBuilderExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use zbus::blocking::Connection;
const WAIT: Duration = Duration::from_secs(5);
const DIRS: [&str; 8] = [
    "config",
    "data",
    "cache",
    "runtime",
    "tmp",
    "tmux",
    "hgs-config",
    "hgs-state",
];

pub struct Bus {
    child: Child,
    pub address: String,
    directory: std::path::PathBuf,
}
impl Bus {
    pub fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("g4-bus-{}", uuid::Uuid::new_v4().simple()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        for name in DIRS {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(directory.join(name))
                .unwrap();
        }
        assert!(
            directory.as_os_str().len() + 4 < 108,
            "private fixture Unix path is too long"
        );
        let address = format!("unix:path={}/bus", directory.display());
        let child = Command::new("dbus-daemon")
            .args([
                "--session",
                "--nofork",
                "--print-address=1",
                "--address",
                &address,
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("HOME", &directory)
            .env("XDG_CONFIG_HOME", directory.join("config"))
            .env("XDG_DATA_HOME", directory.join("data"))
            .env("XDG_CACHE_HOME", directory.join("cache"))
            .env("XDG_RUNTIME_DIR", directory.join("runtime"))
            .env("TMPDIR", directory.join("tmp"))
            .env("TMUX_TMPDIR", directory.join("tmux"))
            .env("HGS_CONFIG_DIR", directory.join("hgs-config"))
            .env("HGS_STATE_DIR", directory.join("hgs-state"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut bus = Self {
            child,
            address,
            directory,
        };
        let mut stdout = bus.child.stdout.take().unwrap();
        let fd = stdout.as_raw_fd();
        // This exact original pipe only; bounded reads never wait past startup.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        let deadline = Instant::now() + WAIT;
        let mut raw = Vec::new();
        loop {
            let mut chunk = [0; 512];
            match stdout.read(&mut chunk) {
                Ok(0) => panic!("private daemon closed before its address"),
                Ok(n) => {
                    assert!(raw.len() + n <= 8192, "private daemon address output limit");
                    raw.extend_from_slice(&chunk[..n]);
                    if raw.ends_with(b"\n") {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (),
                Err(e) => panic!("private daemon address read: {e}"),
            }
            assert!(
                bus.child.try_wait().unwrap().is_none(),
                "private daemon exited before startup"
            );
            assert!(Instant::now() < deadline, "private daemon startup deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
        let actual = std::str::from_utf8(&raw).unwrap().trim_end_matches('\n');
        assert!(actual.starts_with(&(bus.address.clone() + ",guid=")));
        assert!(zbus::Address::try_from(actual).unwrap().guid().is_some());
        bus.address = actual.into();
        bus
    }
    pub fn connect(&self) -> Connection {
        zbus::blocking::connection::Builder::address(self.address.as_str())
            .unwrap()
            .method_timeout(WAIT)
            .build()
            .unwrap()
    }
}
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        self.child.wait().unwrap();
        let _ = std::fs::remove_file(self.directory.join("bus"));
        for name in DIRS {
            let _ = std::fs::remove_dir(self.directory.join(name));
        }
        let _ = std::fs::remove_dir(&self.directory);
    }
}
