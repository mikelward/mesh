//! The history list: the recent commands matching the typed line, shown
//! beneath it when `Up` or `Down` is pressed — `docs/DESIGN.md` §"Interactive
//! history".
//!
//! Three pieces. [`SharedHistory`] is the one store, handed to reedline (which
//! saves to it) and cloned into the list (which reads it) — reedline owns its
//! history outright, so sharing is the only way a menu outside the engine can
//! query it. [`HistoryList`] is the list itself: the matches, the window, the
//! selection, and the rendering, with no reedline type in its interface so it
//! can be tested without a terminal. [`HistoryMenu`] is the thin [`Menu`]
//! adapter reedline paints; the edit mode in `repl.rs` steers it.

use std::collections::HashSet;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use nu_ansi_term::Style;
use reedline::{
    CommandLineSearch, Completer, Editor, History, HistoryItem, HistoryItemId, HistorySessionId,
    Menu, MenuEvent, Painter, SearchDirection, SearchFilter, SearchQuery, Span, Suggestion,
    UndoBehavior,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The menu's name, for `ReedlineEvent::Menu`.
pub(crate) const HISTORY_LIST: &str = "history_list";

/// How many commands the list shows — a glance, not a page.
const ROWS: usize = 5;

/// How many rows one trip to the store fetches. Distinct commands are what the
/// list wants and the store holds every repetition, so a page is read at a time
/// until enough distinct ones have turned up.
const PAGE: i64 = 64;

/// The one history, shared between reedline and the list.
///
/// A `Mutex` rather than `RwLock` because a history *writes* on every submit,
/// and nothing here holds the lock across a call back into reedline.
#[derive(Clone)]
pub(crate) struct SharedHistory(Arc<Mutex<Box<dyn History>>>);

impl SharedHistory {
    pub(crate) fn new(history: impl History + 'static) -> Self {
        Self(Arc::new(Mutex::new(Box::new(history))))
    }

    /// A poisoned lock means another thread panicked mid-call; the store is
    /// still the store, and losing the shell over it would lose more.
    fn lock(&self) -> MutexGuard<'_, Box<dyn History>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl History for SharedHistory {
    fn save(&mut self, item: HistoryItem) -> reedline::Result<HistoryItem> {
        self.lock().save(item)
    }

    fn load(&self, id: HistoryItemId) -> reedline::Result<HistoryItem> {
        self.lock().load(id)
    }

    fn count(&self, query: SearchQuery) -> reedline::Result<i64> {
        self.lock().count(query)
    }

    fn search(&self, query: SearchQuery) -> reedline::Result<Vec<HistoryItem>> {
        self.lock().search(query)
    }

    fn update(
        &mut self,
        id: HistoryItemId,
        updater: &dyn Fn(HistoryItem) -> HistoryItem,
    ) -> reedline::Result<()> {
        self.lock().update(id, updater)
    }

    fn clear(&mut self) -> reedline::Result<()> {
        self.lock().clear()
    }

    fn delete(&mut self, id: HistoryItemId) -> reedline::Result<()> {
        self.lock().delete(id)
    }

    fn sync(&mut self) -> io::Result<()> {
        self.lock().sync()
    }

    fn session(&self) -> Option<HistorySessionId> {
        self.lock().session()
    }
}

/// Where the next page comes from: the rows older than the last one read, or
/// nowhere, the store being read out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Next {
    Before(Option<HistoryItemId>),
    Done,
}

/// The distinct commands containing a query, newest first — recency alone
/// ranks them, a command that starts with the query no higher than one that
/// merely contains it — read from the store a page at a time and only as far
/// as the list has asked to see.
struct Matches {
    history: SharedHistory,
    query: String,
    rows: Vec<String>,
    seen: HashSet<String>,
    next: Next,
    /// A store that could not be read. Kept rather than dropped so the list
    /// can say so — a paint has no stderr to speak through.
    error: Option<String>,
}

impl Matches {
    fn new(history: SharedHistory, query: &str) -> Self {
        Self {
            history,
            query: query.to_owned(),
            rows: Vec::new(),
            seen: HashSet::new(),
            next: Next::Before(None),
            error: None,
        }
    }

