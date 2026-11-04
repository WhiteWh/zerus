//! The status item on macOS: Tauri's tray icon carrying Qt's tile and menu
//! (`tray/src/QtTrayIcon.cpp`, `TrayMenu.cpp`). [`Bar`] decides what reaches
//! it: only changes, and nothing while a menu of the app tracks
//! (`QuietTrayIcon.h`). A primary click opens Zerus; the secondary click
//! opens the menu (`QtTrayIcon.cpp:12-20`). The Dock tile shows the attention
//! count (`MacApplicationBadge.mm`).
use std::cell::Cell;
use std::ptr::NonNull;
use std::sync::{Mutex, PoisonError};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSBezierPath, NSBitmapImageRep,
    NSColor, NSDeviceRGBColorSpace, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSGraphicsContext, NSMenuDidBeginTrackingNotification, NSMenuDidEndTrackingNotification,
    NSStringDrawing,
};
use objc2_foundation::{
    ns_string, NSArray, NSAttributedStringKey, NSDictionary, NSDistributedNotificationCenter,
    NSNotification, NSNotificationCenter, NSOperationQueue, NSPoint, NSRect, NSSize, NSString,
};
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};
use zerus_client_core::menubar::{Apply, Entry, Face, OpenMode};
use zerus_client_core::text::Text;
use zerus_client_core::trayicon::{self, Rgb, Step, Tile};

pub use crate::desk::MenuAction;
use crate::desk::{Bar, Item};

/// The tray's id in Tauri.
pub const TRAY: &str = "zerus";
/// 18 points at 2x: tray-icon sizes every image to 18 points high.
pub const TILE_PIXELS: usize = 36;

struct Glue {
    bar: Bar,
    menu: Menu<Wry>,
    /// The "Open in" entries of the menu, so one stays checked.
    open_in: Vec<(CheckMenuItem<Wry>, OpenMode)>,
    configured: Option<Rgb>,
    /// The face and colour the tile was drawn with.
    drawn: Option<(Face, Rgb)>,
}

pub struct MenuBarState(Mutex<Glue>);

fn lock(state: &MenuBarState) -> std::sync::MutexGuard<'_, Glue> {
    state.0.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Creates the status item with an empty fleet's tile and an empty menu;
/// the desk thread's first offer fills both. Qt shows its icon at once, as
/// an empty tray looks like a crashed app (`TrayAgent.cpp:247-253`).
pub fn install(app: &AppHandle, configured: Option<Rgb>) -> tauri::Result<()> {
    let menu = Menu::new(app)?;
    let face = Face::counts(0, 0, false);
    let colour = foreground(configured);
    let blank = || {
        Image::new_owned(
            vec![0; TILE_PIXELS * TILE_PIXELS * 4],
            TILE_PIXELS as u32,
            TILE_PIXELS as u32,
        )
    };
    let first = tile_image(&face, colour);
    let drawn = first.is_some().then_some((face, colour));
    TrayIconBuilder::with_id(TRAY)
        .icon(first.unwrap_or_else(blank))
        .icon_as_template(false)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if let Err(error) = crate::lifecycle::show_main(tray.app_handle()) {
                    eprintln!("zerus-client: could not open the window: {error}");
                }
            }
        })
        .on_menu_event(|app, event| menu_event(app, event.id().as_ref()))
        .build(app)?;
    app.manage(MenuBarState(Mutex::new(Glue {
        bar: Bar::default(),
        menu,
        open_in: Vec::new(),
        configured,
        drawn,
    })));
    observe(app);
    Ok(())
}

/// The desk thread's latest state, on the main thread.
pub fn offer(
    app: &AppHandle,
    face: Face,
    tooltip: String,
    menu: Vec<Entry>,
    badge: Option<String>,
) {
    let Some(state) = app.try_state::<MenuBarState>() else {
        return;
    };
    let offered = lock(&state).bar.offer(face, tooltip, menu, badge);
    if let Some(badge) = offered.badge {
        set_badge(badge.as_deref());
    }
    apply(app, &state, offered.changes);
}

/// `MacApplicationBadge::setCount`: the count, or no badge at zero.
fn set_badge(badge: Option<&str>) {
    if let Some(mtm) = MainThreadMarker::new() {
        let label = badge.map(NSString::from_str);
        NSApplication::sharedApplication(mtm)
            .dockTile()
            .setBadgeLabel(label.as_deref());
    }
}

/// Removes the Dock badge as the app quits (`~MacApplicationBadge`).
pub fn clear_badge(app: &AppHandle) {
    if app.try_state::<MenuBarState>().is_some() {
        set_badge(None);
    }
}

