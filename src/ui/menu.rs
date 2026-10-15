//! Native macOS menu bar (muda). Items with key equivalents are handled by the menu, which
//! receives those keys before egui, so the same shortcuts are not handled again in egui.

use std::collections::HashMap;
use std::sync::mpsc;

use eframe::egui;
use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{AboutMetadata, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};

use super::actions::Action;
use crate::bookmarks::transfer::ExportScope;
use crate::i18n::t;

pub struct AppMenu {
    _menu: Menu,
    actions: HashMap<MenuId, Action>,
}

fn acc(mods: Modifiers, code: Code) -> Option<Accelerator> {
    Some(Accelerator::new(mods, code))
}

const CMD: Modifiers = Modifiers::META;

impl AppMenu {
    /// Builds the menu and installs it as the application's main menu. Call on the main thread
    /// after the app finished launching (the eframe creation closure).
    pub fn install() -> Self {
        let mut actions = HashMap::new();
        let mut item = |text: &str, accel: Option<Accelerator>, action: Action| -> MenuItem {
            let it = MenuItem::new(text, true, accel);
            actions.insert(it.id().clone(), action);
            it
        };
        let shift_cmd = CMD | Modifiers::SHIFT;
        let alt_cmd = CMD | Modifiers::ALT;

        let about = PredefinedMenuItem::about(
            Some(t("About Peler")),
            Some(AboutMetadata {
                name: Some("Peler".into()),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                comments: Some(t("Lightweight SSH and SFTP client").into()),
                ..Default::default()
            }),
        );
        let settings = item(t("Settings…"), acc(CMD, Code::Comma), Action::Settings);
        let quit = item(t("Quit Peler"), acc(CMD, Code::KeyQ), Action::Quit);
        let app_menu = Submenu::with_items(
            "Peler",
            true,
            &[
                &about,
                &PredefinedMenuItem::separator(),
                &settings,
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::services(Some(t("Services"))),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::hide(Some(t("Hide Peler"))),
                &PredefinedMenuItem::hide_others(Some(t("Hide Others"))),
                &PredefinedMenuItem::show_all(Some(t("Show All"))),
                &PredefinedMenuItem::separator(),
                &quit,
            ],
        )
        .expect("app menu");

        let new_bookmark = item(
            t("New Bookmark…"),
            acc(CMD, Code::KeyN),
            Action::NewBookmark { group: None },
        );
        let quick = item(
            t("Quick Connect…"),
            acc(CMD, Code::KeyK),
            Action::QuickConnect,
        );
        let new_tab = item(t("New Tab"), acc(CMD, Code::KeyT), Action::NewTab);
        let close = item(t("Close"), acc(CMD, Code::KeyW), Action::CloseTabOrPane);
        let reconnect = item(t("Reconnect"), acc(CMD, Code::KeyR), Action::Reconnect);
        let import = item(t("Import Bookmarks…"), None, Action::Import);
        let import_ssh = item(
            t("Import from ~/.ssh/config"),
            None,
            Action::ImportSshConfig,
        );
        let export = item(
            t("Export Bookmarks…"),
            acc(shift_cmd, Code::KeyE),
            Action::Export(ExportScope::All),
        );
        let file_menu = Submenu::with_items(
            t("File"),
            true,
            &[
                &new_bookmark,
                &quick,
                &PredefinedMenuItem::separator(),
                &new_tab,
                &close,
                &reconnect,
                &PredefinedMenuItem::separator(),
                &import,
                &import_ssh,
                &export,
            ],
        )
        .expect("file menu");

        let copy = item(t("Copy"), acc(CMD, Code::KeyC), Action::Copy);
        let paste = item(t("Paste"), acc(CMD, Code::KeyV), Action::Paste);
        let cut = item(t("Cut"), acc(CMD, Code::KeyX), Action::Cut);
        let select_all = item(t("Select All"), None, Action::SelectAll);
        let clear = item(
            t("Clear Scrollback"),
            acc(alt_cmd, Code::KeyK),
            Action::ClearScrollback,
        );
        let edit_menu = Submenu::with_items(
            t("Edit"),
            true,
            &[
                &cut,
                &copy,
                &paste,
                &select_all,
                &PredefinedMenuItem::separator(),
                &clear,
            ],
        )
        .expect("edit menu");

        let sidebar = item(
            t("Show/Hide Sidebar"),
            acc(CMD, Code::KeyB),
            Action::ToggleSidebar,
        );
        let sftp = item(
            t("SFTP Panel"),
            acc(shift_cmd, Code::KeyF),
            Action::ToggleSftp,
        );
        let split_right = item(t("Split Right"), acc(CMD, Code::KeyD), Action::SplitRight);
        let split_down = item(
            t("Split Down"),
            acc(shift_cmd, Code::KeyD),
            Action::SplitDown,
        );
        let next_pane = item(
            t("Next Pane"),
            acc(CMD, Code::BracketRight),
            Action::NextPane,
        );
        let prev_pane = item(
            t("Previous Pane"),
            acc(CMD, Code::BracketLeft),
            Action::PrevPane,
        );
        let bigger = item(t("Bigger Text"), acc(CMD, Code::Equal), Action::FontBigger);
        let smaller = item(
            t("Smaller Text"),
            acc(CMD, Code::Minus),
            Action::FontSmaller,
        );
        let reset = item(t("Actual Size"), acc(CMD, Code::Digit0), Action::FontReset);
        let view_menu = Submenu::with_items(
            t("View"),
            true,
            &[
                &sidebar,
                &sftp,
                &PredefinedMenuItem::separator(),
                &split_right,
                &split_down,
                &next_pane,
                &prev_pane,
                &PredefinedMenuItem::separator(),
                &bigger,
                &smaller,
                &reset,
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::fullscreen(Some(t("Enter Full Screen"))),
            ],
        )
        .expect("view menu");

        let next_tab = item(
            t("Show Next Tab"),
            acc(shift_cmd, Code::BracketRight),
            Action::NextTab,
        );
        let prev_tab = item(
            t("Show Previous Tab"),
            acc(shift_cmd, Code::BracketLeft),
            Action::PrevTab,
        );
        let window_menu = Submenu::with_items(
            t("Window"),
            true,
            &[
                &PredefinedMenuItem::minimize(Some(t("Minimize"))),
                &PredefinedMenuItem::maximize(Some(t("Zoom"))),
                &PredefinedMenuItem::separator(),
                &next_tab,
                &prev_tab,
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::bring_all_to_front(Some(t("Bring All to Front"))),
            ],
        )
        .expect("window menu");

        let shortcuts = item(t("Keyboard Shortcuts"), None, Action::Shortcuts);
        let help_menu = Submenu::with_items(t("Help"), true, &[&shortcuts]).expect("help menu");

        let menu = Menu::with_items(&[
            &app_menu,
            &file_menu,
            &edit_menu,
            &view_menu,
            &window_menu,
            &help_menu,
        ])
        .expect("menu bar");
        menu.init_for_nsapp();
        window_menu.set_as_windows_menu_for_nsapp();
        help_menu.set_as_help_menu_for_nsapp();
        Self {
            _menu: menu,
            actions,
        }
    }

    pub fn action(&self, id: &MenuId) -> Option<Action> {
        self.actions.get(id).cloned()
    }
}

/// Forwards menu clicks into a channel and wakes egui. The handler can be set only once per
/// process, so it is installed once and the receiver is kept by the app.
pub fn event_channel(ctx: &egui::Context) -> mpsc::Receiver<MenuEvent> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let _ = tx.send(event);
        ctx.request_repaint();
    }));
    rx
}
