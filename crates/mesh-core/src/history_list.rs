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
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use nu_ansi_term::Style;
use reedline::{
    CommandLineSearch, Completer, Editor, FileBackedHistory, History, HistoryItem, HistoryItemId,
    HistorySessionId, IgnoreAllExtraInfo, Menu, MenuEvent, Painter, SearchDirection, SearchFilter,
    SearchQuery, Span, SqliteBackedHistory, Suggestion, UndoBehavior,
};
use rusqlite::OpenFlags;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The menu's name, for `ReedlineEvent::Menu`.
pub(crate) const HISTORY_LIST: &str = "history_list";

/// How many commands the list shows — a glance, not a page.
const ROWS: usize = 5;

/// How many rows one trip to the store fetches. Distinct commands are what the
/// list wants and the store holds every repetition, so a page is read at a time
/// until enough distinct ones have turned up.
const PAGE: i64 = 64;

/// A [`History`] that can also mark a row a finalized logical command, so
/// last-argument recall reads whole commands without reassembling raw per-line
/// rows (see `ArgumentRecall` in `repl.rs`). Marking must run on the store's own
/// connection — a separate connection cannot reliably see a row reedline has
/// just written to the WAL — which is why it rides the shared history rather
/// than a second connection. The default is a no-op, for a store that keeps no
/// marks (the in-memory fallback).
pub(crate) trait MeshHistory: History {
    fn mark_command(&mut self, _id: HistoryItemId) {}
}

impl MeshHistory for FileBackedHistory {}

impl MeshHistory for SqliteBackedHistory {
    fn mark_command(&mut self, id: HistoryItemId) {
        // A non-null `more_info` is the mark; the value is reedline's own
        // "no extra info" serialization (`null` as text, still not SQL NULL),
        // so recall's `more_info IS NOT NULL` sees it and no marker type is
        // needed. `save_with_extra` rewrites the row on this same connection,
        // so the row is present; a failure only costs recall this one command.
        match self.load(id) {
            Ok(mut item) => {
                item.more_info = Some(IgnoreAllExtraInfo);
                if let Err(err) = self.save_with_extra(item) {
                    note!("mesh: could not record history command: {err}");
                }
            }
            Err(err) => note!("mesh: could not record history command: {err}"),
        }
    }
}

/// The one history, shared between reedline and the list.
///
/// A `Mutex` rather than `RwLock` because a history *writes* on every submit,
/// and nothing here holds the lock across a call back into reedline.
#[derive(Clone)]
pub(crate) struct SharedHistory(Arc<Mutex<Box<dyn MeshHistory>>>);

impl SharedHistory {
    pub(crate) fn new(history: impl MeshHistory + 'static) -> Self {
        Self(Arc::new(Mutex::new(Box::new(history))))
    }

