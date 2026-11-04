use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

fn first_unblocked(view: &webkit2gtk::WebView) -> libc::c_ulong {
    let signal = gtk::glib::subclass::SignalId::lookup("decide-policy", view.type_()).unwrap();
    // SAFETY: a live, main-thread WebKit GObject and its looked-up signal;
    // the ID/UNBLOCKED mask ignores the null closure/function/data fields.
    unsafe {
        gtk::glib::gobject_ffi::g_signal_handler_find(
            view.as_ptr().cast(),
            gtk::glib::gobject_ffi::G_SIGNAL_MATCH_ID
                | gtk::glib::gobject_ffi::G_SIGNAL_MATCH_UNBLOCKED,
            signal.into_glib(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    }
}

fn hooks(ready: Arc<AtomicUsize>) -> Hooks {
    Hooks {
        load: Arc::new(move |event| {
            if event == Load::Ready {
                ready.fetch_add(1, Ordering::Relaxed);
            }
        }),
        browser: Arc::new(|_| panic!("a fixture page cannot open a browser")),
    }
}

/// Run explicitly with --ignored --test-threads=1 on the private test bus
/// and Xvfb. A default unit-test invocation must never initialize GTK on the
/// owner's desktop. This fixture never loads a live DeepSeek host.
#[test]
#[ignore = "requires a private D-Bus, Xvfb and HOME/XDG/TMPDIR"]
fn child_guard_replaces_one_handler_and_preserves_the_primary_across_reload() {
    gtk::init().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let launch = Launch::parse(&format!(
        "http://127.0.0.1:{}/?token=fixture",
        listener.local_addr().unwrap().port()
    ))
    .unwrap();
    let done = stop.clone();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !done.load(Ordering::Relaxed) && Instant::now() < deadline {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket
                        .set_read_timeout(Some(Duration::from_millis(200)))
                        .unwrap();
                    socket
                        .set_write_timeout(Some(Duration::from_millis(200)))
                        .unwrap();
                    let _ = socket.read(&mut [0; 4096]);
                    let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 16\r\nConnection: close\r\n\r\n<p>fixture</p>\n\n");
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("fixture listener: {error}"),
            }
        }
    });
    let view = || webkit2gtk::WebView::with_context(&webkit2gtk::WebContext::new_ephemeral());
    let primary = view();
    let main_calls = Arc::new(AtomicUsize::new(0));
    let calls = main_calls.clone();
    primary.connect_decide_policy(move |_, decision, _| {
        calls.fetch_add(1, Ordering::Relaxed);
        decision.use_();
        true
    });
    let main_handler = first_unblocked(&primary);
    assert_ne!(main_handler, 0);

    let absent = view();
    assert!(install(&absent, launch.clone(), hooks(Arc::default())).is_err());
    let ambiguous = view();
    for _ in 0..2 {
        ambiguous.connect_decide_policy(|_, _, _| false);
    }
    assert!(install(&ambiguous, launch.clone(), hooks(Arc::default())).is_err());
    assert!(absent.uri().is_none());
    assert!(ambiguous.uri().is_none());

    let child = view();
    let legacy_calls = Arc::new(AtomicUsize::new(0));
    let calls = legacy_calls.clone();
    child.connect_decide_policy(move |_, decision, _| {
        calls.fetch_add(1, Ordering::Relaxed);
        decision.use_();
        true
    });
    let legacy_handler = first_unblocked(&child);
    let ready = Arc::new(AtomicUsize::new(0));
    install(&child, launch.clone(), hooks(ready.clone())).unwrap();
    assert_ne!(first_unblocked(&child), legacy_handler);
    assert_eq!(first_unblocked(&primary), main_handler);

    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
    container.pack_start(&primary, true, true, 0);
    container.pack_start(&child, true, true, 0);
    window.add(&container);
    window.show_all();

    let context = gtk::glib::MainContext::default();
    for pass in 1..=2 {
        primary.load_uri(launch.url().as_str());
        child.load_uri(launch.url().as_str());
        let deadline = Instant::now() + Duration::from_secs(5);
        while ready.load(Ordering::Relaxed) < pass || main_calls.load(Ordering::Relaxed) < pass * 2
        {
            assert!(
                Instant::now() < deadline,
                "fixture navigation did not finish"
            );
            for _ in 0..16 {
                if !context.pending() {
                    break;
                }
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(legacy_calls.load(Ordering::Relaxed), 0);
        assert_eq!(first_unblocked(&primary), main_handler);
    }
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
}
