//! Soft glow flashed at the screen edge where the cursor crosses over to
//! (or arrives from) another computer.
//!
//! The service reports the crossing via [`lan_mouse_ipc::FrontendEvent::EdgeFlash`],
//! the glow itself is drawn here in a transparent click-through window.
//! Only implemented for Windows and macOS, a no-op elsewhere.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use input_event::edge::Edge;
use lan_mouse_ipc::Position;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "windows", target_os = "macos", test))]
mod render;
#[cfg(target_os = "windows")]
mod windows;

/// flashes at the same edge closer than this are ignored
const DEBOUNCE: Duration = Duration::from_millis(120);

static LAST_FLASH: Mutex<Option<(Edge, Instant)>> = Mutex::new(None);

/// Flash the glow at the local edge `pos`, `fraction` (0..1) along it.
pub fn flash(pos: Position, fraction: f64) {
    if !fraction.is_finite() {
        return;
    }
    let edge = to_edge(pos);
    let fraction = fraction.clamp(0.0, 1.0);

    {
        let mut last = LAST_FLASH.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if let Some((last_edge, at)) = *last {
            if last_edge == edge && now.duration_since(at) < DEBOUNCE {
                return;
            }
        }
        *last = Some((edge, now));
    }

    #[cfg(target_os = "windows")]
    windows::flash(edge, fraction);
    #[cfg(target_os = "macos")]
    macos::flash(edge, fraction);
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let _ = (edge, fraction);
}

fn to_edge(pos: Position) -> Edge {
    match pos {
        Position::Left => Edge::Left,
        Position::Right => Edge::Right,
        Position::Top => Edge::Top,
        Position::Bottom => Edge::Bottom,
    }
}
