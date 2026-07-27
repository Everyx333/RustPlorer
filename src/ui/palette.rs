//! Command palette (Ctrl+Shift+P).
//!
//! A searchable list of every action, for keyboard-driven use. Commands are
//! declared as data rather than wired ad-hoc, so a new action appears in the
//! palette, carries its own shortcut hint, and stays discoverable — instead of
//! hiding behind a menu nobody opens.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

use crate::ui::app::RustPlorer;

/// Every action the palette can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    GoBack,
    GoForward,
    GoUp,
    Refresh,
    ToggleHidden,
    ToggleFolderSizes,
    FocusSearch,
    OpenSettings,
    CopyDiagnostics,
    CopyPath,
    NewFolder,
    DeleteSelected,
    OpenInExplorer,
    TogglePane,
    CloseArchive,
}

impl Command {
    /// All commands, in the order shown when the query is empty.
    pub const ALL: &'static [Command] = &[
        Command::Refresh,
        Command::GoBack,
        Command::GoForward,
        Command::GoUp,
        Command::FocusSearch,
        Command::NewFolder,
        Command::DeleteSelected,
        Command::CopyPath,
        Command::OpenInExplorer,
        Command::TogglePane,
        Command::ToggleHidden,
        Command::ToggleFolderSizes,
        Command::CloseArchive,
        Command::OpenSettings,
        Command::CopyDiagnostics,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::GoBack => "Go back",
            Self::GoForward => "Go forward",
            Self::GoUp => "Go up one folder",
            Self::Refresh => "Refresh",
            Self::ToggleHidden => "Toggle hidden files",
            Self::ToggleFolderSizes => "Toggle folder sizes",
            Self::FocusSearch => "Filter files",
            Self::OpenSettings => "Open settings",
            Self::CopyDiagnostics => "Copy diagnostics",
            Self::CopyPath => "Copy current path",
            Self::NewFolder => "New folder",
            Self::DeleteSelected => "Delete selected",
            Self::OpenInExplorer => "Open in Windows Explorer",
            Self::TogglePane => "Toggle second pane",
            Self::CloseArchive => "Close archive",
        }
    }

    /// Shortcut hint, shown right-aligned.
    pub fn shortcut(self) -> Option<&'static str> {
        match self {
            Self::GoBack => Some("Alt+⬅"),
            Self::GoForward => Some("Alt+➡"),
            Self::GoUp => Some("Alt+⬆"),
            Self::Refresh => Some("F5"),
            Self::FocusSearch => Some("Ctrl+F"),
            Self::OpenSettings => Some("Ctrl+,"),
            Self::DeleteSelected => Some("Delete"),
            Self::TogglePane => Some("Ctrl+F6"),
            _ => None,
        }
    }

    /// Extra search terms, so "trash" finds "Delete selected".
    fn keywords(self) -> &'static str {
        match self {
            Self::GoBack => "back previous history",
            Self::GoForward => "forward next history",
            Self::GoUp => "up parent folder",
            Self::Refresh => "reload rescan",
            Self::ToggleHidden => "hidden dotfiles show",
            Self::ToggleFolderSizes => "size calculate directory",
            Self::FocusSearch => "find filter search",
            Self::OpenSettings => "preferences options config",
            Self::CopyDiagnostics => "debug log support report",
            Self::CopyPath => "clipboard location",
            Self::NewFolder => "create directory mkdir",
            Self::DeleteSelected => "remove trash recycle",
            Self::OpenInExplorer => "windows shell native",
            Self::TogglePane => "split dual second panel",
            Self::CloseArchive => "exit zip leave",
        }
    }

    /// Text the matcher searches.
    fn haystack(self) -> String {
        format!("{} {}", self.title(), self.keywords())
    }
}

/// Palette state.
pub struct CommandPalette {
    pub open: bool,
    query: String,
    matcher: Matcher,
    results: Vec<Command>,
    /// Highlighted row, moved with the arrow keys.
    selected: usize,
    dirty: bool,
    focus_requested: bool,
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandPalette {
    pub fn new() -> Self {
        Self {
            open: false,
            query: String::new(),
            matcher: Matcher::new(Config::DEFAULT),
            results: Command::ALL.to_vec(),
            selected: 0,
            dirty: true,
            focus_requested: false,
        }
    }

    /// Open the palette, resetting to a clean state.
    pub fn show(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
        self.dirty = true;
        self.focus_requested = true;
    }

    pub fn hide(&mut self) {
        self.open = false;
    }

    fn refresh(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;

        if self.query.is_empty() {
            self.results = Command::ALL.to_vec();
            return;
        }

        let pattern = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, Command)> = Vec::new();

        for cmd in Command::ALL {
            buf.clear();
            let text = cmd.haystack();
            let haystack = nucleo_matcher::Utf32Str::new(&text, &mut buf);
            if let Some(score) = pattern.score(haystack, &mut self.matcher) {
                scored.push((score, *cmd));
            }
        }

        scored.sort_by(|a, b| b.0.cmp(&a.0));
        self.results = scored.into_iter().map(|(_, c)| c).collect();
        self.selected = 0;
    }

