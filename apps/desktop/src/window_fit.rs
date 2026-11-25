//! Window size and position (#172). The window must always fit the monitor's *work area*:
//! the part of the screen not taken by the Windows taskbar, the macOS menu bar and Dock,
//! or a Linux panel. On start the app either restores where the user left it (per monitor)
//! or fits a default size; both are clamped here, so a disconnected monitor, a DPI change
//! or a taller default can never put the window, or its message box, out of reach.
//!
//! Everything here is pure geometry in physical pixels, so it is tested with fake work
//! areas. `lib.rs` reads the real monitors and applies the result.

use serde::{Deserialize, Serialize};

/// The default inner size, in logical pixels, before it is fitted to the work area.
pub const DEFAULT_SIZE: (f64, f64) = (1280.0, 820.0);
/// The smallest inner size, in logical pixels, at which the layout still works (the
/// sidebar, the chat and the message box are all visible). Mirrors `minWidth` /
/// `minHeight` in `tauri.conf.json`.
pub const MIN_SIZE: (f64, f64) = (720.0, 480.0);
/// Space left around a fitted default window, in logical pixels.
pub const MARGIN: f64 = 24.0;

/// A rectangle in physical pixels (a monitor's work area, or a window's outer frame).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// A monitor as the fitting logic sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Screen {
    /// The OS's name for the monitor, if it gives one; what a saved position is keyed on.
    pub name: Option<String>,
    pub work_area: Rect,
    pub scale: f64,
}

/// Where the window was, saved per monitor. Position is relative to that monitor's work
/// area and, like the size, in logical pixels, so a DPI change rescales it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub monitor: Option<String>,
    pub x: f64,
    pub y: f64,
    /// Outer size (including the title bar), logical.
    pub w: f64,
    pub h: f64,
    pub maximized: bool,
}

/// Where to put the window, and whether to maximise it afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub screen: usize,
    /// Outer frame, physical pixels.
    pub frame: Rect,
    pub maximized: bool,
}

/// The size of the window's frame (title bar and borders), physical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Decorations {
    pub w: u32,
    pub h: u32,
}

fn px(logical: f64, scale: f64) -> u32 {
    (logical * scale).round().max(0.0) as u32
}

/// The outer minimum size on a screen: the minimum inner size plus the frame, but never
/// more than the work area (a tiny screen gets the whole work area instead).
fn min_outer(s: &Screen, deco: Decorations) -> (u32, u32) {
    (
        (px(MIN_SIZE.0, s.scale) + deco.w).min(s.work_area.w),
        (px(MIN_SIZE.1, s.scale) + deco.h).min(s.work_area.h),
    )
}

/// Clamps an outer frame into a work area: no larger than it, no smaller than the minimum
/// (unless the work area itself is smaller), and moved so every edge is inside it.
pub fn clamp(frame: Rect, s: &Screen, deco: Decorations) -> Rect {
    let area = s.work_area;
    let (min_w, min_h) = min_outer(s, deco);
    let w = frame.w.clamp(min_w, area.w);
    let h = frame.h.clamp(min_h, area.h);
    let max_x = area.x + (area.w - w) as i32;
    let max_y = area.y + (area.h - h) as i32;
    Rect {
        x: frame.x.clamp(area.x, max_x),
        y: frame.y.clamp(area.y, max_y),
        w,
        h,
    }
}

/// The default window on a screen: the default size plus the frame, shrunk to leave
/// [`MARGIN`] inside the work area, centred in it.
pub fn fit_default(s: &Screen, deco: Decorations) -> Rect {
    let area = s.work_area;
    let margin = px(MARGIN, s.scale);
    let room_w = area.w.saturating_sub(2 * margin);
    let room_h = area.h.saturating_sub(2 * margin);
    let w = (px(DEFAULT_SIZE.0, s.scale) + deco.w).min(room_w);
    let h = (px(DEFAULT_SIZE.1, s.scale) + deco.h).min(room_h);
    let centred = Rect {
        x: area.x + (area.w.saturating_sub(w) / 2) as i32,
        y: area.y + (area.h.saturating_sub(h) / 2) as i32,
        w,
        h,
    };
    clamp(centred, s, deco)
}

