//! Commands from the menu bar, keyboard shortcuts and buttons all go through `Action`.

use uuid::Uuid;

use crate::bookmarks::transfer::ExportScope;

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    // bookmarks
    NewBookmark { group: Option<Uuid> },
    EditBookmark(Uuid),
    DuplicateBookmark(Uuid),
    DeleteBookmark(Uuid),
    Connect(Uuid),
    NewGroup { parent: Option<Uuid> },
    RenameGroup(Uuid),
    DeleteGroup(Uuid),
    QuickConnect,
    // tabs and panes
    NewTab,
    DuplicateTab(usize),
    CloseTabOrPane,
    CloseTab(usize),
    SelectTab(usize),
    NextTab,
    PrevTab,
    MoveTab { from: usize, to: usize },
    Reconnect,
    SplitRight,
    SplitDown,
    NextPane,
    PrevPane,
    // view
    ToggleSidebar,
    ToggleSftp,
    FontBigger,
    FontSmaller,
    FontReset,
    // edit
    Copy,
    Paste,
    Cut,
    SelectAll,
    ClearScrollback,
    SendQuickCommand(Uuid),
    // import / export
    Import,
    ImportSshConfig,
    Export(ExportScope),
    // app
    Settings,
    About,
    Shortcuts,
    Quit,
}
