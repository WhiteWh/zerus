//! Bounded socket plumbing only. The native entry verifies its original peer
//! before reading Hello and before admitting frames; this module grants no role.
use super::Emission;
use crate::desktop::notifications::{decode, encode, ToHost, FRAME_BYTES};
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    os::unix::net::UnixStream,
};

const QUANTUM: usize = 16 * 1024;
const OUTPUTS: usize = 32;

pub enum Input {
    Pending,
    Progress,
    Frame(Box<ToHost>),
    Closed,
}

struct Writing {
    bytes: Vec<u8>,
    offset: usize,
    // Keep the original job's hold until the final byte or exact socket close.
    _emission: Emission,
}

pub struct Channel {
    socket: Option<UnixStream>,
    input: Vec<u8>,
    output: VecDeque<Writing>,
}
impl Channel {
    /// The caller retains the native selection/worker. This constructor merely
    /// takes ownership of the already-established socket, never authenticates it.
    pub fn new(socket: UnixStream) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket: Some(socket),
            input: Vec::new(),
            output: VecDeque::new(),
        })
    }
    pub fn socket(&self) -> Option<&UnixStream> {
        self.socket.as_ref()
    }
    pub fn read_turn(&mut self) -> Result<Input, &'static str> {
        if let Some(end) = self.input.iter().position(|b| *b == b'\n') {
            let frame = decode(&self.input[..=end])?;
            self.input.drain(..=end);
            return Ok(Input::Frame(Box::new(frame)));
        }
        let Some(socket) = &mut self.socket else {
            return Ok(Input::Closed);
        };
        if self.input.len() == FRAME_BYTES {
            return Err("notification_frame_limit");
        }
        let mut bytes = [0; QUANTUM];
        let count = bytes.len().min(FRAME_BYTES - self.input.len());
        match socket.read(&mut bytes[..count]) {
            Ok(0) if self.input.is_empty() => Ok(Input::Closed),
            Ok(0) => Err("partial_notification_frame"),
            Ok(n) => {
                self.input.extend_from_slice(&bytes[..n]);
                Ok(Input::Progress)
            }
            Err(error) if transient(&error) => Ok(Input::Pending),
            Err(_) => Err("notification_socket_read"),
        }
    }
    pub fn queue(&mut self, emission: Emission) -> Result<(), &'static str> {
        if self.socket.is_none() || self.output.len() == OUTPUTS {
            self.close(); // Close before dropping a rejected original emission.
            return Err("notification_socket_output_unavailable");
        }
        let bytes = match encode(&emission.frame) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.close();
                return Err(error);
            }
        };
        self.output.push_back(Writing {
            bytes,
            offset: 0,
            _emission: emission,
        });
        Ok(())
    }
    pub fn write_turn(&mut self) -> Result<bool, &'static str> {
        let Some(socket) = &mut self.socket else {
            return Err("notification_socket_closed");
        };
        let Some(frame) = self.output.front_mut() else {
            return Ok(false);
        };
        let end = frame.bytes.len().min(frame.offset + QUANTUM);
        match socket.write(&frame.bytes[frame.offset..end]) {
            Ok(0) => Err("notification_socket_write_zero"),
            Ok(n) => {
                frame.offset += n;
                if frame.offset == frame.bytes.len() {
                    self.output.pop_front();
                }
                Ok(true)
            }
            Err(error) if transient(&error) => Ok(false),
            Err(_) => Err("notification_socket_write"),
        }
    }
    pub fn output_empty(&self) -> bool {
        self.output.is_empty()
    }
    pub fn accepts_output(&self) -> bool {
        self.socket.is_some() && self.output.len() < OUTPUTS
    }
    pub fn close(&mut self) {
        drop(self.socket.take());
        self.output.clear();
        self.input.clear();
    }
}
impl Drop for Channel {
    fn drop(&mut self) {
        self.close();
    }
}
fn transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::notifications::{
        self as wire,
        macos::{Callback, Center, Host},
    };
    use crate::macos_notify::Envelope;
    use std::{sync::Mutex, time::Instant};

    #[test]
    fn split_frames_limits_and_partial_eof_use_only_the_owned_socket() {
        let (socket, mut service) = UnixStream::pair().unwrap();
        let mut channel = Channel::new(socket).unwrap();
        let hello = ToHost::Hello {
            abi: wire::ABI.into(),
            session: wire::tests::command(1).binding.session,
        };
        let bytes = encode(&hello).unwrap();
        service.write_all(&bytes[..4]).unwrap();
        assert!(matches!(channel.read_turn(), Ok(Input::Progress)));
        service.write_all(&bytes[4..]).unwrap();
        assert!(matches!(channel.read_turn(), Ok(Input::Progress)));
        assert!(matches!(
            channel.read_turn(),
            Ok(Input::Frame(frame)) if matches!(*frame, ToHost::Hello { .. })
        ));
        service.write_all(b"{").unwrap();
        drop(service);
        // Nonblocking reads may consume the byte and observe peer EOF on
        // separate turns, with WouldBlock/Interrupted between them.
        let until = Instant::now() + std::time::Duration::from_secs(2);
        let eof = loop {
            match channel.read_turn() {
                Ok(Input::Pending | Input::Progress) => {
                    assert!(Instant::now() < until, "partial-frame EOF not observed");
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                terminal => break terminal,
            }
        };
        assert!(matches!(eof, Err("partial_notification_frame")));
        let (socket, mut service) = UnixStream::pair().unwrap();
        let mut channel = Channel::new(socket).unwrap();
        let writer = std::thread::spawn(move || {
            service
                .set_write_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            service.write_all(&vec![b'x'; FRAME_BYTES]).unwrap();
        });
        let until = Instant::now() + std::time::Duration::from_secs(2);
        while channel.input.len() < FRAME_BYTES {
            assert!(Instant::now() < until);
            assert!(matches!(
                channel.read_turn(),
                Ok(Input::Pending | Input::Progress)
            ));
            std::thread::yield_now();
        }
        writer.join().unwrap();
        assert!(matches!(
            channel.read_turn(),
            Err("notification_frame_limit")
        ));
    }

    #[derive(Default)]
    struct Fake(Mutex<Option<Callback>>);
    impl Center for Fake {
        fn authorize(&self, reply: Callback) {
            *self.0.lock().unwrap() = Some(reply);
        }
        fn post(&self, _: &Envelope, _: &str, _: &str, reply: Callback) {
            *self.0.lock().unwrap() = Some(reply);
        }
        fn withdraw(&self, _: &str) {}
    }

    #[test]
    fn receipt_transport_and_completed_native_copies_both_hold_the_original_job() {
        let command = wire::tests::command(1);
        let mut host = Host::new(command.binding.session.clone()).unwrap();
        drop(host.next_output());
        let center = Fake::default();
        let now = Instant::now();
        host.receive(ToHost::Command(command), &center, now)
            .unwrap();
        center.0.lock().unwrap().take().unwrap().complete(true);
        host.poll(&center, now);
        let native_copy = center.0.lock().unwrap().take().unwrap();
        native_copy.complete(true);
        host.poll(&center, now);
        let receipt = host.next_output().unwrap();
        assert!(matches!(receipt.frame, wire::FromHost::Receipt(_)));
        let (socket, _service) = UnixStream::pair().unwrap();
        let mut channel = Channel::new(socket).unwrap();
        channel.queue(receipt).unwrap();
        host.poll(&center, now);
        assert!(host.next_output().is_none());
        drop(native_copy);
        host.poll(&center, now);
        assert!(
            host.next_output().is_none(),
            "queued receipt is still retained"
        );
        channel.write_turn().unwrap();
        assert!(channel.output_empty());
        host.poll(&center, now);
        assert!(matches!(
            host.next_output().unwrap().frame,
            wire::FromHost::Released { .. }
        ));
    }

    #[test]
    fn blocked_write_retains_the_original_until_exact_socket_close() {
        let command = wire::tests::command(1);
        let mut host = Host::new(command.binding.session.clone()).unwrap();
        drop(host.next_output());
        let center = Fake::default();
        let now = Instant::now();
        host.receive(ToHost::Command(command), &center, now)
            .unwrap();
        center.0.lock().unwrap().take().unwrap().complete(true);
        host.poll(&center, now);
        center.0.lock().unwrap().take().unwrap().complete(true);
        host.poll(&center, now);
        let receipt = host.next_output().unwrap();
        let (socket, mut service) = UnixStream::pair().unwrap();
        let mut channel = Channel::new(socket).unwrap();
        // Fill this test's actual socket, without involving a native host.
        // The queued original receipt must survive an actual EAGAIN.
        let mut borrowed = channel.socket().unwrap();
        let bytes = [0; 4096];
        loop {
            match borrowed.write(&bytes) {
                Ok(n) => assert!(n > 0),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("socket fill: {error}"),
            }
        }
        channel.queue(receipt).unwrap();
        assert!(!channel.write_turn().unwrap());
        host.poll(&center, now);
        assert!(host.next_output().is_none());
        // macOS refuses SO_RCVTIMEO with EINVAL once the peer has closed.
        service
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        channel.close();
        let mut drained = Vec::new();
        service.read_to_end(&mut drained).unwrap();
        assert!(!drained.is_empty());
        host.poll(&center, now);
        assert!(matches!(
            host.next_output().unwrap().frame,
            wire::FromHost::Released { .. }
        ));
    }
}