/// Where the window goes on start. `saved` restores on the monitor it was on (matched by
/// name) when that monitor is still there; otherwise the saved size is kept but centred on
/// `fallback` (the primary or current monitor). With nothing saved, the default is fitted.
/// Every result is clamped to its work area.
pub fn place(
    screens: &[Screen],
    fallback: usize,
    saved: Option<&Saved>,
    deco: Decorations,
) -> Option<Placement> {
    let fallback = fallback.min(screens.len().checked_sub(1)?);
    let Some(saved) = saved.filter(|s| s.w > 0.0 && s.h > 0.0) else {
        let s = &screens[fallback];
        return Some(Placement {
            screen: fallback,
            frame: fit_default(s, deco),
            maximized: false,
        });
    };
    let same = saved
        .monitor
        .as_ref()
        .and_then(|name| screens.iter().position(|s| s.name.as_ref() == Some(name)));
    let (i, frame) = match same {
        Some(i) => {
            let s = &screens[i];
            let a = s.work_area;
            let frame = Rect {
                x: a.x + (saved.x * s.scale).round() as i32,
                y: a.y + (saved.y * s.scale).round() as i32,
                w: px(saved.w, s.scale),
                h: px(saved.h, s.scale),
            };
            (i, frame)
        }
        None => {
            // The monitor is gone: keep the size, centre it where the window opens now.
            let s = &screens[fallback];
            let a = s.work_area;
            let (w, h) = (px(saved.w, s.scale), px(saved.h, s.scale));
            let frame = Rect {
                x: a.x + (a.w.saturating_sub(w.min(a.w)) / 2) as i32,
                y: a.y + (a.h.saturating_sub(h.min(a.h)) / 2) as i32,
                w,
                h,
            };
            (fallback, frame)
        }
    };
    Some(Placement {
        screen: i,
        frame: clamp(frame, &screens[i], deco),
        maximized: saved.maximized,
    })
}

/// The monitor a frame is mostly on: the one whose work area overlaps it most, or the
/// one nearest its centre when it overlaps none.
pub fn screen_of(frame: Rect, screens: &[Screen]) -> Option<usize> {
    let overlap = |a: Rect| -> i64 {
        let x0 = frame.x.max(a.x) as i64;
        let y0 = frame.y.max(a.y) as i64;
        let x1 = (frame.x as i64 + frame.w as i64).min(a.x as i64 + a.w as i64);
        let y1 = (frame.y as i64 + frame.h as i64).min(a.y as i64 + a.h as i64);
        (x1 - x0).max(0) * (y1 - y0).max(0)
    };
    let centre = |r: Rect| (r.x as i64 + r.w as i64 / 2, r.y as i64 + r.h as i64 / 2);
    let (cx, cy) = centre(frame);
    screens
        .iter()
        .enumerate()
        .max_by_key(|(_, s)| {
            let (sx, sy) = centre(s.work_area);
            (overlap(s.work_area), -((sx - cx).pow(2) + (sy - cy).pow(2)))
        })
        .map(|(i, _)| i)
}