    /// Read pages until `want` rows are held or the store runs out.
    fn ensure(&mut self, want: usize) {
        while self.rows.len() < want && self.next != Next::Done {
            self.page();
        }
    }

    fn page(&mut self) {
        let Next::Before(before) = self.next else {
            return;
        };
        let query = SearchQuery {
            direction: SearchDirection::Backward,
            start_time: None,
            end_time: None,
            // Backward, `start_id` is the *exclusive* upper bound: the page after
            // the one that ended at `before`.
            start_id: before,
            end_id: None,
            limit: Some(PAGE),
            // This session's rows plus every row from before it began: what a
            // peer session is running right now is not yet history here.
            filter: SearchFilter::from_text_search(
                CommandLineSearch::Substring(self.query.clone()),
                self.history.session(),
            ),
        };
        let items = match self.history.search(query) {
            Ok(items) => items,
            Err(err) => {
                self.error = Some(err.to_string());
                self.next = Next::Done;
                return;
            }
        };
        let exhausted = (items.len() as i64) < PAGE;
        let mut last = None;
        for item in items {
            last = item.id.or(last);
            if self.seen.insert(item.command_line.clone()) {
                self.rows.push(item.command_line);
            }
        }
        self.next = match last {
            Some(_) if !exhausted => Next::Before(last),
            // A store without ids cannot be paged; the one page it gave is it.
            _ => Next::Done,
        };
    }
}

/// What walking back past the first row did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Back {
    /// The selection moved to a newer row.
    Moved,
    /// The selection left the rows; the line is what was typed again.
    ToTyped,
    /// There was nothing to walk back to: the list is done.
    Close,
}

/// The list: what was typed, the matches for it, the window onto them, and
/// which one is selected.
pub(crate) struct HistoryList {
    history: SharedHistory,
    matches: Option<Matches>,
    /// The line as the user left it — the query, and what walking back
    /// restores. The line itself follows the selection.
    typed: String,
    selected: Option<usize>,
    /// The first visible row.
    top: usize,
}

impl HistoryList {
    pub(crate) fn new(history: SharedHistory) -> Self {
        Self {
            history,
            matches: None,
            typed: String::new(),
            selected: None,
            top: 0,
        }
    }

    /// Open on `line`, selecting the first match. `false` when there is
    /// nothing to show: no match, and no failure to report either.
    pub(crate) fn open(&mut self, line: &str) -> bool {
        if !self.refilter(line) {
            return false;
        }
        if self.selected_row_exists() {
            self.selected = Some(0);
        }
        true
    }

    fn selected_row_exists(&self) -> bool {
        self.matches
            .as_ref()
            .is_some_and(|matches| !matches.rows.is_empty())
    }

    /// The line changed under the list: match the new text, with nothing
    /// selected. `false` when there is nothing to show any more — a store
    /// that failed to answer is something to show, so that is `true`.
    pub(crate) fn refilter(&mut self, line: &str) -> bool {
        self.typed = line.to_owned();
        let mut matches = Matches::new(self.history.clone(), line);
        matches.ensure(ROWS);
        let any = !matches.rows.is_empty() || matches.error.is_some();
        self.matches = Some(matches);
        self.selected = None;
        self.top = 0;
        any
    }

    pub(crate) fn close(&mut self) {
        self.matches = None;
        self.selected = None;
        self.top = 0;
    }

    /// Select the next older match, scrolling the window after it. At the
    /// oldest, nothing moves — the walk stops where a shell's would.
    pub(crate) fn walk(&mut self) -> bool {
        let Some(matches) = self.matches.as_mut() else {
            return false;
        };
        let next = self.selected.map_or(0, |index| index + 1);
        matches.ensure(next + 1);
        if next >= matches.rows.len() {
            return false;
        }
        self.selected = Some(next);
        if next >= self.top + ROWS {
            self.top = next + 1 - ROWS;
        }
        // The window's last row is one the user may walk to next; have it read.
        matches.ensure(self.top + ROWS);
        true
    }

    /// Select the next newer match; past the first, back to the typed line.
    pub(crate) fn back(&mut self) -> Back {
        match self.selected {
            Some(0) => {
                self.selected = None;
                Back::ToTyped
            }
            Some(index) => {
                self.selected = Some(index - 1);
                self.top = self.top.min(index - 1);
                Back::Moved
            }
            None => Back::Close,
        }
    }