    /// A poisoned lock means another thread panicked mid-call; the store is
    /// still the store, and losing the shell over it would lose more.
    fn lock(&self) -> MutexGuard<'_, Box<dyn MeshHistory>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Mark a row a finalized logical command, on the store's own connection.
    pub(crate) fn mark_command(&self, id: HistoryItemId) {
        self.lock().mark_command(id);
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

/// Where a fuzzy page is read from.
///
/// reedline's search knows a prefix and a substring, and a fuzzy match — the
/// query's characters in order, anything between — is neither; it is a `GLOB`
/// (`*g*s*t*`), which the store's own file answers through a second, read-only
/// connection, scanning the table in C. A store with no file, the in-memory
/// one behind `--no-save-history`, has its rows read out and sifted here
/// instead; so does a file whose reader would not open.
pub(crate) enum Reader {
    Sqlite {
        connection: rusqlite::Connection,
        /// This session, and when it started, in the store's own terms: the
        /// recall view is this session's rows plus every row from before it
        /// began, and reedline applies that to its own searches only. Both
        /// or neither, as reedline has it.
        session: Option<(i64, i64)>,
    },
    Memory,
}

impl Reader {
    /// A read-only reader on the store at `path`, or the in-memory sift when
    /// the file will not open that way — said once on stderr, since the list
    /// still works, only slower on a long history.
    pub(crate) fn open(
        path: &Path,
        session: Option<HistorySessionId>,
        started: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Self {
        match rusqlite::Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(connection) => Reader::Sqlite {
                connection,
                session: session
                    .zip(started)
                    .map(|(session, started)| (i64::from(session), started.timestamp_millis())),
            },
            Err(err) => {
                note!("mesh: could not open the history database for reading: {err}");
                Reader::Memory
            }
        }
    }

    /// The page of rows older than `before` whose text has `query`'s
    /// characters in order, newest first.
    fn fuzzy_page(
        &self,
        history: &SharedHistory,
        query: &str,
        before: Option<HistoryItemId>,
    ) -> Result<Page, String> {
        match self {
            Reader::Sqlite {
                connection,
                session,
            } => {
                // The session clause is reedline's own, `sqlite_backed.rs`.
                let mut statement = connection
                    .prepare_cached(
                        "SELECT id, command_line FROM history \
                         WHERE command_line GLOB ?1 AND (?2 IS NULL OR id < ?2) \
                         AND (?4 IS NULL OR session_id = ?4 OR start_timestamp < ?5) \
                         ORDER BY id DESC LIMIT ?3",
                    )
                    .map_err(|err| err.to_string())?;
                let (session_id, started) = session.unzip();
                let rows = statement
                    .query_map(
                        rusqlite::params![
                            glob_pattern(query),
                            before.map(|id| id.0),
                            PAGE,
                            session_id,
                            started
                        ],
                        |row| {
                            Ok((
                                Some(HistoryItemId(row.get::<_, i64>(0)?)),
                                row.get::<_, String>(1)?,
                            ))
                        },
                    )
                    .map_err(|err| err.to_string())?;
                let rows = rows
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|err| err.to_string())?;
                Ok(Page::of(rows))
            }
            Reader::Memory => {
                let mut query_ =
                    SearchQuery::everything(SearchDirection::Backward, history.session());
                query_.start_id = before;
                query_.limit = Some(PAGE);
                let items = history.search(query_).map_err(|err| err.to_string())?;
                // The page is the store's, matched or not: how far it reached
                // and whether it was full are read off it *before* the sift,
                // or a page of near misses would read as the end of the store.
                let mut page = Page::of(
                    items
                        .into_iter()
                        .map(|item| (item.id, item.command_line))
                        .collect(),
                );
                page.rows
                    .retain(|(_, command)| is_subsequence(query, command));
                Ok(page)
            }
        }
    }
}

/// One page read from the store, with where it reached and whether the
/// store had more — judged on the page as read, before any sifting.
struct Page {
    rows: Vec<(Option<HistoryItemId>, String)>,
    /// The oldest id on the page; the next page is the rows before it.
    last: Option<HistoryItemId>,
    /// A short page is the end of the store. So is a page with no ids at
    /// all, which cannot be paged past.
    more: bool,
}

impl Page {
    fn of(rows: Vec<(Option<HistoryItemId>, String)>) -> Self {
        let last = rows.iter().filter_map(|(id, _)| *id).next_back();
        let more = rows.len() as i64 >= PAGE && last.is_some();
        Self { rows, last, more }
    }
}

/// `query` as the `GLOB` that matches its characters in order: `*g*s*t*`,
/// with the three characters `GLOB` reads specially bracketed to themselves.
fn glob_pattern(query: &str) -> String {
    let mut pattern = String::from("*");
    for ch in query.chars() {
        match ch {
            '*' | '?' | '[' => {
                pattern.push('[');
                pattern.push(ch);
                pattern.push(']');
            }
            ch => pattern.push(ch),
        }
        pattern.push('*');
    }
    pattern
}

/// Whether `query`'s characters occur in `text` in that order.
fn is_subsequence(query: &str, text: &str) -> bool {
    let mut wanted = query.chars();
    let mut next = wanted.next();
    for ch in text.chars() {
        if next == Some(ch) {
            next = wanted.next();
        }
    }
    next.is_none()
}

/// Which pass over the store the next page comes from, and where in it.
/// Commands that *contain* the query come first, newest first; once those are
/// read out, the commands whose text merely has the query's characters in
/// order follow, newest first again. Recency ranks within each — a command
/// that starts with the query is no higher than one that merely contains it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pass {
    Contains(Option<HistoryItemId>),
    Fuzzy(Option<HistoryItemId>),
    Done,
}

