//! One original continuation shared by copies of a native callback capture.
//! These helpers carry no notification policy, service identity or authority.
use std::sync::{Mutex, PoisonError};

/// A completion, called once on any thread.
pub type Done<T> = Box<dyn FnOnce(T) + Send>;

/// A completion the system calls through an `Fn` block, at most once used.
pub(crate) fn once<T>(done: Done<T>) -> Mutex<Option<Done<T>>> {
    Mutex::new(Some(done))
}

pub(crate) fn take<T>(done: &Mutex<Option<Done<T>>>) -> Option<Done<T>> {
    done.lock().unwrap_or_else(PoisonError::into_inner).take()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    };

    #[test]
    fn competing_fake_callbacks_consume_the_original_only_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let received = calls.clone();
        let capture = Arc::new(once(Box::new(move |()| {
            received.fetch_add(1, Ordering::SeqCst);
        })));
        let barrier = Arc::new(Barrier::new(8));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let capture = capture.clone();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    if let Some(done) = take(&capture) {
                        done(());
                    }
                });
            }
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(take(&capture).is_none());
    }

    struct Held(Arc<AtomicUsize>);
    impl Drop for Held {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn an_uncalled_original_is_retained_until_the_final_fake_capture_drops() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let held = Held(dropped.clone());
        let capture = Arc::new(once(Box::new(move |()| drop(held))));
        let copy = capture.clone();
        drop(capture);
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        drop(copy);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn poisoned_capture_keeps_the_original_available_for_one_completion() {
        let calls = Arc::new(AtomicUsize::new(0));
        let received = calls.clone();
        let capture = Arc::new(once(Box::new(move |()| {
            received.fetch_add(1, Ordering::SeqCst);
        })));
        let copy = capture.clone();
        assert!(std::thread::spawn(move || {
            let _guard = copy.lock().unwrap();
            panic!("synthetic capture poison");
        })
        .join()
        .is_err());
        take(&capture).unwrap()(());
        assert!(take(&capture).is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
