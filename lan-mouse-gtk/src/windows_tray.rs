//! Windows: tray icon + "show window" requests from a second instance.
//! Mirrors `macos_status_item`.

use adw::prelude::*;
use gtk::glib;
use std::time::Duration;

use crate::{
    window::Window,
    windows_single_instance,
    windows_tray_core::{Tray, TrayAction},
};

const POLL_INTERVAL: Duration = Duration::from_millis(100);

fn show(window: &Window) {
    window.set_visible(true);
    window.present();
}

/// Sets up the tray icon (on the GTK main thread) and the listener for a
/// second launch. Returns `false` if the tray could not be created; the
/// caller must then not hide the window on close.
pub fn setup(app: &adw::Application, window: &Window) -> bool {
    // a second launch asks us to show the window
    let (show_tx, show_rx) = async_channel::bounded::<()>(1);
    windows_single_instance::listen_for_show(move || {
        // full channel = a show request is already pending
        let _ = show_tx.try_send(());
    });
    glib::spawn_future_local({
        let window = window.downgrade();
        async move {
            while show_rx.recv().await.is_ok() {
                if let Some(window) = window.upgrade() {
                    show(&window);
                }
            }
        }
    });

    let tray = match Tray::new() {
        Ok(tray) => tray,
        Err(e) => {
            log::error!("could not create tray icon, continuing without: {e}");
            return false;
        }
    };

    // keep the application alive while the window is hidden
    let hold = app.hold();
    let window = window.downgrade();
    let app = app.downgrade();
    // the closure owns the tray and the hold for the lifetime of the app
    glib::timeout_add_local(POLL_INTERVAL, move || {
        let _ = &hold;
        for action in tray.poll() {
            match action {
                TrayAction::Show => {
                    if let Some(window) = window.upgrade() {
                        show(&window);
                    }
                }
                TrayAction::Quit => {
                    if let Some(app) = app.upgrade() {
                        app.quit();
                    }
                }
            }
        }
        glib::ControlFlow::Continue
    });
    true
}