/// What to save for a window with this outer frame on these screens.
pub fn save(frame: Rect, maximized: bool, screens: &[Screen]) -> Option<Saved> {
    let s = &screens[screen_of(frame, screens)?];
    let a = s.work_area;
    Some(Saved {
        monitor: s.name.clone(),
        x: f64::from(frame.x - a.x) / s.scale,
        y: f64::from(frame.y - a.y) / s.scale,
        w: f64::from(frame.w) / s.scale,
        h: f64::from(frame.h) / s.scale,
        maximized,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECO: Decorations = Decorations { w: 0, h: 32 };

    fn screen(name: &str, x: i32, y: i32, w: u32, h: u32, scale: f64) -> Screen {
        Screen {
            name: Some(name.into()),
            work_area: Rect { x, y, w, h },
            scale,
        }
    }

    fn inside(f: Rect, s: &Screen) -> bool {
        let a = s.work_area;
        f.x >= a.x
            && f.y >= a.y
            && f.x + f.w as i32 <= a.x + a.w as i32
            && f.y + f.h as i32 <= a.y + a.h as i32
    }

    #[test]
    fn the_default_never_reaches_under_a_windows_taskbar() {
        // 1920×1080 at 150 %: a 48 px taskbar at the bottom leaves a 1032 px tall work
        // area, which is only 688 logical px: less than the 820 px default.
        let s = screen("\\\\.\\DISPLAY1", 0, 0, 1920, 1032, 1.5);
        let f = fit_default(&s, DECO);
        assert!(inside(f, &s), "{f:?}");
        assert_eq!(
            f.y + f.h as i32,
            1032 - 36,
            "a 24 px (logical) margin to the taskbar"
        );
        assert_eq!(f.h, 1032 - 2 * 36);
        // Centred horizontally, full default width (1280 × 1.5 = 1920 > room): shrunk too.
        assert_eq!(f.w, 1920 - 2 * 36);
    }

    #[test]
    fn taskbars_on_the_side_or_top_and_the_macos_menu_bar_and_dock() {
        for s in [
            // Windows taskbar on the left (work area starts at x = 62).
            screen("left", 62, 0, 1858, 1080, 1.0),
            // On top (starts at y = 48).
            screen("top", 0, 48, 1920, 1032, 1.0),
            // macOS 1440×900: menu bar 25 pt on top, Dock 70 pt at the bottom, Retina.
            screen("Built-in Retina Display", 0, 50, 2880, 1610, 2.0),
            // A small laptop: 1366×768 at 125 %, 40 px taskbar.
            screen("laptop", 0, 0, 1366, 728, 1.25),
        ] {
            let f = fit_default(&s, DECO);
            assert!(inside(f, &s), "{} {f:?}", s.name.as_deref().unwrap());
            let (min_w, min_h) = min_outer(&s, DECO);
            assert!(f.w >= min_w && f.h >= min_h, "at least the minimum: {f:?}");
        }
    }

    #[test]
    fn a_big_screen_gets_the_default_size_centred() {
        let s = screen("big", 0, 0, 2560, 1400, 1.0);
        let f = fit_default(&s, DECO);
        assert_eq!((f.w, f.h), (1280, 820 + 32));
        assert_eq!((f.x, f.y), ((2560 - 1280) / 2, (1400 - 852) / 2));
    }

    #[test]
    fn a_screen_smaller_than_the_minimum_gets_its_whole_work_area() {
        let s = screen("tiny", 0, 0, 800, 500, 1.0);
        let f = fit_default(&s, DECO);
        assert!(inside(f, &s));
        // The width keeps its margin; the height is below the minimum with one, so the
        // minimum wins and the window takes the whole work-area height.
        assert_eq!((f.w, f.h), (800 - 48, 500));
        let big = Rect {
            x: -50,
            y: -50,
            w: 5000,
            h: 5000,
        };
        assert_eq!(
            clamp(big, &s, DECO),
            Rect {
                x: 0,
                y: 0,
                w: 800,
                h: 500
            }
        );
    }

    #[test]
    fn restoring_puts_the_window_back_on_its_monitor() {
        let screens = [
            screen("primary", 0, 0, 1920, 1040, 1.0),
            screen("right", 1920, 0, 2560, 1400, 1.0),
        ];
        let frame = Rect {
            x: 2100,
            y: 100,
            w: 1400,
            h: 900,
        };
        let saved = save(frame, false, &screens).unwrap();
        assert_eq!(saved.monitor.as_deref(), Some("right"));
        let p = place(&screens, 0, Some(&saved), DECO).unwrap();
        assert_eq!(p.screen, 1);
        assert_eq!(p.frame, frame);
        assert!(!p.maximized);
    }

    #[test]
    fn a_missing_monitor_or_an_off_screen_position_is_pulled_back_in() {
        let primary = screen("primary", 0, 0, 1920, 1040, 1.0);
        // Saved on a monitor that's no longer connected.
        let gone = Saved {
            monitor: Some("unplugged".into()),
            x: 3000.0,
            y: 2000.0,
            w: 1400.0,
            h: 900.0,
            maximized: true,
        };
        let p = place(std::slice::from_ref(&primary), 0, Some(&gone), DECO).unwrap();
        assert!(inside(p.frame, &primary), "{:?}", p.frame);
        assert_eq!((p.frame.w, p.frame.h), (1400, 900), "the size is kept");
        assert!(p.maximized, "maximised state is restored");
        // Same monitor, but the window was left hanging off the bottom-right corner.
        let off = Saved {
            monitor: Some("primary".into()),
            x: 1800.0,
            y: 1000.0,
            w: 1400.0,
            h: 900.0,
            maximized: false,
        };
        let p = place(std::slice::from_ref(&primary), 0, Some(&off), DECO).unwrap();
        assert!(inside(p.frame, &primary), "{:?}", p.frame);
    }

    #[test]
    fn a_dpi_change_rescales_and_still_fits() {
        // Saved at 100 % on a 1920×1040 work area, filling most of it…
        let at_100 = screen("main", 0, 0, 1920, 1040, 1.0);
        let saved = save(
            Rect {
                x: 20,
                y: 20,
                w: 1880,
                h: 1000,
            },
            false,
            &[at_100],
        )
        .unwrap();
        // …restored after switching the same monitor to 150 % (work area now 1280×693 logical).
        let at_150 = screen("main", 0, 0, 1920, 1040, 1.5);
        let p = place(std::slice::from_ref(&at_150), 0, Some(&saved), DECO).unwrap();
        assert!(inside(p.frame, &at_150), "{:?}", p.frame);
        assert_eq!(
            (p.frame.w, p.frame.h),
            (1920, 1040),
            "clamped to the work area"
        );
    }

    #[test]
    fn no_saved_state_or_a_broken_one_fits_the_default() {
        let s = screen("primary", 0, 0, 1920, 1040, 1.0);
        let p = place(std::slice::from_ref(&s), 0, None, DECO).unwrap();
        assert_eq!(p.frame, fit_default(&s, DECO));
        let zero = Saved::default();
        assert_eq!(
            place(std::slice::from_ref(&s), 0, Some(&zero), DECO)
                .unwrap()
                .frame,
            fit_default(&s, DECO)
        );
        assert_eq!(
            place(&[], 0, None, DECO),
            None,
            "no monitors: leave the window alone"
        );
    }

    #[test]
    fn the_saved_monitor_is_the_one_the_window_is_mostly_on() {
        let screens = [
            screen("a", 0, 0, 1920, 1040, 1.0),
            screen("b", 1920, 0, 1920, 1040, 1.0),
        ];
        // Straddling, but mostly on b.
        let f = Rect {
            x: 1700,
            y: 0,
            w: 1000,
            h: 800,
        };
        assert_eq!(screen_of(f, &screens), Some(1));
        // Overlapping neither: the nearest.
        let f = Rect {
            x: 5000,
            y: 0,
            w: 100,
            h: 100,
        };
        assert_eq!(screen_of(f, &screens), Some(1));
    }

    #[test]
    fn the_config_matches_these_sizes_and_starts_hidden() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        let w = &conf["app"]["windows"][0];
        assert_eq!(w["label"], "main");
        assert_eq!(w["minWidth"].as_f64(), Some(MIN_SIZE.0));
        assert_eq!(w["minHeight"].as_f64(), Some(MIN_SIZE.1));
        assert_eq!(w["width"].as_f64(), Some(DEFAULT_SIZE.0));
        assert_eq!(w["height"].as_f64(), Some(DEFAULT_SIZE.1));
        // Shown only after it has been fitted, so it never flashes at the wrong size.
        assert_eq!(w["visible"], false);
    }
}