/// The distinct commands matching a query, read from the store a page at a
/// time and only as far as the list has asked to see.
struct Matches {
    history: SharedHistory,
    reader: Arc<Mutex<Reader>>,
    query: String,
    rows: Vec<String>,
    seen: HashSet<String>,
    pass: Pass,
    /// A store that could not be read. Kept rather than dropped so the list
    /// can say so — a paint has no stderr to speak through.
    error: Option<String>,
}

impl Matches {
    fn new(history: SharedHistory, reader: Arc<Mutex<Reader>>, query: &str) -> Self {
        Self {
            history,
            reader,
            query: query.to_owned(),
            rows: Vec::new(),
            seen: HashSet::new(),
            pass: Pass::Contains(None),
            error: None,
        }
    }

    /// Read pages until `want` rows are held or the store runs out.
    fn ensure(&mut self, want: usize) {
        while self.rows.len() < want && self.pass != Pass::Done {
            self.page();
        }
    }

    fn page(&mut self) {
        let page = match self.pass {
            Pass::Contains(before) => self.contains_page(before),
            Pass::Fuzzy(before) => {
                let reader = self
                    .reader
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                reader.fuzzy_page(&self.history, &self.query, before)
            }
            Pass::Done => return,
        };
        let page = match page {
            Ok(page) => page,
            Err(err) => {
                self.error = Some(err);
                self.pass = Pass::Done;
                return;
            }
        };
        for (_, command) in page.rows {
            if self.seen.insert(command.clone()) {
                self.rows.push(command);
            }
        }
        self.pass = match self.pass {
            Pass::Contains(_) if page.more => Pass::Contains(page.last),
            // One character in order is one character contained, and an empty
            // query already matched every row: neither has a fuzzy pass to run.
            Pass::Contains(_) if self.query.chars().count() >= 2 => Pass::Fuzzy(None),
            Pass::Fuzzy(_) if page.more => Pass::Fuzzy(page.last),
            Pass::Contains(_) | Pass::Fuzzy(_) | Pass::Done => Pass::Done,
        };
    }