    /// The command the selection names, or `None` when the typed line stands.
    pub(crate) fn selected_line(&self) -> Option<&str> {
        let matches = self.matches.as_ref()?;
        self.selected
            .and_then(|index| matches.rows.get(index))
            .map(String::as_str)
    }

    /// What the line should show: the selection, else what was typed.
    pub(crate) fn line(&self) -> &str {
        self.selected_line().unwrap_or(&self.typed)
    }

    /// The visible rows, each with whether it is the selected one.
    pub(crate) fn visible(&self) -> Vec<(&str, bool)> {
        let Some(matches) = self.matches.as_ref() else {
            return Vec::new();
        };
        matches
            .rows
            .iter()
            .enumerate()
            .skip(self.top)
            .take(ROWS)
            .map(|(index, row)| (row.as_str(), self.selected == Some(index)))
            .collect()
    }

    fn error(&self) -> Option<&str> {
        self.matches.as_ref()?.error.as_deref()
    }

    /// How many lines [`render`](Self::render) draws: one per visible row,
    /// plus one for a store error.
    pub(crate) fn lines(&self) -> u16 {
        (self.visible().len() + usize::from(self.error().is_some())) as u16
    }

    /// The list as the terminal draws it, one row per line: the selection
    /// marked with `>` — and reversed, with color — and the match underlined.
    pub(crate) fn render(&self, width: u16, color: bool) -> String {
        let plain = Style::default();
        let selected = plain.bold().reverse();
        let mut out = Vec::new();
        for (command, is_selected) in self.visible() {
            let (marker, style) = if is_selected {
                ("> ", selected)
            } else {
                ("  ", plain)
            };
            let text = row_text(command, width.saturating_sub(marker.width() as u16));
            let mut line = String::from(marker);
            if color {
                let (before, matched, after) = split_match(&text, &self.typed);
                line.push_str(&style.paint(before).to_string());
                if !matched.is_empty() {
                    line.push_str(&style.underline().paint(matched).to_string());
                }
                line.push_str(&style.paint(after).to_string());
            } else {
                line.push_str(&text);
            }
            out.push(line);
        }
        if let Some(err) = self.error() {
            // Cut to the width like a row: `lines` books one line for it.
            out.push(truncate(
                &format!("  history list: could not read history: {err}"),
                usize::from(width),
            ));
        }
        out.join("\r\n")
    }
}

/// A command as its row: its first line, `…` when there were more, cut to
/// `width` columns with `…` again when it was cut.
fn row_text(command: &str, width: u16) -> String {
    let mut lines = command.lines();
    let first = lines.next().unwrap_or("");
    let text = if lines.next().is_some() {
        format!("{first} …")
    } else {
        first.to_owned()
    };
    truncate(&text, usize::from(width))
}

/// `text` cut to `width` columns, ending in `…` when it did not fit.
fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let room = width.saturating_sub(1);
    let mut used = 0;
    let mut out = String::new();
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > room {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.push('…');
    out
}

/// `text` around its first occurrence of `query`, for underlining the match.
/// No query, or none left after truncation, leaves the middle empty.
fn split_match<'a>(text: &'a str, query: &str) -> (&'a str, &'a str, &'a str) {
    if query.is_empty() {
        return (text, "", "");
    }
    match text.find(query) {
        Some(start) => {
            let end = start + query.len();
            (&text[..start], &text[start..end], &text[end..])
        }
        None => (text, "", ""),
    }
}

/// The [`Menu`] reedline paints. Its active flag is shared with the edit mode,
/// which reads it to steer the keys — the engine sets and clears it through
/// [`Menu::set_active`], so what the edit mode sees is the engine's own answer,
/// never a mirror that can drift.
pub(crate) struct HistoryMenu {
    active: Arc<AtomicBool>,
    list: HistoryList,
    /// Events not yet applied, in order — one keystroke can bring several.
    events: Vec<MenuEvent>,
    /// The line and cursor as of the last event applied, so the next edit can
    /// be told apart: a changed line re-filters, a moved cursor closes the
    /// list, and neither is the engine reporting the same edit twice.
    synced: Option<(String, usize)>,
    /// The terminal's width at the last paint; `menu_string` is not told it.
    width: u16,
    /// The visible rows as reedline's own type, for `get_values`.
    values: Vec<Suggestion>,
}