fn apply(app: &AppHandle, state: &MenuBarState, changes: Vec<Apply>) {
    if changes.is_empty() {
        return;
    }
    let Some(tray) = app.tray_by_id(TRAY) else {
        return;
    };
    let mut glue = lock(state);
    for change in changes {
        let result = match change {
            Apply::Face(face) => draw_tile(&tray, &mut glue, face),
            Apply::Tooltip(text) => tray.set_tooltip(Some(text)),
            Apply::Menu(entries) => rebuild(app, &mut glue, &entries),
        };
        if let Err(error) = result {
            eprintln!("zerus-client: menu bar: {error}");
        }
    }
}

/// Draws `face` unless the tile already shows it in the current colour.
fn draw_tile(tray: &tauri::tray::TrayIcon<Wry>, glue: &mut Glue, face: Face) -> tauri::Result<()> {
    let colour = foreground(glue.configured);
    if glue.drawn.as_ref() == Some(&(face.clone(), colour)) {
        return Ok(());
    }
    let Some(image) = tile_image(&face, colour) else {
        return Ok(());
    };
    tray.set_icon(Some(image))?;
    glue.drawn = Some((face, colour));
    Ok(())
}

/// Rebuilds the one menu in place, so the status item keeps its NSMenu.
fn rebuild(app: &AppHandle, glue: &mut Glue, entries: &[Entry]) -> tauri::Result<()> {
    let items = glue.bar.layout(entries);
    // From the end: nothing shifts (`QMenu::clear`).
    for position in (0..glue.menu.items()?.len()).rev() {
        glue.menu.remove_at(position)?;
    }
    glue.open_in.clear();
    for item in items {
        match item {
            Item::Action { id, text } => {
                glue.menu
                    .append(&MenuItem::with_id(app, id, text, true, None::<&str>)?)?
            }
            Item::Label { id, text } => {
                glue.menu
                    .append(&MenuItem::with_id(app, id, text, false, None::<&str>)?)?
            }
            Item::Separator => glue.menu.append(&PredefinedMenuItem::separator(app)?)?,
            Item::OpenIn { id, modes } => {
                let title = crate::native_text::render(&zerus_client_core::text!("Open in"));
                let submenu = Submenu::with_id(app, id, title, true)?;
                for (id, mode, checked) in modes {
                    let label = crate::native_text::render(&Text::new(mode.label()));
                    let check =
                        CheckMenuItem::with_id(app, id, label, true, checked, None::<&str>)?;
                    submenu.append(&check)?;
                    glue.open_in.push((check, mode));
                }
                glue.menu.append(&submenu)?;
            }
        }
    }
    Ok(())
}

pub fn menu_event(app: &AppHandle, id: &str) {
    let Some(state) = app.try_state::<MenuBarState>() else {
        return;
    };
    let action = lock(&state).bar.action(id);
    match action {
        Some(MenuAction::Open) => {
            if let Err(error) = crate::lifecycle::show_main(app) {
                eprintln!("zerus-client: could not open the window: {error}");
            }
        }
        Some(MenuAction::Updates) => crate::updates::open_page(app),
        Some(MenuAction::Restart) => {
            if let Err(error) = crate::updates::restart_app(app) {
                eprintln!("zerus-client: could not restart into the installed bundle: {error}");
            }
        }
        Some(MenuAction::Session {
            computer,
            key,
            running: false,
            ..
        }) => crate::desk::open_session(app, &computer, &key),
        Some(MenuAction::Session {
            computer,
            name,
            running: true,
            ..
        }) => crate::desk::open_running(app, &computer, &name),
        Some(MenuAction::Mode(mode)) => {
            // A check item toggles itself on a click; Qt's exclusive group
            // keeps exactly the chosen mode checked, the same one clicked again too.
            for (item, shown) in &lock(&state).open_in {
                let _ = item.set_checked(*shown == mode);
            }
            crate::desk::set_mode(app, mode);
        }
        Some(MenuAction::Quit) => crate::exit_flush::quit_with_flush(app, 0),
        Some(MenuAction::Refresh) => {}
        None => {}
    }
}

/// Runs `task` on the main queue once the current callback has returned.
/// Tauri runs a main-thread task at once when it is asked from the main
/// thread, so it cannot defer one.
fn later(app: &AppHandle, task: impl FnOnce(&AppHandle) + Send + 'static) {
    let handle = app.clone();
    let task = Cell::new(Some(task));
    let block = RcBlock::new(move || {
        if let Some(task) = task.take() {
            task(&handle);
        }
    });
    // SAFETY: the block runs once, on the main queue, and captures only an
    // AppHandle and a Cell that it takes the task from.
    unsafe { NSOperationQueue::mainQueue().addOperationWithBlock(&block) };
}

