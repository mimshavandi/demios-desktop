//! Applies [`crate::window_fit`] to the real main window (#172): reads the monitors' work
//! areas, places the hidden window before showing it, follows its normal (not maximised)
//! frame while it moves, and saves it when the window closes or the app quits.

use crate::state::AppState;
use crate::window_fit::{self, Decorations, Rect, Saved, Screen};
use std::sync::Mutex;
use tauri::{
    Manager, PhysicalPosition, PhysicalSize, Runtime, WebviewWindow, WebviewWindowBuilder, Window,
    WindowEvent,
};

pub const MAIN: &str = "main";

/// What to save for the main window, kept up to date while it moves and resizes: its last
/// normal (not maximised or minimised) frame, and whether it is maximised now. Saving then
/// needs no window, which on macOS is already gone by the time the app quits (⌘Q).
#[derive(Default)]
pub struct Tracker(Mutex<Option<Saved>>);

impl Tracker {
    fn get(&self) -> Option<Saved> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
    fn set(&self, s: Saved) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(s);
    }
}

fn screens<R: Runtime>(w: &WebviewWindow<R>) -> tauri::Result<(Vec<Screen>, usize)> {
    let monitors = w.available_monitors()?;
    let to_screen = |m: &tauri::Monitor| {
        let a = m.work_area();
        Screen {
            name: m.name().cloned(),
            work_area: Rect {
                x: a.position.x,
                y: a.position.y,
                w: a.size.width,
                h: a.size.height,
            },
            scale: m.scale_factor(),
        }
    };
    let list: Vec<Screen> = monitors.iter().map(to_screen).collect();
    // Where a new window would open: the monitor it was created on, else the primary.
    let here = w
        .current_monitor()?
        .or(w.primary_monitor()?)
        .and_then(|m| list.iter().position(|s| s.name.as_ref() == m.name()))
        .unwrap_or(0);
    Ok((list, here))
}

fn frame<R: Runtime>(w: &WebviewWindow<R>) -> tauri::Result<Rect> {
    let p = w.outer_position()?;
    let s = w.outer_size()?;
    Ok(Rect {
        x: p.x,
        y: p.y,
        w: s.width,
        h: s.height,
    })
}

/// Creates the main window hidden, places it, then shows it (#195). The window is
/// `create: false` in every config, so Tauri doesn't build it before setup: it would
/// load the page while the database opens, and a later placement would undo any resize
/// made meanwhile. Here nothing can resize it, and the page can't load, before it is placed.
///
/// A failure to read the monitors is logged and the window is shown where the OS put
/// it: it must still appear. A failure to create it is an error.
pub fn open<R: Runtime>(app: &tauri::AppHandle<R>, state: &AppState) -> tauri::Result<()> {
    let Some(config) = app.config().app.windows.iter().find(|w| w.label == MAIN) else {
        return Err(tauri::Error::WindowNotFound);
    };
    let w = WebviewWindowBuilder::from_config(app, config)?
        .visible(false)
        .build()?;
    if let Err(e) = place(&w, state) {
        eprintln!("could not fit the window to the screen: {e}");
    }
    if let Err(e) = w.show().and_then(|()| w.set_focus()) {
        eprintln!("could not show the window: {e}");
    }
    Ok(())
}

fn place<R: Runtime>(w: &WebviewWindow<R>, state: &AppState) -> Result<(), String> {
    let saved = state
        .db()
        .map_err(|e| e.message)
        .and_then(|db| crate::settings::load_window(&db).map_err(|e| e.to_string()))
        .unwrap_or_else(|e| {
            eprintln!("could not read the saved window position: {e}");
            None
        });
    let (list, here) = screens(w).map_err(|e| e.to_string())?;
    let outer = w.outer_size().map_err(|e| e.to_string())?;
    let inner = w.inner_size().map_err(|e| e.to_string())?;
    let deco = Decorations {
        w: outer.width.saturating_sub(inner.width),
        h: outer.height.saturating_sub(inner.height),
    };
    let Some(p) = window_fit::place(&list, here, saved.as_ref(), deco) else {
        return Err("no monitors reported".into());
    };
    let f = p.frame;
    #[cfg(debug_assertions)]
    eprintln!(
        "window: {:?} on {:?} (work area {:?}, scale {}), saved {:?}",
        p, list[p.screen].name, list[p.screen].work_area, list[p.screen].scale, saved
    );
    // Move first, then size, then move again: crossing to a monitor with another scale
    // makes Windows rescale the window, so it is sized where it ends up (and the second
    // move fixes the position if the resize shifted it).
    let at = PhysicalPosition::new(f.x, f.y);
    w.set_position(at).map_err(|e| e.to_string())?;
    w.set_size(PhysicalSize::new(
        f.w.saturating_sub(deco.w),
        f.h.saturating_sub(deco.h),
    ))
    .map_err(|e| e.to_string())?;
    w.set_position(at).map_err(|e| e.to_string())?;
    if let (Some(t), Some(s)) = (
        w.try_state::<Tracker>(),
        window_fit::save(f, p.maximized, &list),
    ) {
        t.set(s);
    }
    if p.maximized {
        w.maximize().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Follows the main window's normal frame; saves on close.
pub fn on_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    if window.label() != MAIN {
        return;
    }
    let Some(w) = window.get_webview_window(MAIN) else {
        return;
    };
    match event {
        WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
            let Some(t) = w.try_state::<Tracker>() else {
                return;
            };
            let maximized = w.is_maximized().unwrap_or(false);
            if w.is_minimized().unwrap_or(false) {
                return;
            }
            if maximized {
                // Keep the frame it restores to; only the flag changes.
                if let Some(mut s) = t.get() {
                    s.maximized = true;
                    t.set(s);
                }
            } else if let (Ok(f), Ok((list, _))) = (frame(&w), screens(&w))
                && let Some(s) = window_fit::save(f, false, &list)
            {
                t.set(s);
            }
        }
        WindowEvent::CloseRequested { .. } => persist(window.app_handle()),
        _ => {}
    }
}

/// Writes what the tracker holds. Called when the window closes and when the app quits.
pub fn persist<R: Runtime>(app: &tauri::AppHandle<R>) {
    let (Some(t), Some(state)) = (
        app.try_state::<Tracker>(),
        app.try_state::<std::sync::Arc<AppState>>(),
    ) else {
        return;
    };
    let Some(saved) = t.get() else {
        return;
    };
    match state.db() {
        Ok(db) => {
            if let Err(e) = crate::settings::save_window(&db, &saved) {
                eprintln!("could not save the window position: {e}");
            }
        }
        Err(e) => eprintln!("could not save the window position: {}", e.message),
    }
}