    /// The page of rows older than `before` that contain the query, newest
    /// first.
    fn contains_page(&self, before: Option<HistoryItemId>) -> Result<Page, String> {
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
        let items = self.history.search(query).map_err(|err| err.to_string())?;
        Ok(Page::of(
            items
                .into_iter()
                .map(|item| (item.id, item.command_line))
                .collect(),
        ))
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
    reader: Arc<Mutex<Reader>>,
    matches: Option<Matches>,
    /// The line as the user left it — the query, and what walking back
    /// restores. The line itself follows the selection.
    typed: String,
    selected: Option<usize>,
    /// The first visible row.
    top: usize,
}

impl HistoryList {
    pub(crate) fn new(history: SharedHistory, reader: Reader) -> Self {
        Self {
            history,
            reader: Arc::new(Mutex::new(reader)),
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
        let mut matches = Matches::new(self.history.clone(), Arc::clone(&self.reader), line);
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

    /// The typed line — the query, and what an edit applies to.
    pub(crate) fn typed(&self) -> &str {
        &self.typed
    }

    /// Drop the selection so the typed line stands again. `false` when there
    /// was none.
    pub(crate) fn deselect(&mut self) -> bool {
        self.selected.take().is_some()
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
                let mut at = 0;
                for (start, end) in match_ranges(&text, &self.typed) {
                    line.push_str(&style.paint(&text[at..start]).to_string());
                    line.push_str(&style.underline().paint(&text[start..end]).to_string());
                    at = end;
                }
                line.push_str(&style.paint(&text[at..]).to_string());
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

/// The byte ranges of `text` to underline as the match: the query's first
/// occurrence when `text` contains it, else each of its characters at the
/// first place it can stand, runs of neighbors merged. Nothing for an empty
/// query, or for a match the row's truncation cut away.
pub(crate) fn match_ranges(text: &str, query: &str) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    if let Some(start) = text.find(query) {
        return vec![(start, start + query.len())];
    }
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut wanted = query.chars();
    let mut next = wanted.next();
    for (at, ch) in text.char_indices() {
        if next != Some(ch) {
            continue;
        }
        let end = at + ch.len_utf8();
        match ranges.last_mut() {
            Some(last) if last.1 == at => last.1 = end,
            _ => ranges.push((at, end)),
        }
        next = wanted.next();
    }
    if next.is_some() { Vec::new() } else { ranges }
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
    /// What was typed, while a selected row stands on the line — for the
    /// highlighter, which draws that part bold and the rest of the recalled
    /// command in normal weight. `None` whenever the line is the user's own.
    recalled_from: Arc<Mutex<Option<String>>>,
}

impl HistoryMenu {
    pub(crate) fn new(history: SharedHistory, reader: Reader) -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
            list: HistoryList::new(history, reader),
            events: Vec::new(),
            synced: None,
            width: 80,
            values: Vec::new(),
            recalled_from: Arc::new(Mutex::new(None)),
        }
    }

    /// The flag the edit mode reads.
    pub(crate) fn active_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.active)
    }

    /// The typed text behind a recalled line, for the highlighter; see
    /// [`Self::recalled_from`].
    pub(crate) fn recalled_from(&self) -> Arc<Mutex<Option<String>>> {
        Arc::clone(&self.recalled_from)
    }

    fn publish_recalled_from(&self) {
        let typed = self
            .is_active()
            .then(|| {
                self.list
                    .selected_line()
                    .map(|_| self.list.typed().to_owned())
            })
            .flatten();
        *self
            .recalled_from
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = typed;
    }