/// NSMenu posts the tracking notifications around every menu of the app,
/// the status item's included; another menu only delays an update. A
/// theme change redraws the tile, as Qt follows the application palette on
/// each refresh (`TrayAgent::refreshIcon`). The observers live as long as
/// the app.
fn observe(app: &AppHandle) {
    let began_app = app.clone();
    let began = RcBlock::new(move |_note: NonNull<NSNotification>| {
        if let Some(state) = began_app.try_state::<MenuBarState>() {
            lock(&state).bar.began();
        }
    });
    let ended_app = app.clone();
    let ended = RcBlock::new(move |_note: NonNull<NSNotification>| {
        let Some(state) = ended_app.try_state::<MenuBarState>() else {
            return;
        };
        if lock(&state).bar.ended() {
            // Once this notification has returned: still later than Qt,
            // whose quiet icon flushes in aboutToHide (`QuietTrayIcon.cpp:44-47`).
            later(&ended_app, |handle| {
                if let Some(state) = handle.try_state::<MenuBarState>() {
                    let changes = lock(&state).bar.settle();
                    apply(handle, &state, changes);
                    redraw(handle, &state);
                }
            });
        }
    });
    let theme_app = app.clone();
    let theme = RcBlock::new(move |_note: NonNull<NSNotification>| {
        // The application's appearance follows on a later turn.
        later(&theme_app, |handle| {
            if let Some(state) = handle.try_state::<MenuBarState>() {
                redraw(handle, &state);
            }
        });
    });
    let center = NSNotificationCenter::defaultCenter();
    let distributed = NSDistributedNotificationCenter::defaultCenter();
    let main = NSOperationQueue::mainQueue();
    // SAFETY: AppKit's constant notification names; no object filter; the
    // blocks run on the main queue or the posting (main) thread and capture
    // only Send state.
    unsafe {
        let observers = [
            center.addObserverForName_object_queue_usingBlock(
                Some(NSMenuDidBeginTrackingNotification),
                None,
                None,
                &began,
            ),
            center.addObserverForName_object_queue_usingBlock(
                Some(NSMenuDidEndTrackingNotification),
                None,
                None,
                &ended,
            ),
            distributed.addObserverForName_object_queue_usingBlock(
                Some(ns_string!("AppleInterfaceThemeChangedNotification")),
                None,
                Some(&main),
                &theme,
            ),
        ];
        std::mem::forget(observers);
    }
}

/// Draws the tile shown in the current colour, unless a menu tracks.
fn redraw(app: &AppHandle, state: &MenuBarState) {
    let Some(tray) = app.tray_by_id(TRAY) else {
        return;
    };
    let mut glue = lock(state);
    if !glue.bar.quiet() {
        return;
    }
    if let Some((face, _)) = glue.drawn.clone() {
        if let Err(error) = draw_tile(&tray, &mut glue, face) {
            eprintln!("zerus-client: menu bar: {error}");
        }
    }
}

fn foreground(configured: Option<Rgb>) -> Rgb {
    trayicon::foreground(
        MainThreadMarker::new().is_some_and(dark_appearance),
        configured,
    )
}

fn tile_image(face: &Face, foreground: Rgb) -> Option<Image<'static>> {
    let tile = trayicon::tile(face, TILE_PIXELS as f64, foreground);
    Some(Image::new_owned(
        render(&tile)?,
        TILE_PIXELS as u32,
        TILE_PIXELS as u32,
    ))
}

/// Qt follows the application palette (`IconFactory::foreground`).
fn dark_appearance(mtm: MainThreadMarker) -> bool {
    let appearance = NSApplication::sharedApplication(mtm).effectiveAppearance();
    // SAFETY: AppKit's constant appearance names.
    let (aqua, dark) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
    appearance
        .bestMatchFromAppearancesWithNames(&NSArray::from_slice(&[aqua, dark]))
        .is_some_and(|name| name.isEqualToString(dark))
}

/// Draws the tile into a fresh bitmap and returns straight RGBA, top row first.
pub fn render(tile: &Tile) -> Option<Vec<u8>> {
    let pixels = tile.size as isize;
    // SAFETY: a bitmap AppKit allocates for our size (null planes); we read
    // its rows after drawing, each `bytesPerRow` apart.
    unsafe {
        let rep = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(), std::ptr::null_mut(), pixels, pixels, 8, 4, true, false, NSDeviceRGBColorSpace, pixels * 4, 32,
        )?;
        let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&context));
        draw(tile);
        context.flushGraphics();
        NSGraphicsContext::restoreGraphicsState_class();
        let data = rep.bitmapData();
        if data.is_null() {
            return None;
        }
        let (stride, width) = (rep.bytesPerRow() as usize, tile.size as usize);
        let mut rgba = Vec::with_capacity(width * width * 4);
        for row in 0..width {
            rgba.extend_from_slice(std::slice::from_raw_parts(
                data.add(row * stride),
                width * 4,
            ));
        }
        trayicon::unpremultiply(&mut rgba);
        Some(rgba)
    }
}