    pub fn results(&self) -> &[Command] {
        &self.results
    }
}

/// Draw the palette. Returns the command to run, if any.
pub fn draw(app: &mut RustPlorer, ctx: &egui::Context) -> Option<Command> {
    if !app.palette.open {
        return None;
    }

    let mut chosen = None;

    // Escape closes; arrows move; Enter runs. Handled before the widgets so
    // the text field does not swallow them.
    ctx.input(|i| {
        if i.key_pressed(egui::Key::Escape) {
            app.palette.open = false;
        }
        if i.key_pressed(egui::Key::ArrowDown) && !app.palette.results.is_empty() {
            app.palette.selected = (app.palette.selected + 1) % app.palette.results.len();
        }
        if i.key_pressed(egui::Key::ArrowUp) && !app.palette.results.is_empty() {
            app.palette.selected = app
                .palette
                .selected
                .checked_sub(1)
                .unwrap_or(app.palette.results.len() - 1);
        }
    });

    egui::Window::new("Command palette")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_TOP, [0.0, 80.0])
        .default_width(520.0)
        .show(ctx, |ui| {
            ui.add_space(4.0);

            let mut query = app.palette.query.clone();
            let resp = ui.add(
                egui::TextEdit::singleline(&mut query)
                    .desired_width(f32::INFINITY)
                    .hint_text("Type a command…")
                    .font(egui::TextStyle::Heading),
            );

            if resp.changed() {
                app.palette.query = query;
                app.palette.dirty = true;
            }

            if app.palette.focus_requested {
                app.palette.focus_requested = false;
                resp.request_focus();
            }

            app.palette.refresh();

            ui.add_space(6.0);
            ui.separator();

            if app.palette.results.is_empty() {
                ui.add_space(8.0);
                ui.weak("No matching commands.");
                ui.add_space(8.0);
                return;
            }

            egui::ScrollArea::vertical()
                .max_height(340.0)
                .show(ui, |ui| {
                    let selected = app.palette.selected;
                    let results = app.palette.results.clone();

                    for (i, cmd) in results.iter().enumerate() {
                        let is_sel = i == selected;

                        let resp = ui.add(egui::Button::selectable(is_sel, ""));

                        // Draw the row content over the selectable background
                        // so the shortcut can sit right-aligned.
                        let rect = resp.rect;
                        ui.scope_builder(
                            egui::UiBuilder::new().max_rect(rect),
                            |ui| {
                                ui.horizontal(|ui| {
                                    ui.add_space(6.0);
                                    ui.label(cmd.title());
                                    if let Some(sc) = cmd.shortcut() {
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.add_space(6.0);
                                                ui.label(
                                                    egui::RichText::new(sc)
                                                        .monospace()
                                                        .weak()
                                                        .small(),
                                                );
                                            },
                                        );
                                    }
                                });
                            },
                        );

                        if resp.clicked() {
                            chosen = Some(*cmd);
                        }
                    }
                });

            // Enter runs the highlighted command.
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                chosen = app.palette.results.get(app.palette.selected).copied();
            }
        });

    if chosen.is_some() {
        app.palette.open = false;
    }

    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette_with(query: &str) -> CommandPalette {
        let mut p = CommandPalette::new();
        p.query = query.to_string();
        p.dirty = true;
        p.refresh();
        p
    }

    #[test]
    fn empty_query_lists_everything() {
        let p = palette_with("");
        assert_eq!(p.results().len(), Command::ALL.len());
    }

    #[test]
    fn matches_by_title() {
        let p = palette_with("refresh");
        assert_eq!(p.results().first().copied(), Some(Command::Refresh));
    }

    #[test]
    fn matches_by_keyword_not_in_title() {
        // "trash" appears only in keywords, not in "Delete selected".
        let p = palette_with("trash");
        assert!(
            p.results().contains(&Command::DeleteSelected),
            "keyword search should find Delete selected"
        );
    }

    #[test]
    fn fuzzy_abbreviation_works() {
        let p = palette_with("nf");
        assert!(
            p.results().contains(&Command::NewFolder),
            "abbreviations should match"
        );
    }

    #[test]
    fn nonsense_query_matches_nothing() {
        let p = palette_with("qqqzzzxxx");
        assert!(p.results().is_empty());
    }

    #[test]
    fn every_command_has_a_title() {
        for cmd in Command::ALL {
            assert!(!cmd.title().is_empty());
        }
    }

    #[test]
    fn command_list_has_no_duplicates() {
        let mut seen = Vec::new();
        for cmd in Command::ALL {
            assert!(!seen.contains(cmd), "{cmd:?} listed twice");
            seen.push(*cmd);
        }
    }
}