    fn deactivate(&mut self) {
        self.active.store(false, Ordering::Relaxed);
        self.list.close();
        self.values.clear();
        self.events.clear();
        self.synced = None;
        self.publish_recalled_from();
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
        self.publish_recalled_from();
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
                    // Typing edits what was typed, and the list narrows to it.
                    // The edit mode puts the typed line back (`MoveLeft`)
                    // ahead of every such edit, so with nothing selected a
                    // changed line is the typed line changed — never a
                    // keystroke on a row's text to be inferred back.
                    Some((line, _)) if *line != now.0 && self.list.selected_line().is_none() => {
                        if !self.list.refilter(&now.0) {
                            self.deactivate();
                        }
                    }
                    // The cursor moved, or a selected row was edited by a
                    // path that did not put the typed line back first: the
                    // line is being edited, and the list is in the way of it.
                    Some(_) => self.deactivate(),
                    None => {}
                }
            }
            // The edit mode's "back to what was typed": `Esc` leads with it so
            // the list closes over the typed line, not the selection.
            MenuEvent::MoveLeft => {
                if self.list.deselect() {
                    Self::show(editor, self.list.line());
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
        let mut list = HistoryList::new(store, Reader::Memory);
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
        let mut list = HistoryList::new(store, Reader::Memory);
        assert!(list.open(""));
        assert_eq!(rows(&list), ["g", "e", "f", "d", "c"]);
    }

    #[test]
    fn nothing_matching_is_no_list() {
        let mut list = HistoryList::new(store(&["ls", "pwd"]), Reader::Memory);
        assert!(!list.open("git"));
        assert!(rows(&list).is_empty());
        assert_eq!(list.line(), "git", "the typed line stands");
    }

    #[test]
    fn walking_moves_the_selection_and_scrolls_the_window() {
        let store = store(&["m1", "m2", "m3", "m4", "m5", "m6", "m7", "m8"]);
        let mut list = HistoryList::new(store, Reader::Memory);
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
        let mut list = HistoryList::new(store(&["ls -l", "ls"]), Reader::Memory);
        list.open("ls");
        assert!(list.walk());
        assert_eq!(list.line(), "ls -l");
        assert!(!list.walk(), "nothing older");
        assert_eq!(list.line(), "ls -l");
    }

    #[test]
    fn walking_back_past_the_first_row_restores_the_typed_line_then_closes() {
        let mut list = HistoryList::new(store(&["ls -l", "ls"]), Reader::Memory);
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
        let mut list = HistoryList::new(store(&["git status", "git push"]), Reader::Memory);
        list.open("git");
        list.walk();
        assert!(list.refilter("git s"));
        assert_eq!(list.selected_line(), None);
        assert_eq!(
            rows(&list),
            ["git status", "git push"],
            "the containing row, then the fuzzy one (`git pu`s`h`)"
        );
        assert_eq!(list.line(), "git s");
        assert!(!list.refilter("git x"));
        assert!(rows(&list).is_empty());
    }

    #[test]
    fn matches_are_read_a_page_at_a_time_as_the_walk_needs_them() {
        let commands: Vec<String> = (0..150).map(|n| format!("cmd {n}")).collect();
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        let mut list = HistoryList::new(store(&refs), Reader::Memory);
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
        let mut list = HistoryList::new(store(&commands), Reader::Memory);
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
        let mut list = HistoryList::new(store, Reader::Memory);
        list.open("");
        assert_eq!(
            list.render(24, false),
            "> ls\r\n  echo a very long comm…\r\n  for x in [1 2 3] { …",
        );
        assert_eq!(list.lines(), 3);
    }

    #[test]
    fn with_color_the_selection_is_reversed_and_the_match_underlined() {
        let mut list = HistoryList::new(store(&["git status"]), Reader::Memory);
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
    fn fuzzy_matches_follow_the_substring_matches_newest_first() {
        let store = store(&["git stash", "gs", "git status", "ls", "grep -rs tail"]);
        let mut list = HistoryList::new(store, Reader::Memory);
        assert!(list.open("gs"));
        assert_eq!(
            rows(&list),
            ["gs", "grep -rs tail", "git status", "git stash"],
            "the one row containing `gs`, then those with a g before an s, newest first"
        );
        assert!(list.refilter("gst"));
        assert_eq!(rows(&list), ["grep -rs tail", "git status", "git stash"]);
        assert!(!list.refilter("gsx"));
    }

    #[test]
    fn the_in_memory_sift_pages_past_a_page_of_near_misses() {
        // Newest first, the first page holds nothing that matches; the match
        // is older than the page, and the sift must not take a page it
        // emptied for the end of the store.
        let mut commands = vec!["git status"];
        commands.extend(std::iter::repeat_n("ls -la", PAGE as usize + 3));
        let mut list = HistoryList::new(store(&commands), Reader::Memory);
        assert!(list.open("gst"));
        assert_eq!(rows(&list), ["git status"]);
    }

    #[test]
    fn a_one_character_query_has_no_fuzzy_pass() {
        let mut list = HistoryList::new(store(&["ab", "b"]), Reader::Memory);
        list.open("b");
        assert_eq!(rows(&list), ["b", "ab"]);
        assert_eq!(list.matches.as_ref().unwrap().pass, Pass::Done);
    }

    #[test]
    fn the_sqlite_reader_answers_the_fuzzy_pass_with_a_glob() {
        let dir = std::env::temp_dir().join(format!("mesh-history-list-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.sqlite3");
        let _ = std::fs::remove_file(&path);
        let mut history =
            reedline::SqliteBackedHistory::with_file(path.clone(), None, None).unwrap();
        for command in ["git stash", "a*b?[c]", "git status", "ls"] {
            history
                .save(HistoryItem::from_command_line(command))
                .unwrap();
        }
        let reader = Reader::open(&path, None, None);
        assert!(matches!(reader, Reader::Sqlite { .. }));
        let mut list = HistoryList::new(SharedHistory::new(history), reader);
        assert!(list.open("gst"));
        assert_eq!(rows(&list), ["git status", "git stash"]);
        assert!(
            list.refilter("*?["),
            "the GLOB's own characters match themselves"
        );
        assert_eq!(rows(&list), ["a*b?[c]"]);
        assert!(!list.refilter("zz"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_glob_pattern_puts_the_query_in_order_and_brackets_its_specials() {
        assert_eq!(glob_pattern("gst"), "*g*s*t*");
        assert_eq!(glob_pattern("a*b?["), "*a*[*]*b*[?]*[[]*");
        assert_eq!(glob_pattern(""), "*");
    }

    #[test]
    fn a_fuzzy_row_underlines_each_matched_character() {
        assert_eq!(match_ranges("git status", "stat"), vec![(4, 8)]);
        assert_eq!(match_ranges("git status", "gst"), vec![(0, 1), (4, 6)]);
        assert_eq!(
            match_ranges("git status", "gsx"),
            vec![],
            "no match, nothing underlined"
        );
        assert_eq!(match_ranges("git status", ""), vec![]);
        assert_eq!(match_ranges("日本語", "日語"), vec![(0, 3), (6, 9)]);
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

    impl MeshHistory for Broken {}

    #[test]
    fn a_store_that_cannot_be_read_is_said_so_in_the_list() {
        let mut list = HistoryList::new(SharedHistory::new(Broken), Reader::Memory);
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
            HistoryMenu::new(SharedHistory::new(Broken), Reader::Memory),
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

    /// The store as one session sees it: `session` started at `started`.
    fn session_store(
        path: &std::path::Path,
        session: Option<HistorySessionId>,
        started: chrono::DateTime<chrono::Utc>,
    ) -> reedline::SqliteBackedHistory {
        reedline::SqliteBackedHistory::with_file(path.to_path_buf(), session, Some(started))
            .unwrap()
    }

    #[test]
    fn a_peer_sessions_running_commands_are_not_yet_history_in_either_pass() {
        let dir = std::env::temp_dir().join(format!("mesh-history-peer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.sqlite3");
        let _ = std::fs::remove_file(&path);
        let earlier = chrono::Utc::now() - chrono::Duration::seconds(10);
        let now = chrono::Utc::now();
        let peer_session = reedline::Reedline::create_history_session_id();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let this_session = reedline::Reedline::create_history_session_id();
        assert_ne!(peer_session, this_session);
        let mut peer = session_store(&path, peer_session, earlier);
        let this = session_store(&path, this_session, now);
        // Before this session began: history. After: a peer still at work.
        for (command, when) in [
            ("git status", earlier),
            ("peer secret", now + chrono::Duration::seconds(1)),
        ] {
            let mut item = HistoryItem::from_command_line(command);
            item.session_id = peer_session;
            item.start_timestamp = Some(when);
            peer.save(item).unwrap();
        }
        let reader = Reader::open(&path, this_session, Some(now));
        let mut list = HistoryList::new(SharedHistory::new(this), reader);
        assert!(list.open(""));
        assert_eq!(rows(&list), ["git status"], "the substring pass");
        assert!(!list.refilter("psc"), "the fuzzy pass through the reader");
        assert!(list.refilter("gst"));
        assert_eq!(rows(&list), ["git status"]);
        std::fs::remove_dir_all(&dir).unwrap();
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
        (HistoryMenu::new(store(commands), Reader::Memory), editor)
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

        // The edit mode's spelling of a keystroke with a row selected: back
        // to the typed line, then the edit — which lands on that line.
        menu.menu_event(MenuEvent::MoveLeft);
        menu.menu_event(MenuEvent::Edit(false));
        menu.apply(&mut editor);
        assert_eq!(editor.get_buffer(), "git");
        typed(&mut editor, "git p");
        menu.menu_event(MenuEvent::Edit(false));
        menu.apply(&mut editor);
        assert!(menu.is_active());
        assert_eq!(menu.list().selected_line(), None);
        assert_eq!(rows(menu.list()), ["git push"]);
        assert_eq!(editor.get_buffer(), "git p");

        // With nothing selected the line is the typed text and edits are direct.
        typed(&mut editor, "git px");
        menu.menu_event(MenuEvent::Edit(false));
        menu.apply(&mut editor);
        assert!(!menu.is_active(), "nothing matches: no list");
        assert_eq!(editor.get_buffer(), "git px");
    }

    #[test]
    fn an_edit_on_a_selected_row_without_the_typed_line_put_back_closes_the_list() {
        let (mut menu, mut editor) = menu(&["git push", "git status"], "git");
        menu.menu_event(MenuEvent::Activate(false));
        menu.apply(&mut editor);
        assert_eq!(editor.get_buffer(), "git status");
        // No path in the edit mode does this; were one to, the row's text is
        // not the typed line, and guessing the typed edit from it is what
        // went wrong three times over. The list gets out of the way instead.
        typed(&mut editor, "git statu");
        menu.menu_event(MenuEvent::Edit(false));
        menu.apply(&mut editor);
        assert!(!menu.is_active());
        assert_eq!(
            editor.get_buffer(),
            "git statu",
            "the line is left as edited"
        );
    }

    #[test]
    fn a_word_deletion_runs_on_the_typed_line_once_it_is_put_back() {
        let (mut menu, mut editor) = menu(&["git stash", "git status --short"], "git st");
        menu.menu_event(MenuEvent::Activate(false));
        menu.apply(&mut editor);
        assert_eq!(editor.get_buffer(), "git status --short");
        // Ctrl-W as the edit mode spells it: back to the typed line, then the
        // cut — which the engine runs on that line, so the count of what
        // came off the row never enters into it.
        menu.menu_event(MenuEvent::MoveLeft);
        menu.menu_event(MenuEvent::Edit(false));
        menu.apply(&mut editor);
        assert_eq!(editor.get_buffer(), "git st");
        typed(&mut editor, "git ");
        menu.menu_event(MenuEvent::Edit(false));
        menu.apply(&mut editor);
        assert_eq!(
            editor.get_buffer(),
            "git ",
            "the word came off the typed line"
        );
        assert_eq!(rows(menu.list()), ["git status --short", "git stash"]);
        assert_eq!(menu.list().selected_line(), None);
    }

    #[test]
    fn the_back_to_typed_event_drops_the_selection() {
        let (mut menu, mut editor) = menu(&["git push", "git status"], "git");
        menu.menu_event(MenuEvent::Activate(false));
        menu.apply(&mut editor);
        menu.menu_event(MenuEvent::MoveLeft);
        menu.apply(&mut editor);
        assert!(menu.is_active(), "the list stays; only the selection goes");
        assert_eq!(editor.get_buffer(), "git");
        assert_eq!(menu.list().selected_line(), None);
        menu.menu_event(MenuEvent::MoveLeft);
        menu.apply(&mut editor);
        assert_eq!(
            editor.get_buffer(),
            "git",
            "nothing selected: nothing to do"
        );
    }

    #[test]
    fn the_typed_text_behind_a_recalled_line_is_published_for_the_highlighter() {
        let (mut menu, mut editor) = menu(&["git push", "git status"], "git");
        let recalled = menu.recalled_from();
        let read = |recalled: &Arc<Mutex<Option<String>>>| recalled.lock().unwrap().clone();
        assert_eq!(read(&recalled), None);
        menu.menu_event(MenuEvent::Activate(false));
        menu.apply(&mut editor);
        assert_eq!(
            read(&recalled),
            Some("git".to_owned()),
            "a row is on the line"
        );
        menu.menu_event(MenuEvent::MoveLeft);
        menu.apply(&mut editor);
        assert_eq!(read(&recalled), None, "the line is the user's own again");
        menu.menu_event(MenuEvent::MoveDown);
        menu.apply(&mut editor);
        assert_eq!(read(&recalled), Some("git".to_owned()));
        menu.menu_event(MenuEvent::Deactivate);
        assert_eq!(read(&recalled), None);
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
