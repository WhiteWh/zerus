//! Restore/import once, then save through the app's existing Settings writer.
use std::sync::Mutex;
use tauri::{LogicalPosition, LogicalSize, WebviewWindow, WindowEvent};
use zerus_client_core::settings::{
    geometry::{Geometry, KEY},
    Settings,
};

fn current(window: &WebviewWindow, previous: Option<Geometry>) -> Option<Geometry> {
    let scale = window.scale_factor().ok()?;
    // Wayland does not expose global window positions. Still remember size
    // and state, retaining an earlier position when one was available.
    let position = window
        .outer_position()
        .ok()
        .map(|position| position.to_logical::<i32>(scale))
        .unwrap_or_else(|| {
            LogicalPosition::new(previous.map_or(0, |g| g.x), previous.map_or(0, |g| g.y))
        });
    let size = window.inner_size().ok()?.to_logical::<u32>(scale);
    let geometry = Geometry {
        version: 1,
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
        maximized: false,
        fullscreen: false,
    };
    geometry.valid().then_some(geometry)
}

pub fn attach(window: &WebviewWindow, settings: Settings) {
    let saved = Geometry::parse(&settings.text(KEY));
    let restored = saved.and_then(|saved| {
        let monitors = window.available_monitors().ok()?;
        let areas: Vec<_> = monitors
            .iter()
            .map(|monitor| {
                let area = monitor.work_area();
                let position = area.position.to_logical::<i32>(monitor.scale_factor());
                let size = area.size.to_logical::<u32>(monitor.scale_factor());
                (position.x, position.y, size.width, size.height)
            })
            .collect();
        // Prefer the saved display, otherwise the current/primary display.
        let screen = areas
            .iter()
            .copied()
            .find(|&(x, y, w, h)| {
                saved.x as i64 >= x as i64
                    && (saved.x as i64) < x as i64 + w as i64
                    && saved.y as i64 >= y as i64
                    && (saved.y as i64) < y as i64 + h as i64
            })
            .or_else(|| {
                window.current_monitor().ok().flatten().and_then(|monitor| {
                    let pos = monitor
                        .work_area()
                        .position
                        .to_logical::<i32>(monitor.scale_factor());
                    areas
                        .iter()
                        .copied()
                        .find(|area| area.0 == pos.x && area.1 == pos.y)
                })
            })
            .or_else(|| areas.first().copied())?;
        let fitted = saved.fit(screen);
        let _ = window.set_min_size(Some(LogicalSize::new(900.min(screen.2), 560.min(screen.3))));
        if window
            .set_size(LogicalSize::new(fitted.width, fitted.height))
            .is_err()
        {
            return None;
        }
        let _ = window.set_position(LogicalPosition::new(fitted.x, fitted.y));
        if fitted.maximized {
            let _ = window.maximize();
        }
        if fitted.fullscreen {
            let _ = window.set_fullscreen(true);
        }
        Some(fitted)
    });
    let normal = Mutex::new(restored.or_else(|| current(window, saved)));
    let tracked = window.clone();
    window.on_window_event(move |event| {
        if !matches!(
            event,
            WindowEvent::Moved(_) | WindowEvent::Resized(_) | WindowEvent::CloseRequested { .. }
        ) || tracked.is_visible().ok() != Some(true)
            || tracked.is_minimized().ok() != Some(false)
        {
            return;
        }
        let (Ok(maximized), Ok(fullscreen)) = (tracked.is_maximized(), tracked.is_fullscreen())
        else {
            return;
        };
        let mut normal = normal.lock().unwrap_or_else(|error| error.into_inner());
        if !maximized && !fullscreen {
            *normal = current(&tracked, *normal);
        }
        if let Some(mut geometry) = *normal {
            geometry.maximized = maximized;
            geometry.fullscreen = fullscreen;
            // Settings coalesces writes off the UI thread; no filesystem work
            // is performed by a move/resize callback.
            if let Ok(text) = serde_json::to_string(&geometry) {
                let _ = settings.set(KEY, serde_json::Value::String(text));
            }
        }
    });
}