impl HistoryMenu {
    pub(crate) fn new(history: SharedHistory) -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
            list: HistoryList::new(history),
            events: Vec::new(),
            synced: None,
            width: 80,
            values: Vec::new(),
        }
    }

    /// The flag the edit mode reads.
    pub(crate) fn active_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.active)
    }

    fn deactivate(&mut self) {
        self.active.store(false, Ordering::Relaxed);
        self.list.close();
        self.values.clear();
        self.events.clear();
        self.synced = None;
    }

    /// Put `line` on the line, cursor at its end, as recalling does.
    fn show(editor: &mut Editor, line: &str) {
        if editor.get_buffer() != line {
            let line = line.to_owned();
            editor.edit_buffer(
                |buffer| buffer.set_buffer(line),
                UndoBehavior::HistoryNavigation,
            );
        }
    }

    /// Apply the pending events to the list and the line.
    pub(crate) fn apply(&mut self, editor: &mut Editor) {
        for event in std::mem::take(&mut self.events) {
            self.apply_one(event, editor);
            if !self.is_active() {
                return;
            }
            // After each one, so the edit that rides behind a walk sees the
            // line the walk wrote rather than taking it for typing.
            self.sync(editor);
        }
    }

    fn sync(&mut self, editor: &Editor) {
        self.synced = Some((
            editor.get_buffer().to_owned(),
            editor.line_buffer().insertion_point(),
        ));
    }

    fn apply_one(&mut self, event: MenuEvent, editor: &mut Editor) {
        match event {
            MenuEvent::Activate(_) => {
                if self.list.open(editor.get_buffer()) {
                    Self::show(editor, self.list.line());
                } else {
                    self.deactivate();
                }
            }
            MenuEvent::Edit(_) => {
                let now = (
                    editor.get_buffer().to_owned(),
                    editor.line_buffer().insertion_point(),
                );
                match &self.synced {
                    // The engine reports a quick menu's edit twice per key.
                    Some(synced) if *synced == now => {}
                    // Typing narrows the list; nothing is selected until walked to.
                    Some((line, _)) if *line != now.0 => {
                        if !self.list.refilter(&now.0) {
                            self.deactivate();
                        }
                    }
                    // The cursor moved on a line that did not change: the line
                    // is being edited, and the list is in the way of that.
                    Some(_) => self.deactivate(),
                    None => {}
                }
            }
            MenuEvent::MoveDown | MenuEvent::NextElement => {
                if self.list.walk() {
                    Self::show(editor, self.list.line());
                }
            }
            MenuEvent::MoveUp | MenuEvent::PreviousElement => match self.list.back() {
                Back::Moved | Back::ToTyped => {
                    Self::show(editor, self.list.line());
                }
                Back::Close => self.deactivate(),
            },
            MenuEvent::Deactivate
            | MenuEvent::MoveLeft
            | MenuEvent::MoveRight
            | MenuEvent::NextPage
            | MenuEvent::PreviousPage => {}
        }
        if self.is_active() {
            let span = Span {
                start: 0,
                end: editor.get_buffer().len(),
            };
            self.values = self
                .list
                .visible()
                .into_iter()
                .map(|(value, _)| Suggestion {
                    value: value.to_owned(),
                    span,
                    ..Suggestion::default()
                })
                .collect();
        }
    }

    #[cfg(test)]
    pub(crate) fn list(&self) -> &HistoryList {
        &self.list
    }
}

impl Menu for HistoryMenu {
    // `MenuSettings` is not nameable outside reedline, so the two things it
    // would carry are answered directly and `settings()` is never reached.
    fn name(&self) -> &str {
        HISTORY_LIST
    }

