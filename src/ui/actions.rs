//! Commands from the menu bar, keyboard shortcuts and buttons all go through `Action`.

use uuid::Uuid;

use crate::bookmarks::transfer::ExportScope;

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    // bookmarks
    NewBookmark {
        group: Option<Uuid>,
    },
    EditBookmark(Uuid),
    DuplicateBookmark(Uuid),
    DeleteBookmark(Uuid),
    Connect(Uuid),
    NewGroup {
        parent: Option<Uuid>,
    },
    RenameGroup(Uuid),
    DeleteGroup(Uuid),
    QuickConnect,
    // tabs and panes
    NewTab,
    /// Tabs are referred to by their id: indices shift when tabs close or move.
    DuplicateTab(u64),
    CloseTabOrPane,
    CloseTab(u64),
    SelectTab(usize),
    NextTab,
    PrevTab,
    MoveTab {
        from: u64,
        to: u64,
    },
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
