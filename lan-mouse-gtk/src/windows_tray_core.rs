//! Tray icon on Windows, independent of GTK.

use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
};

/// 32x32 RGBA, generated from `de.feschber.LanMouse.svg`
const ICON_RGBA: &[u8] = include_bytes!("../resources/tray-icon-32.rgba");
const ICON_SIZE: u32 = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayAction {
    Show,
    CheckUpdates,
    Quit,
}

pub struct Tray {
    /// keeps the icon alive; it is removed on drop
    icon: TrayIcon,
    show_id: MenuId,
    check_updates_id: MenuId,
    quit_id: MenuId,
}

impl Tray {
    pub fn new() -> Result<Self, String> {
        let show = MenuItem::new("Open Lan Mouse", true, None);
        let check_updates = MenuItem::new("Check for updates", true, None);
        let quit = MenuItem::new("Quit", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &show,
            &check_updates,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(|e| format!("tray menu: {e}"))?;
        let icon = Icon::from_rgba(ICON_RGBA.to_vec(), ICON_SIZE, ICON_SIZE)
            .map_err(|e| format!("tray icon: {e}"))?;
        let tray = TrayIconBuilder::new()
            .with_tooltip("Lan Mouse")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| format!("tray creation: {e}"))?;
        Ok(Self {
            icon: tray,
            show_id: show.id().clone(),
            check_updates_id: check_updates.id().clone(),
            quit_id: quit.id().clone(),
        })
    }

    /// Sets the tray tooltip text.
    pub fn set_tooltip(&self, text: &str) {
        if let Err(e) = self.icon.set_tooltip(Some(text)) {
            log::warn!("could not set tray tooltip: {e}");
        }
    }

    /// Drains pending tray / menu events and returns the resulting actions.
    pub fn poll(&self) -> Vec<TrayAction> {
        let mut actions = Vec::new();
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            match event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
                | TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } => actions.push(TrayAction::Show),
                _ => {}
            }
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.show_id {
                actions.push(TrayAction::Show);
            } else if event.id == self.check_updates_id {
                actions.push(TrayAction::CheckUpdates);
            } else if event.id == self.quit_id {
                actions.push(TrayAction::Quit);
            }
        }
        actions
    }
}
