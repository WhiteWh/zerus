//! One fanout, with byte and item bounds independent of subscriber speed.
use super::protocol::{FRAME_BYTES, QUEUE_BYTES, SUBSCRIBERS};
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
};

#[derive(Default)]
struct State {
    frames: VecDeque<Arc<[u8]>>,
    bytes: usize,
    closed: bool,
}

#[derive(Default)]
pub struct Queue {
    state: Mutex<State>,
    ready: Condvar,
}

impl Queue {
    #[cfg(test)]
    pub(super) fn take(&self) -> Option<Arc<[u8]>> {
        let mut state = self.state.lock().unwrap();
        let frame = state.frames.pop_front()?;
        state.bytes -= frame.len();
        Some(frame)
    }
    pub fn offer(&self, frame: Arc<[u8]>) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return false;
        }
        if frame.len() > FRAME_BYTES
            || state.bytes + frame.len() > QUEUE_BYTES
            || state.frames.len() >= 1024
        {
            state.closed = true;
            state.frames.clear();
            state.bytes = 0;
            self.ready.notify_all();
            return false;
        }
        state.bytes += frame.len();
        state.frames.push_back(frame);
        self.ready.notify_one();
        true
    }
    pub fn next(&self) -> Option<Arc<[u8]>> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(frame) = state.frames.pop_front() {
                state.bytes -= frame.len();
                return Some(frame);
            }
            if state.closed {
                return None;
            }
            state = self.ready.wait(state).unwrap();
        }
    }
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        self.ready.notify_all();
    }
    pub fn closed(&self) -> bool {
        self.state.lock().unwrap().closed
    }
}

#[derive(Default)]
pub struct Fanout(Vec<Arc<Queue>>);
impl Fanout {
    pub fn len(&mut self) -> usize {
        self.0.retain(|queue| !queue.closed());
        self.0.len()
    }
    pub fn subscribe(&mut self, queue: Arc<Queue>) -> bool {
        if self.len() == SUBSCRIBERS {
            return false;
        }
        self.0.push(queue);
        true
    }
    pub fn publish(&mut self, frame: Arc<[u8]>) {
        self.0.retain(|queue| queue.offer(frame.clone()));
    }
}
impl Drop for Fanout {
    fn drop(&mut self) {
        for queue in &self.0 {
            queue.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_slow_subscriber_is_disconnected_without_blocking_another() {
        let mut fanout = Fanout::default();
        let slow = Arc::new(Queue::default());
        let fast = Arc::new(Queue::default());
        assert!(fanout.subscribe(slow.clone()));
        assert!(fanout.subscribe(fast.clone()));
        let frame: Arc<[u8]> = vec![0; FRAME_BYTES].into();
        for _ in 0..8 {
            fanout.publish(frame.clone());
            assert_eq!(fast.next().unwrap().len(), FRAME_BYTES);
        }
        assert!(slow.closed());
        assert_eq!(fanout.len(), 1);
        assert_eq!(slow.state.lock().unwrap().bytes, 0);
    }
    #[test]
    fn subscriber_and_small_frame_counts_are_bounded() {
        let mut fanout = Fanout::default();
        for _ in 0..SUBSCRIBERS {
            assert!(fanout.subscribe(Arc::default()));
        }
        assert!(!fanout.subscribe(Arc::default()));
        for _ in 0..1025 {
            fanout.publish(Arc::from(&b"x"[..]));
        }
        assert_eq!(fanout.len(), 0);
    }
}
