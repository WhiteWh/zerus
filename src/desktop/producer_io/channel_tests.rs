use super::*;
use std::{
    io::{BufRead, Read, Write},
    os::unix::net::UnixStream,
};

fn until() -> Instant {
    Instant::now() + Duration::from_secs(1)
}
fn pair() -> (BufReader<Stream>, UnixStream) {
    let (stream, peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    peer.set_write_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    (BufReader::new(stream), peer)
}

#[test]
fn supplied_buffer_and_stream_survive_ordinary_reads_and_writes() {
    let (mut reader, mut peer) = pair();
    peer.write_all(b"one\ntwo\n").unwrap();
    assert_eq!(reader.fill_buf().unwrap(), b"one\ntwo\n");
    let mut channel = Channel::new(reader).unwrap();
    assert_eq!(channel.line(until()).unwrap().unwrap(), b"one\n");
    assert_eq!(channel.line(until()).unwrap().unwrap(), b"two\n");
    channel.write_line(b"ok\n", until()).unwrap();
    let mut reply = [0; 3];
    peer.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"ok\n");
    assert!(!channel.closed);
    drop(channel);
    assert_eq!(peer.read(&mut reply).unwrap(), 0);
}

#[test]
fn partial_read_timeout_is_terminal_and_never_resumes_a_frame() {
    let (reader, mut peer) = pair();
    peer.write_all(b"partial").unwrap();
    let mut channel = Channel::new(reader).unwrap();
    assert!(channel
        .line(Instant::now() + Duration::from_millis(30))
        .is_err());
    assert!(channel.closed);
    assert!(channel.line(until()).is_err());
    assert!(channel.write_line(b"retry\n", until()).is_err());
    // Linux refuses the peer's write after our full shutdown; XNU accepts and
    // discards it. The fence is the refused reads above and the EOF below.
    let late_write_refused = peer.write_all(b"tail\nnew\n").is_err();
    assert!(late_write_refused || cfg!(target_vendor = "apple"));
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
}

#[test]
fn expired_buffered_read_cannot_be_reopened_with_a_new_deadline() {
    let (mut reader, mut peer) = pair();
    peer.write_all(b"cached\n").unwrap();
    reader.fill_buf().unwrap();
    let mut channel = Channel::new(reader).unwrap();
    assert!(channel
        .line(Instant::now() - Duration::from_nanos(1))
        .is_err());
    assert_eq!(channel.reader.buffer(), b"cached\n");
    assert!(channel.line(until()).is_err());
    assert_eq!(channel.reader.buffer(), b"cached\n");
}

#[test]
fn eof_and_truncated_frame_both_fence_later_io() {
    for partial in [false, true] {
        let (reader, mut peer) = pair();
        if partial {
            peer.write_all(b"partial").unwrap();
        }
        drop(peer);
        let mut channel = Channel::new(reader).unwrap();
        let first = channel.line(until());
        if partial {
            assert!(first.is_err());
        } else {
            assert!(first.unwrap().is_none());
        }
        assert!(channel.closed);
        assert!(channel.line(until()).is_err());
        assert!(channel.write_line(b"later\n", until()).is_err());
    }
}

#[test]
fn write_refusal_is_terminal_without_sending_a_replacement() {
    for invalid_frame in [false, true] {
        let (reader, mut peer) = pair();
        let mut channel = Channel::new(reader).unwrap();
        let (bytes, deadline) = if invalid_frame {
            (&b"invalid"[..], until())
        } else {
            (&b"valid\n"[..], Instant::now() - Duration::from_nanos(1))
        };
        assert!(channel.write_line(bytes, deadline).is_err());
        assert!(channel.closed);
        assert!(channel.write_line(b"replacement\n", until()).is_err());
        assert!(channel.line(until()).is_err());
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    }
}
