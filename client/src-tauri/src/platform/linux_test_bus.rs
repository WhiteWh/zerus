//! Each native protocol test starts its own bus by explicit address. No test
//! connects to the inherited session bus or activates desktop services.
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

pub struct Bus {
    child: Child,
    pub address: String,
}
impl Bus {
    pub fn new() -> Self {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(address.starts_with("unix:"));
        Self {
            child,
            address: address.trim().to_owned(),
        }
    }
    /// Backpressure fixture, only for this helper's owned private daemon.
    #[cfg(feature = "live")]
    pub fn suspend(&self) -> Suspended<'_> {
        assert_eq!(
            unsafe { libc::kill(self.child.id() as i32, libc::SIGSTOP) },
            0
        );
        Suspended(self)
    }
    pub fn connect(&self) -> zbus::blocking::Connection {
        zbus::blocking::connection::Builder::address(self.address.as_str())
            .unwrap()
            .method_timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap()
    }
}
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(feature = "live")]
pub struct Suspended<'bus>(&'bus Bus);
#[cfg(feature = "live")]
impl Drop for Suspended<'_> {
    fn drop(&mut self) {
        let _ = unsafe { libc::kill(self.0.child.id() as i32, libc::SIGCONT) };
    }
}
