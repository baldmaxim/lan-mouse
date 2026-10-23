//! Update banner, manual / periodic update checks and the install flow.
//! Network and disk work lives in [`crate::updater`] and runs on std
//! threads; results come back to the GTK thread through channels.

use std::{thread, time::Duration};

use adw::{prelude::*, subclass::prelude::*};
use gtk::{gio, glib, glib::clone};

use super::Window;
use crate::{
    app_version,
    updater::{self, PendingInstall, UpdateError, UpdateInfo},
};

const FIRST_CHECK_DELAY_SECS: u32 = 10;
const CHECK_INTERVAL_SECS: u32 = 6 * 60 * 60;
const MACOS_PERMISSIONS_NOTE: &str =
    "macOS will ask for Accessibility/Input Monitoring permissions again after the update";
/// time to read the macOS note before the app quits
const MACOS_QUIT_DELAY: Duration = Duration::from_secs(5);

enum DownloadEvent {
    /// percent, `None` if the size is unknown
    Progress(Option<u8>),
    Finished(Result<PendingInstall, UpdateError>),
}

impl Window {
    pub(super) fn setup_update_ui(&self) {
        self.imp()
            .update_check_row
            .set_subtitle(&format!("Lan Mouse {}", app_version()));
    }

    /// Checks 10 s after start and then every 6 hours.
    pub(crate) fn start_update_checks(&self) {
        glib::timeout_add_seconds_local_once(
            FIRST_CHECK_DELAY_SECS,
            clone!(
                #[weak(rename_to = window)]
                self,
                move || window.check_for_updates(false)
            ),
        );
        glib::timeout_add_seconds_local(
            CHECK_INTERVAL_SECS,
            clone!(
                #[weak(rename_to = window)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    window.check_for_updates(false);
                    glib::ControlFlow::Continue
                }
            ),
        );
    }

    /// Version of the update found by the last check, if any.
    #[cfg(target_os = "windows")]
    pub(crate) fn available_update_version(&self) -> Option<String> {
        self.imp()
            .update_info
            .borrow()
            .as_ref()
            .map(|info| info.version.clone())
    }

    /// Runs an update check. `manual` checks report their outcome in a toast,
    /// automatic ones only show the banner when an update is found.
    pub(crate) fn check_for_updates(&self, manual: bool) {
        let imp = self.imp();
        if imp.update_busy.get() {
            if manual {
                self.show_toast("An update check or download is already running");
            }
            return;
        }
        imp.update_busy.set(true);
        imp.update_check_button.set_sensitive(false);

        let (tx, rx) = async_channel::bounded(1);
        let spawned = thread::Builder::new()
            .name("update-check".into())
            .spawn(move || {
                let _ = tx.send_blocking(updater::check(app_version()));
            });
        if let Err(e) = spawned {
            log::warn!("could not start update check: {e}");
            self.end_update_check(manual, Err(format!("could not start the check: {e}")));
            return;
        }
        glib::spawn_future_local(clone!(
            #[weak(rename_to = window)]
            self,
            async move {
                let result = match rx.recv().await {
                    Ok(result) => result.map_err(|e| e.to_string()),
                    Err(_) => Err("the check stopped unexpectedly".to_string()),
                };
                window.end_update_check(manual, result);
            }
        ));
    }

    fn end_update_check(&self, manual: bool, result: Result<Option<UpdateInfo>, String>) {
        let imp = self.imp();
        imp.update_busy.set(false);
        imp.update_check_button.set_sensitive(true);
        match result {
            Ok(Some(info)) => self.show_update_available(info),
            Ok(None) => {
                imp.update_info.replace(None);
                imp.update_revealer.set_reveal_child(false);
                if manual {
                    self.show_toast(&format!("Lan Mouse {} is up to date", app_version()));
                }
            }
            Err(e) => {
                log::warn!("update check failed: {e}");
                if manual {
                    self.show_toast(&format!("Update check failed: {e}"));
                }
            }
        }
    }

    fn show_update_available(&self, info: UpdateInfo) {
        let imp = self.imp();
        imp.update_label
            .set_label(&format!("Lan Mouse {} is available", info.version));
        let button_label = if info.asset.is_some() {
            "Update"
        } else {
            "Open"
        };
        imp.update_button.set_label(button_label);
        imp.update_button.set_sensitive(true);
        imp.update_info.replace(Some(info));
        imp.update_revealer.set_reveal_child(true);
    }

    /// "Update" / "Open" banner button.
    pub(super) fn start_update(&self) {
        let imp = self.imp();
        if imp.update_busy.get() {
            return;
        }
        let Some(info) = imp.update_info.borrow().clone() else {
            return;
        };
        let Some(asset) = info.asset.clone() else {
            // nothing to install on this platform: show the release page
            if let Err(e) = gio::AppInfo::launch_default_for_uri(
                &info.notes_url,
                None::<&gio::AppLaunchContext>,
            ) {
                self.show_toast(&format!("Could not open {}: {e}", info.notes_url));
            }
            return;
        };

        imp.update_busy.set(true);
        imp.update_button.set_sensitive(false);
        imp.update_check_button.set_sensitive(false);
        self.set_download_progress(Some(0));

        let (tx, rx) = async_channel::unbounded();
        let spawned = thread::Builder::new()
            .name("update-download".into())
            .spawn(move || {
                let mut last_percent = None;
                let result =
                    updater::download_and_verify(&asset, &updater::update_dir(), |done, total| {
                        let percent = total
                            .filter(|t| *t > 0)
                            .map(|t| (done.saturating_mul(100) / t).min(100) as u8);
                        if percent != last_percent {
                            last_percent = percent;
                            let _ = tx.send_blocking(DownloadEvent::Progress(percent));
                        }
                    })
                    .and_then(|package| updater::prepare_install(&package));
                let _ = tx.send_blocking(DownloadEvent::Finished(result));
            });
        if let Err(e) = spawned {
            self.update_failed(&format!("could not start the download: {e}"));
            return;
        }
        glib::spawn_future_local(clone!(
            #[weak(rename_to = window)]
            self,
            async move {
                while let Ok(event) = rx.recv().await {
                    match event {
                        DownloadEvent::Progress(percent) => window.set_download_progress(percent),
                        DownloadEvent::Finished(result) => {
                            match result {
                                Ok(pending) => window.install_update(&pending),
                                Err(e) => window.update_failed(&e.to_string()),
                            }
                            return;
                        }
                    }
                }
                window.update_failed("the download stopped unexpectedly");
            }
        ));
    }

    fn set_download_progress(&self, percent: Option<u8>) {
        let text = match percent {
            Some(p) => format!("Downloading update… {p}%"),
            None => "Downloading update…".to_string(),
        };
        self.imp().update_label.set_label(&text);
    }

    fn install_update(&self, pending: &PendingInstall) {
        if let Err(e) = updater::launch_install(pending) {
            self.update_failed(&e.to_string());
            return;
        }
        let quit_delay = if cfg!(target_os = "macos") {
            self.imp()
                .update_label
                .set_label(&format!("Installing the update… {MACOS_PERMISSIONS_NOTE}"));
            self.show_toast(MACOS_PERMISSIONS_NOTE);
            MACOS_QUIT_DELAY
        } else {
            self.imp()
                .update_label
                .set_label("Installing the update… Lan Mouse will restart");
            Duration::ZERO
        };
        let quit = clone!(
            #[weak(rename_to = window)]
            self,
            move || {
                if let Some(app) = window.application() {
                    app.quit();
                }
            }
        );
        if quit_delay.is_zero() {
            quit();
        } else {
            glib::timeout_add_local_once(quit_delay, quit);
        }
    }

    /// Download / install failed: toast the reason, banner back to "Update".
    fn update_failed(&self, message: &str) {
        log::warn!("update failed: {message}");
        let imp = self.imp();
        imp.update_busy.set(false);
        imp.update_check_button.set_sensitive(true);
        let info = imp.update_info.borrow().clone();
        if let Some(info) = info {
            self.show_update_available(info);
        }
        self.show_toast(&format!("Update failed: {message}"));
    }
}