    /// The same marker the completion menu shows in the prompt while open.
    fn indicator(&self) -> &str {
        "| "
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    fn set_active(&mut self, active: bool) {
        if active {
            self.active.store(true, Ordering::Relaxed);
        } else {
            self.deactivate();
        }
    }

    fn clear_input(&mut self) {}

    fn menu_event(&mut self, event: MenuEvent) {
        self.handle_menu_event(&event);
        match event {
            MenuEvent::Activate(_) => self.events = vec![event],
            MenuEvent::Deactivate => {}
            event => self.events.push(event),
        }
    }

    /// Quick completion is the one path on which the engine refreshes a menu
    /// *before* it snapshots the line for painting — otherwise a line the list
    /// changed would draw one keystroke late. The rest of quick completion is
    /// answered elsewhere: a lone row is kept from being accepted outright by
    /// [`results_are_provisional`](Self::results_are_provisional), and the
    /// engine's closing of a quick menu on `Backspace` and `Ctrl-A` is the
    /// list's own rule for a line being edited.
    fn can_quick_complete(&self) -> bool {
        true
    }

    /// A lone row is still something to look at before running: without this
    /// the engine would accept it the moment the list opened.
    fn results_are_provisional(&self) -> bool {
        self.values.len() == 1
    }

    fn can_partially_complete(
        &mut self,
        _values_updated: bool,
        _editor: &mut Editor,
        _completer: &mut dyn Completer,
    ) -> bool {
        false
    }

    /// The store is the list's own; the completer reedline offers is unused.
    fn update_values(&mut self, editor: &mut Editor, _completer: &mut dyn Completer) {
        self.apply(editor);
    }

    fn reset_position(&mut self) {}

    fn update_working_details(
        &mut self,
        editor: &mut Editor,
        _completer: &mut dyn Completer,
        painter: &Painter,
    ) {
        self.width = painter.screen_width();
        self.apply(editor);
    }

    /// Idempotent: the line already follows the selection.
    fn replace_in_buffer(&self, editor: &mut Editor) {
        if let Some(line) = self.list.selected_line() {
            Self::show(editor, line);
        }
    }

    fn menu_required_lines(&self, _terminal_columns: u16) -> u16 {
        self.list.lines()
    }

    fn menu_string(&self, _available_lines: u16, use_ansi_coloring: bool) -> String {
        self.list.render(self.width, use_ansi_coloring)
    }

    fn min_rows(&self) -> u16 {
        self.list.lines()
    }

    fn get_values(&self) -> &[Suggestion] {
        &self.values
    }
}

#[cfg(test)]
mod tests {
    use reedline::{FileBackedHistory, ReedlineError, ReedlineErrorVariants};

    use super::*;

    /// An in-memory store holding `commands`, oldest first.
    fn store(commands: &[&str]) -> SharedHistory {
        let mut history = FileBackedHistory::default();
        for command in commands {
            history
                .save(HistoryItem::from_command_line(*command))
                .unwrap();
        }
        SharedHistory::new(history)
    }

    fn rows(list: &HistoryList) -> Vec<&str> {
        list.visible().into_iter().map(|(row, _)| row).collect()
    }

    #[test]
    fn the_matches_are_the_distinct_commands_containing_the_query_newest_first() {
        let store = store(&[
            "cd git",
            "git status",
            "echo git",
            "git push",
            "ls",
            "git status",
        ]);
        let mut list = HistoryList::new(store);
        assert!(list.open("git"));
        assert_eq!(
            rows(&list),
            ["git status", "git push", "echo git", "cd git"],
            "recency alone ranks them: `echo git` sits above `cd git`, and a \
             command starting with the query is not lifted above one that does not"
        );
        assert_eq!(list.line(), "git status");
    }

    #[test]
    fn an_empty_line_lists_the_last_distinct_commands() {
        let store = store(&["a", "b", "c", "d", "e", "f", "e", "g"]);
        let mut list = HistoryList::new(store);
        assert!(list.open(""));
        assert_eq!(rows(&list), ["g", "e", "f", "d", "c"]);
    }

    #[test]
    fn nothing_matching_is_no_list() {
        let mut list = HistoryList::new(store(&["ls", "pwd"]));
        assert!(!list.open("git"));
        assert!(rows(&list).is_empty());
        assert_eq!(list.line(), "git", "the typed line stands");
    }