/// AppKit's y axis points up; the tile's geometry points down.
fn draw(tile: &Tile) {
    let size = tile.size;
    let rect =
        |r: trayicon::Rect| NSRect::new(NSPoint::new(r.x, size - r.y - r.h), NSSize::new(r.w, r.h));
    fill_rounded(rect(tile.upper), tile.radius, tile.surface);
    fill_rounded(rect(tile.lower), tile.radius, tile.lower_fill);
    if let Some(mark) = tile.mark {
        let point = |(x, y): (f64, f64)| {
            NSPoint::new(
                mark.x + x * mark.w / 100.0,
                size - (mark.y + y * mark.h / 100.0),
            )
        };
        let path = NSBezierPath::bezierPath();
        for step in trayicon::mark_path() {
            match step {
                Step::Move(x, y) => path.moveToPoint(point((x, y))),
                Step::Curve { c1, c2, to } => {
                    path.curveToPoint_controlPoint1_controlPoint2(point(to), point(c1), point(c2))
                }
                Step::Close => path.closePath(),
            }
        }
        color(tile.foreground).setFill();
        path.fill();
    }
    draw_text(&tile.total, tile.font_pixels, size);
    draw_text(&tile.attention, tile.font_pixels, size);
    if let Some((x, y, r)) = tile.dot {
        color(tile.foreground).setFill();
        NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
            NSPoint::new(x - r, size - y - r),
            NSSize::new(2.0 * r, 2.0 * r),
        ))
        .fill();
    }
}

/// Bold, shrunk until the count fits its box, centred on the cap height
/// (`IconFactory.cpp:130-141`).
fn draw_text(text: &trayicon::Text, start_pixels: f64, size: f64) {
    let string = NSString::from_str(&text.text);
    let mut pixels = start_pixels;
    loop {
        let font = NSFont::boldSystemFontOfSize(pixels);
        let attributes = attributes(&font, text.color);
        // SAFETY: the dictionary holds a font and a colour under their keys.
        let width = unsafe { string.sizeWithAttributes(Some(&attributes)) }.width;
        if (width <= text.rect.w && font.capHeight() <= text.rect.h) || pixels <= 5.0 {
            let x = text.rect.x + (text.rect.w - width) / 2.0;
            let baseline = size - (text.rect.y + (text.rect.h + font.capHeight()) / 2.0);
            // SAFETY: as above.
            unsafe {
                string.drawAtPoint_withAttributes(
                    NSPoint::new(x, baseline + font.descender()),
                    Some(&attributes),
                )
            };
            return;
        }
        pixels -= 1.0;
    }
}

fn attributes(font: &NSFont, rgb: Rgb) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
    let color = color(rgb);
    // SAFETY: AppKit's constant attribute names.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let values: [&AnyObject; 2] = [font, &color];
    NSDictionary::from_slices(&keys, &values)
}

fn color(rgb: Rgb) -> Retained<NSColor> {
    let channel = |value: u8| f64::from(value) / 255.0;
    NSColor::colorWithSRGBRed_green_blue_alpha(
        channel(rgb.r),
        channel(rgb.g),
        channel(rgb.b),
        channel(rgb.a),
    )
}

fn fill_rounded(rect: NSRect, radius: f64, rgb: Rgb) {
    color(rgb).setFill();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, radius, radius).fill();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tile_draws_amber_attention_and_the_stale_dot() {
        let tile = trayicon::tile(
            &Face::counts(12, 3, true),
            TILE_PIXELS as f64,
            Rgb::new(255, 255, 255),
        );
        let rgba = render(&tile).expect("AppKit draws into a bitmap without a display");
        assert_eq!(rgba.len(), TILE_PIXELS * TILE_PIXELS * 4);
        let pixel = |x: usize, y: usize| rgba[(y * TILE_PIXELS + x) * 4..][..4].to_vec();
        let amber = pixel(3, 33);
        assert!(
            amber[0] > 200 && amber[1] > 150 && amber[2] < 150 && amber[3] == 255,
            "{amber:?}"
        );
        assert!(
            pixel(TILE_PIXELS - 2, 2)[0] > 200,
            "the white dot on the dark surface"
        );
        assert_eq!(pixel(18, 18)[3], 0, "the gap between the rows stays clear");
    }
}
