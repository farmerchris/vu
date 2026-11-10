//! The native macOS menu bar, built from the shared menu description
//! ([`super::input::menus`]) with the standard application and Window menus
//! around it.

use super::input::{apply, is_on, label, menus, Cmd, Entry, MenuState, Request};
use crate::app::App;
use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{
    AboutMetadata, CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use std::collections::HashMap;

pub struct NativeMenu {
    /// Kept alive while installed.
    _menu: Menu,
    cmds: HashMap<MenuId, Cmd>,
    checks: Vec<(CheckMenuItem, Cmd)>,
    /// Items whose label follows the state, with their base label and the
    /// label last shown.
    items: Vec<(MenuItem, Cmd, String, String)>,
}

/// The application's name in its menu (macOS titles the menu itself from
/// `CFBundleName` in `macos/Info.plist`, embedded by `build.rs`).
const APP_NAME: &str = "VU Meter";

fn cmd_key(code: Code) -> Option<Accelerator> {
    Some(Accelerator::new(Some(Modifiers::SUPER), code))
}

impl NativeMenu {
    /// Build the menu bar and install it as the application's. Call on the
    /// main thread once the application is running.
    pub fn install(sources: &MenuState) -> muda::Result<Self> {
        let mut nm = NativeMenu {
            _menu: Menu::new(),
            cmds: HashMap::new(),
            checks: Vec::new(),
            items: Vec::new(),
        };
        let menu = Menu::new();

        // Quit through the app, not the system's terminate:, so a recording
        // in progress is finalized.
        let quit = MenuItem::new(format!("Quit {APP_NAME}"), true, cmd_key(Code::KeyQ));
        nm.cmds.insert(quit.id().clone(), Cmd::Quit);
        let about = AboutMetadata {
            name: Some(APP_NAME.into()),
            version: Some(env!("CARGO_PKG_VERSION").into()),
            comments: Some(env!("CARGO_PKG_DESCRIPTION").into()),
            ..Default::default()
        };
        let app_menu = Submenu::with_items(
            APP_NAME,
            true,
            &[
                &PredefinedMenuItem::about(Some(&format!("About {APP_NAME}")), Some(about)),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::services(None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::hide(Some(&format!("Hide {APP_NAME}"))),
                &PredefinedMenuItem::hide_others(None),
                &PredefinedMenuItem::show_all(None),
                &PredefinedMenuItem::separator(),
                &quit,
            ],
        )?;
        menu.append(&app_menu)?;

        for (title, entries) in menus(true, sources) {
            let sub = Submenu::new(title, true);
            nm.build(&sub, &entries)?;
            match title {
                "File" => {
                    let close = MenuItem::new("Close Window", true, cmd_key(Code::KeyW));
                    nm.cmds.insert(close.id().clone(), Cmd::Quit);
                    sub.append(&close)?;
                }
                "View" => {
                    sub.append_items(&[
                        &PredefinedMenuItem::separator(),
                        &PredefinedMenuItem::fullscreen(None),
                    ])?;
                }
                "Help" => {
                    // The Window menu goes just before Help, as on every Mac app.
                    let window = Submenu::with_items(
                        "Window",
                        true,
                        &[
                            &PredefinedMenuItem::minimize(None),
                            &PredefinedMenuItem::maximize(Some("Zoom")),
                            &PredefinedMenuItem::separator(),
                            &PredefinedMenuItem::bring_all_to_front(None),
                        ],
                    )?;
                    menu.append(&window)?;
                    window.set_as_windows_menu_for_nsapp();
                    sub.set_as_help_menu_for_nsapp();
                }
                _ => {}
            }
            menu.append(&sub)?;
        }
        menu.init_for_nsapp();
        nm._menu = menu;
        Ok(nm)
    }

    fn build(&mut self, sub: &Submenu, entries: &[Entry]) -> muda::Result<()> {
        for e in entries {
            match e {
                Entry::Separator => sub.append(&PredefinedMenuItem::separator())?,
                Entry::Sub(title, items) => {
                    let s = Submenu::new(title, true);
                    self.build(&s, items)?;
                    sub.append(&s)?;
                }
                Entry::Item(text, cmd, _) => {
                    let it = MenuItem::new(text, true, None);
                    self.cmds.insert(it.id().clone(), *cmd);
                    sub.append(&it)?;
                    self.items.push((it, *cmd, text.clone(), text.clone()));
                }
                Entry::Check(text, cmd, _) => {
                    let it = CheckMenuItem::new(text, true, false, None);
                    self.cmds.insert(it.id().clone(), *cmd);
                    sub.append(&it)?;
                    self.checks.push((it, *cmd));
                }
            }
        }
        Ok(())
    }

    /// Carry out chosen items and bring check marks and labels up to date
    /// with the state (which keys change too).
    pub fn poll(&mut self, app: &mut App, sources: &MenuState) -> Option<Request> {
        let mut req = None;
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            if let Some(&cmd) = self.cmds.get(ev.id()) {
                if let Some(r) = apply(app, cmd, sources) {
                    req = Some(r);
                }
            }
        }
        for (it, cmd) in &self.checks {
            let on = is_on(app, *cmd, sources);
            if it.is_checked() != on {
                it.set_checked(on);
            }
        }
        for (it, cmd, base, shown) in &mut self.items {
            let text = label(app, *cmd, base);
            if *shown != text {
                it.set_text(&text);
                *shown = text;
            }
        }
        req
    }
}