    #[test]
    fn walking_moves_the_selection_and_scrolls_the_window() {
        let store = store(&["m1", "m2", "m3", "m4", "m5", "m6", "m7", "m8"]);
        let mut list = HistoryList::new(store);
        list.open("m");
        assert_eq!(rows(&list), ["m8", "m7", "m6", "m5", "m4"]);
        for _ in 0..4 {
            assert!(list.walk());
        }
        assert_eq!(list.line(), "m4");
        assert_eq!(list.visible()[4], ("m4", true), "still in the window");
        assert!(list.walk());
        assert_eq!(list.line(), "m3");
        assert_eq!(
            rows(&list),
            ["m7", "m6", "m5", "m4", "m3"],
            "the window follows the selection past its edge"
        );
        // Back scrolls the other way only once the selection leaves the top.
        for _ in 0..4 {
            assert_eq!(list.back(), Back::Moved);
        }
        assert_eq!(list.line(), "m7");
        assert_eq!(rows(&list), ["m7", "m6", "m5", "m4", "m3"]);
        assert_eq!(list.back(), Back::Moved);
        assert_eq!(list.line(), "m8");
        assert_eq!(rows(&list), ["m8", "m7", "m6", "m5", "m4"]);
    }

    #[test]
    fn the_walk_stops_at_the_oldest_match() {
        let mut list = HistoryList::new(store(&["ls -l", "ls"]));
        list.open("ls");
        assert!(list.walk());
        assert_eq!(list.line(), "ls -l");
        assert!(!list.walk(), "nothing older");
        assert_eq!(list.line(), "ls -l");
    }

    #[test]
    fn walking_back_past_the_first_row_restores_the_typed_line_then_closes() {
        let mut list = HistoryList::new(store(&["ls -l", "ls"]));
        list.open("l");
        assert_eq!(list.line(), "ls");
        assert_eq!(list.back(), Back::ToTyped);
        assert_eq!(list.line(), "l");
        assert_eq!(list.selected_line(), None);
        assert_eq!(rows(&list), ["ls", "ls -l"], "the list is still showing");
        assert_eq!(list.back(), Back::Close);
        assert!(
            list.walk(),
            "from the typed line the walk selects the first row"
        );
        assert_eq!(list.line(), "ls");
    }

    #[test]
    fn refiltering_selects_nothing_and_reports_whether_anything_matched() {
        let mut list = HistoryList::new(store(&["git status", "git push"]));
        list.open("git");
        list.walk();
        assert!(list.refilter("git s"));
        assert_eq!(list.selected_line(), None);
        assert_eq!(rows(&list), ["git status"]);
        assert_eq!(list.line(), "git s");
        assert!(!list.refilter("git x"));
        assert!(rows(&list).is_empty());
    }

    #[test]
    fn matches_are_read_a_page_at_a_time_as_the_walk_needs_them() {
        let commands: Vec<String> = (0..150).map(|n| format!("cmd {n}")).collect();
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        let mut list = HistoryList::new(store(&refs));
        list.open("cmd");
        assert_eq!(
            list.matches.as_ref().unwrap().rows.len(),
            PAGE as usize,
            "one page read to fill the window"
        );
        for _ in 0..100 {
            assert!(list.walk());
        }
        assert_eq!(list.line(), "cmd 49", "the hundredth older than cmd 149");
        assert!(list.matches.as_ref().unwrap().rows.len() > PAGE as usize);
    }

    #[test]
    fn repeats_inside_a_page_do_not_count_toward_the_window() {
        let mut commands = vec!["ls"; 100];
        commands.push("ls -l");
        let mut list = HistoryList::new(store(&commands));
        list.open("ls");
        assert_eq!(
            rows(&list),
            ["ls -l", "ls"],
            "read to the end for distinct rows"
        );
        assert!(list.walk());
        assert!(!list.walk());
    }

    #[test]
    fn a_row_is_the_first_line_cut_to_the_width() {
        let store = store(&[
            "for x in [1 2 3] {\n  puts $x\n}",
            "echo a very long command line that will not fit",
            "ls",
        ]);
        let mut list = HistoryList::new(store);
        list.open("");
        assert_eq!(
            list.render(24, false),
            "> ls\r\n  echo a very long comm…\r\n  for x in [1 2 3] { …",
        );
        assert_eq!(list.lines(), 3);
    }

    #[test]
    fn with_color_the_selection_is_reversed_and_the_match_underlined() {
        let mut list = HistoryList::new(store(&["git status"]));
        list.open("stat");
        let drawn = list.render(80, true);
        assert!(drawn.starts_with("> "), "{drawn:?}");
        assert!(drawn.contains("\u{1b}[1;7m"), "reversed: {drawn:?}");
        assert!(
            drawn.contains("\u{1b}[1;4;7mstat"),
            "underlined match: {drawn:?}"
        );
    }

    #[test]
    fn a_wide_character_counts_its_columns() {
        assert_eq!(truncate("日本語です", 6), "日本…");
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abcd", 3), "ab…");
    }

    /// A store whose reads fail.
    struct Broken;

    impl History for Broken {
        fn save(&mut self, item: HistoryItem) -> reedline::Result<HistoryItem> {
            Ok(item)
        }

        fn load(&self, _id: HistoryItemId) -> reedline::Result<HistoryItem> {
            Err(ReedlineError(ReedlineErrorVariants::OtherHistoryError(
                "broken",
            )))
        }

        fn count(&self, _query: SearchQuery) -> reedline::Result<i64> {
            Ok(0)
        }

        fn search(&self, _query: SearchQuery) -> reedline::Result<Vec<HistoryItem>> {
            Err(ReedlineError(ReedlineErrorVariants::OtherHistoryError(
                "broken",
            )))
        }

        fn update(
            &mut self,
            _id: HistoryItemId,
            _updater: &dyn Fn(HistoryItem) -> HistoryItem,
        ) -> reedline::Result<()> {
            Ok(())
        }

        fn clear(&mut self) -> reedline::Result<()> {
            Ok(())
        }

        fn delete(&mut self, _id: HistoryItemId) -> reedline::Result<()> {
            Ok(())
        }

        fn sync(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn session(&self) -> Option<HistorySessionId> {
            None
        }
    }

    #[test]
    fn a_store_that_cannot_be_read_is_said_so_in_the_list() {
        let mut list = HistoryList::new(SharedHistory::new(Broken));
        assert!(list.open("x"), "a failure is something to show");
        assert_eq!(list.selected_line(), None);
        assert_eq!(list.lines(), 1);
        assert!(
            list.render(80, false)
                .contains("history list: could not read history:"),
            "{}",
            list.render(80, false)
        );
        assert_eq!(
            list.render(20, false),
            "  history list: cou…",
            "the row is cut to the width, as `lines` books one line for it"
        );
        let (mut menu, mut editor) = (
            HistoryMenu::new(SharedHistory::new(Broken)),
            Editor::default(),
        );
        menu.menu_event(MenuEvent::Activate(false));
        menu.apply(&mut editor);
        assert!(menu.is_active(), "the menu stays up to paint the failure");
        assert_eq!(menu.menu_required_lines(80), 1);
        assert!(
            menu.menu_string(10, false)
                .contains("could not read history")
        );
    }

    fn typed(editor: &mut Editor, line: &str) {
        let line = line.to_owned();
        editor.edit_buffer(
            |buffer| buffer.set_buffer(line),
            UndoBehavior::CreateUndoPoint,
        );
    }

    fn menu(commands: &[&str], line: &str) -> (HistoryMenu, Editor) {
        let mut editor = Editor::default();
        typed(&mut editor, line);
        (HistoryMenu::new(store(commands)), editor)
    }

    #[test]
    fn opening_puts_the_first_match_on_the_line_and_the_arrows_walk_it() {
        let (mut menu, mut editor) = menu(&["git push", "git status"], "git");
        menu.menu_event(MenuEvent::Activate(false));
        menu.apply(&mut editor);
        assert!(menu.is_active());
        assert_eq!(editor.get_buffer(), "git status");
        assert_eq!(menu.get_values().len(), 2);

        menu.menu_event(MenuEvent::MoveDown);
        menu.apply(&mut editor);
        assert_eq!(editor.get_buffer(), "git push");

        menu.menu_event(MenuEvent::MoveUp);
        menu.apply(&mut editor);
        assert_eq!(editor.get_buffer(), "git status");

        menu.menu_event(MenuEvent::MoveUp);
        menu.apply(&mut editor);
        assert_eq!(editor.get_buffer(), "git", "back to what was typed");
        assert!(menu.is_active());

        menu.menu_event(MenuEvent::MoveUp);
        menu.apply(&mut editor);
        assert!(!menu.is_active(), "and once more closes the list");
        assert_eq!(editor.get_buffer(), "git");
    }

    #[test]
    fn opening_on_nothing_matching_closes_at_once_and_leaves_the_line() {
        let (mut menu, mut editor) = menu(&["ls"], "git");
        menu.menu_event(MenuEvent::Activate(false));
        menu.apply(&mut editor);
        assert!(!menu.is_active());
        assert_eq!(editor.get_buffer(), "git");
    }

    #[test]
    fn a_changed_line_refilters_and_a_cursor_move_does_not() {
        let (mut menu, mut editor) = menu(&["git push", "git status"], "git");
        menu.menu_event(MenuEvent::Activate(false));
        menu.apply(&mut editor);
        assert_eq!(editor.get_buffer(), "git status");

        // The engine reports a quick menu's edit twice; the echo changes nothing.
        menu.menu_event(MenuEvent::Edit(false));
        menu.apply(&mut editor);
        assert_eq!(menu.list().selected_line(), Some("git status"));
        assert!(menu.is_active());

        typed(&mut editor, "git p");
        menu.menu_event(MenuEvent::Edit(false));
        menu.apply(&mut editor);
        assert!(menu.is_active());
        assert_eq!(menu.list().selected_line(), None);
        assert_eq!(rows(menu.list()), ["git push"]);
        assert_eq!(editor.get_buffer(), "git p", "typing is not overwritten");

        typed(&mut editor, "git px");
        menu.menu_event(MenuEvent::Edit(false));
        menu.apply(&mut editor);
        assert!(!menu.is_active(), "nothing matches: no list");
    }

    #[test]
    fn moving_the_cursor_closes_the_list_and_keeps_the_line() {
        let (mut menu, mut editor) = menu(&["git push", "git status"], "git");
        menu.menu_event(MenuEvent::Activate(false));
        menu.apply(&mut editor);
        editor.edit_buffer(|buffer| buffer.move_to_start(), UndoBehavior::MoveCursor);
        menu.menu_event(MenuEvent::Edit(false));
        menu.apply(&mut editor);
        assert!(!menu.is_active());
        assert_eq!(
            editor.get_buffer(),
            "git status",
            "the recalled line is kept"
        );
    }

    #[test]
    fn several_events_from_one_key_apply_in_order() {
        let (mut menu, mut editor) = menu(&["git push", "git status"], "git");
        menu.menu_event(MenuEvent::Activate(false));
        menu.apply(&mut editor);
        // The walk arrives with a no-op edit behind it, which is what makes the
        // engine refresh the menu before painting; the walk must still count.
        menu.menu_event(MenuEvent::MoveDown);
        menu.menu_event(MenuEvent::Edit(false));
        menu.apply(&mut editor);
        assert_eq!(editor.get_buffer(), "git push");
        assert!(menu.is_active());
        assert_eq!(
            menu.list().selected_line(),
            Some("git push"),
            "the edit behind the walk is not taken for typing"
        );
        assert_eq!(rows(menu.list()), ["git status", "git push"]);
    }

    #[test]
    fn a_lone_row_is_shown_rather_than_accepted() {
        let (mut menu, mut editor) = menu(&["git status"], "git");
        menu.menu_event(MenuEvent::Activate(false));
        menu.apply(&mut editor);
        assert!(
            menu.can_quick_complete(),
            "the engine refreshes quick menus before painting"
        );
        assert!(
            menu.results_are_provisional(),
            "so a lone row must be held back"
        );
        assert_eq!(menu.get_values().len(), 1);
    }

    #[test]
    fn accepting_is_idempotent_and_deactivation_clears_the_flag() {
        let (mut menu, mut editor) = menu(&["git status"], "git");
        let flag = menu.active_flag();
        menu.menu_event(MenuEvent::Activate(false));
        menu.apply(&mut editor);
        assert!(flag.load(Ordering::Relaxed));
        menu.replace_in_buffer(&mut editor);
        assert_eq!(editor.get_buffer(), "git status");
        menu.menu_event(MenuEvent::Deactivate);
        assert!(!flag.load(Ordering::Relaxed));
        assert!(menu.get_values().is_empty());
        assert_eq!(menu.menu_required_lines(80), 0);
    }
}
