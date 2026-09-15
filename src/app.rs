use std::collections::{HashMap, HashSet, VecDeque};

use crate::{
    diff::{self, FileSection},
    git::{self, Commit, CommitKind},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Log,
    Show,
}

#[derive(Clone, Debug)]
pub struct ShowRow {
    pub text: String,
    pub source: usize,
    pub file: Option<usize>,
    pub folded: bool,
    pub fold_separator: bool,
}

#[derive(Clone, Copy)]
pub enum ShowScroll {
    Cursor,
    Bottom,
    Search,
    PreserveCursorPosition(isize),
}

pub struct App {
    pub commits: Vec<Commit>,
    pub log_format: crate::log_format::LogFormat,
    pub selected: usize,
    pub mode: Mode,
    pub log_offset: usize,
    // Viewport offsets are screen rows; the cursor indexes logical Show rows.
    pub show_offset: usize,
    pub show_cursor: usize,
    pub show_text: String,
    pub show_rows: Vec<ShowRow>,
    // Start of each wrapped row, followed by the total screen height.
    pub show_row_starts: Vec<usize>,
    pub show_scroll: Option<ShowScroll>,
    pub status: Option<String>,
    pub search: Option<String>,
    pub search_input: Option<String>,
    pub search_reverse: bool,
    pub search_match: Option<(Mode, usize)>,
    pub show_help: bool,
    pub log_row_origin: u16,
    pub visible_log_rows: Vec<Option<usize>>,
    pub show_row_origin: u16,
    pub visible_show_rows: usize,
    pub watch: bool,
    pub context: String,
    pub log_tab_start: u16,
    pub log_tab_end: u16,
    pub show_tab_start: u16,
    pub show_tab_end: u16,
    pub quit: bool,
    pub pending_history: Option<Vec<String>>,
    pub show_paths: Vec<String>,
    pub redraw: bool,
    search_history: Vec<String>,
    search_history_index: usize,
    search_draft: String,
    cache: HashMap<String, String>,
    cache_order: VecDeque<String>,
    show_files: Vec<FileSection>,
    expanded_folds: HashSet<String>,
}

impl App {
    pub fn new(commits: Vec<Commit>) -> Self {
        Self {
            commits,
            log_format: crate::log_format::LogFormat::default(),
            selected: 0,
            mode: Mode::Log,
            log_offset: 0,
            show_offset: 0,
            show_cursor: 0,
            show_text: String::new(),
            show_rows: Vec::new(),
            show_row_starts: Vec::new(),
            show_scroll: None,
            status: None,
            search: None,
            search_input: None,
            search_reverse: false,
            search_match: None,
            show_help: false,
            log_row_origin: 0,
            visible_log_rows: Vec::new(),
            show_row_origin: 0,
            visible_show_rows: 0,
            watch: false,
            context: String::new(),
            log_tab_start: 7,
            log_tab_end: 12,
            show_tab_start: 13,
            show_tab_end: 19,
            quit: false,
            pending_history: None,
            show_paths: Vec::new(),
            redraw: false,
            search_history: Vec::new(),
            search_history_index: 0,
            search_draft: String::new(),
            cache: HashMap::new(),
            cache_order: VecDeque::new(),
            show_files: Vec::new(),
            expanded_folds: HashSet::new(),
        }
    }

    fn load_history(&mut self) {
        let Some(args) = self.pending_history.clone() else {
            return;
        };
        match git::load_log(&args) {
            Ok(commits) => {
                self.pending_history = None;
                self.replace_commits(commits);
            }
            Err(error) => self.status = Some(format!("Could not load history: {error}")),
        }
    }

    pub fn switch_mode(&mut self) {
        self.mode = if self.mode == Mode::Log {
            Mode::Show
        } else {
            Mode::Log
        };
        if self.mode == Mode::Show {
            self.load_show();
        } else {
            self.load_history();
        }
    }

    pub fn move_by(&mut self, delta: isize, page: usize) {
        match self.mode {
            Mode::Log => {
                if self.commits.is_empty() {
                    return;
                }
                let amount = if delta.abs() == 1 { 1 } else { page.max(1) };
                self.selected = if delta < 0 {
                    self.selected.saturating_sub(amount)
                } else {
                    (self.selected + amount).min(self.commits.len() - 1)
                };
            }
            Mode::Show => {
                let amount = if delta.abs() == 1 { 1 } else { page.max(1) };
                self.show_cursor = if delta < 0 {
                    self.show_cursor.saturating_sub(amount)
                } else {
                    self.show_cursor
                        .saturating_add(amount)
                        .min(self.show_rows.len().saturating_sub(1))
                };
            }
        }
    }

    pub fn scroll_show(&mut self, delta: isize) {
        let height = self.visible_show_rows.max(1);
        let total = self
            .show_row_starts
            .last()
            .copied()
            .unwrap_or(self.show_rows.len());
        let max_offset = total.saturating_sub(height);
        self.show_offset = if delta < 0 {
            self.show_offset.saturating_sub(delta.unsigned_abs())
        } else {
            self.show_offset
                .saturating_add(delta as usize)
                .min(max_offset)
        };
        let first = self.show_row_at_screen(self.show_offset);
        let last = self.show_row_at_screen(self.show_offset + height - 1);
        self.show_cursor = self.show_cursor.clamp(first, last);
    }

    pub fn move_selection(&mut self, delta: isize) -> bool {
        let leaving_direct_diff = self.pending_history.is_some()
            && self
                .commits
                .get(self.selected)
                .is_some_and(|commit| commit.kind != CommitKind::Revision);
        self.load_history();
        // Loading history from a direct diff already selects HEAD. Do not skip it.
        if leaving_direct_diff && self.pending_history.is_none() {
            return !self.commits.is_empty();
        }
        if self.commits.is_empty() {
            return false;
        }
        let selected = if delta < 0 {
            self.selected.saturating_sub(1)
        } else {
            (self.selected + 1).min(self.commits.len() - 1)
        };
        if selected == self.selected {
            return false;
        }
        self.selected = selected;
        true
    }

    pub fn replace_commits(&mut self, commits: Vec<Commit>) {
        let selected = self
            .commits
            .get(self.selected)
            .map(|commit| (commit.kind, commit.hash.clone()));
        let old_start = self.log_selected_start();
        self.commits = commits;
        let preserved = selected.and_then(|key| {
            self.commits
                .iter()
                .position(|commit| commit.kind == key.0 && commit.hash == key.1)
        });
        self.selected =
            preserved.unwrap_or_else(|| self.selected.min(self.commits.len().saturating_sub(1)));
        if preserved.is_some() {
            self.log_offset = self
                .log_offset
                .saturating_add_signed(self.log_selected_start() as isize - old_start as isize);
        } else {
            self.log_offset = 0;
        }
        if self.mode == Mode::Show {
            if preserved.is_some() {
                self.refresh_show();
            } else {
                self.show_offset = 0;
                self.show_cursor = 0;
                self.show_scroll = None;
                self.search_match = None;
                self.load_show();
            }
        }
    }

    fn log_selected_start(&self) -> usize {
        let separator = self
            .commits
            .iter()
            .position(|commit| commit.kind == CommitKind::Revision)
            .is_some_and(|index| index > 0 && self.selected >= index);
        self.commits
            .iter()
            .take(self.selected)
            .map(|commit| commit.graph.len())
            .sum::<usize>()
            + usize::from(separator)
    }

    fn refresh_show(&mut self) {
        let Some(commit) = self.commits.get(self.selected) else {
            return;
        };
        // A selected historical commit is immutable, including expanded folds.
        if commit.kind == CommitKind::Revision {
            return;
        }
        let text = match git::show(commit, &self.show_paths) {
            Ok(text) => text,
            Err(error) => {
                self.status = Some(error);
                return;
            }
        };
        if text == self.show_text {
            return;
        }
        let old_cursor = self.show_cursor;
        let cursor_row = self.show_rows.get(old_cursor).cloned();
        let cursor_file = cursor_row
            .as_ref()
            .and_then(|row| row.file)
            .map(|index| self.show_files[index].clone());
        let screen_position = self
            .show_row_starts
            .get(old_cursor)
            .copied()
            .unwrap_or(old_cursor) as isize
            - self.show_offset as isize;
        let expanded = self.expanded_folds.clone();
        self.show_text = text;
        self.reset_show_folds();
        // Reopen by path, loading fresh contents for lazy untracked files too.
        for path in expanded {
            if let Some(index) = self.show_rows.iter().position(|row| {
                row.folded
                    && row
                        .file
                        .is_some_and(|file| self.show_files[file].path == path)
            }) {
                self.show_cursor = index;
                self.toggle_show_file();
            }
        }
        let candidates: Vec<_> = self
            .show_rows
            .iter()
            .enumerate()
            .filter(|(_, row)| match (&cursor_file, row.file) {
                (Some(old), Some(index)) => self.show_files[index].path == old.path,
                (None, None) => true,
                _ => false,
            })
            .collect();
        let relative_source = cursor_row
            .as_ref()
            .map(|row| {
                row.source
                    .saturating_sub(cursor_file.as_ref().map_or(0, |file| file.start))
            })
            .unwrap_or(0);
        let distance = |row: &ShowRow| {
            row.source
                .saturating_sub(row.file.map_or(0, |index| self.show_files[index].start))
                .abs_diff(relative_source)
        };
        self.show_cursor = candidates
            .iter()
            .filter(|(_, row)| {
                cursor_row
                    .as_ref()
                    .is_some_and(|old| old.text == row.text && old.folded == row.folded)
            })
            .min_by_key(|(_, row)| distance(row))
            .or_else(|| candidates.iter().min_by_key(|(_, row)| distance(row)))
            .map(|(index, _)| *index)
            .unwrap_or(old_cursor.min(self.show_rows.len().saturating_sub(1)));
        self.search_match = None;
        self.show_scroll = Some(ShowScroll::PreserveCursorPosition(screen_position));
    }

    pub fn top(&mut self) {
        match self.mode {
            Mode::Log => self.selected = 0,
            Mode::Show => {
                self.show_cursor = 0;
                self.show_offset = 0;
            }
        }
    }
    pub fn bottom(&mut self) {
        match self.mode {
            Mode::Log => self.selected = self.commits.len().saturating_sub(1),
            Mode::Show => {
                self.show_cursor = self.show_rows.len().saturating_sub(1);
                self.show_scroll = Some(ShowScroll::Bottom);
            }
        }
    }

    pub fn load_show(&mut self) {
        let Some(commit) = self.commits.get(self.selected).cloned() else {
            self.show_text = "No commits matched the supplied arguments.".to_owned();
            self.reset_show_folds();
            return;
        };
        if commit.kind == CommitKind::Revision {
            if let Some(text) = self.cache.get(&commit.hash) {
                let changed = self.show_text != *text;
                self.show_text = text.clone();
                if changed {
                    self.reset_show_folds();
                }
                return;
            }
        }
        self.show_offset = 0;
        self.show_cursor = 0;
        match git::show(&commit, &self.show_paths) {
            Ok(text) => {
                self.show_text = text.clone();
                self.reset_show_folds();
                if commit.kind == CommitKind::Revision {
                    self.insert_cache(commit.hash, text);
                }
                self.status = None;
            }
            Err(error) => {
                self.show_text = error.clone();
                self.reset_show_folds();
                self.status = Some(error);
            }
        }
    }

    fn reset_show_folds(&mut self) {
        self.show_files = diff::file_sections(&self.show_text);
        self.expanded_folds.clear();
        self.rebuild_show_rows();
    }

    pub fn ensure_show_rows(&mut self) {
        if self.show_rows.is_empty() && !self.show_text.is_empty() {
            self.reset_show_folds();
        }
    }

    fn rebuild_show_rows(&mut self) {
        let lines: Vec<_> = self.show_text.lines().collect();
        let mut rows = Vec::new();
        let mut source = 0;
        for (file_index, file) in self.show_files.iter().enumerate() {
            for (index, line) in lines[source..file.start].iter().enumerate() {
                rows.push(ShowRow {
                    text: (*line).to_owned(),
                    source: source + index,
                    file: None,
                    folded: false,
                    fold_separator: false,
                });
            }
            if (file.lockfile || file.untracked) && !self.expanded_folds.contains(&file.path) {
                rows.push(ShowRow {
                    text: String::new(),
                    source: file.start,
                    file: Some(file_index),
                    folded: false,
                    fold_separator: true,
                });
                rows.push(ShowRow {
                    text: format!(
                        "▶ {}{} ({}; Enter/z to expand)",
                        file.path,
                        if file.lazy_untracked_path.is_some() {
                            String::new()
                        } else {
                            format!(" — +{} −{}", file.additions, file.deletions)
                        },
                        if file.untracked {
                            if file.lazy_untracked_path.is_some() {
                                "untracked file, contents not loaded"
                            } else {
                                "untracked file folded"
                            }
                        } else {
                            "lockfile folded"
                        }
                    ),
                    source: file.start,
                    file: Some(file_index),
                    folded: true,
                    fold_separator: false,
                });
                rows.push(ShowRow {
                    text: String::new(),
                    source: file.start,
                    file: Some(file_index),
                    folded: false,
                    fold_separator: true,
                });
            } else {
                for (index, line) in lines[file.start..file.end].iter().enumerate() {
                    rows.push(ShowRow {
                        text: (*line).to_owned(),
                        source: file.start + index,
                        file: Some(file_index),
                        folded: false,
                        fold_separator: false,
                    });
                }
            }
            source = file.end;
        }
        for (index, line) in lines[source..].iter().enumerate() {
            rows.push(ShowRow {
                text: (*line).to_owned(),
                source: source + index,
                file: None,
                folded: false,
                fold_separator: false,
            });
        }
        self.show_rows = rows;
        self.show_cursor = self.show_cursor.min(self.show_rows.len().saturating_sub(1));
        self.show_row_starts.clear();
    }

    pub fn toggle_show_file(&mut self) {
        let Some(file_index) = self
            .show_rows
            .get(self.show_cursor)
            .and_then(|row| row.file)
        else {
            return;
        };
        let file = &self.show_files[file_index];
        if !file.lockfile && !file.untracked {
            return;
        }
        let path = file.path.clone();
        let source = file.start;
        let loaded = if let Some(untracked_path) = file.lazy_untracked_path.clone() {
            match git::show_untracked(&untracked_path) {
                Ok(text) => {
                    let mut lines: Vec<_> = self.show_text.lines().map(str::to_owned).collect();
                    lines.splice(file.start..file.end, text.lines().map(str::to_owned));
                    self.show_text = lines.join("\n");
                    self.show_text.push('\n');
                    self.show_files = diff::file_sections(&self.show_text);
                    self.expanded_folds.insert(path.clone());
                    self.status = None;
                    true
                }
                Err(error) => {
                    self.status = Some(error);
                    return;
                }
            }
        } else {
            false
        };
        if loaded || !self.expanded_folds.remove(&path) {
            self.expanded_folds.insert(path);
        }
        self.search_match = None;
        self.rebuild_show_rows();
        self.show_cursor = self
            .show_rows
            .iter()
            .position(|row| row.source == source && row.folded)
            .or_else(|| self.show_rows.iter().position(|row| row.source == source))
            .unwrap_or(self.show_cursor);
        self.show_scroll = Some(ShowScroll::Cursor);
    }

    pub fn toggle_all_lockfiles(&mut self) {
        let current_source = self.show_rows.get(self.show_cursor).map(|row| row.source);
        let lockfiles: Vec<_> = self
            .show_files
            .iter()
            .filter(|file| file.lockfile)
            .map(|file| file.path.clone())
            .collect();
        if lockfiles
            .iter()
            .any(|path| !self.expanded_folds.contains(path))
        {
            self.expanded_folds.extend(lockfiles);
        } else {
            self.expanded_folds.retain(|path| {
                !self
                    .show_files
                    .iter()
                    .any(|file| file.lockfile && file.path == *path)
            });
        }
        self.search_match = None;
        self.rebuild_show_rows();
        if let Some(source) = current_source {
            self.show_cursor = self
                .show_rows
                .iter()
                .rposition(|row| row.source <= source)
                .unwrap_or(0);
            self.show_scroll = Some(ShowScroll::Cursor);
        }
    }

    pub fn jump_show_file(&mut self, delta: isize) {
        let Some(current_source) = self.show_rows.get(self.show_cursor).map(|row| row.source)
        else {
            return;
        };
        let target = if delta < 0 {
            self.show_files
                .iter()
                .rev()
                .find(|file| file.start < current_source)
        } else {
            self.show_files
                .iter()
                .find(|file| file.start > current_source)
        };
        if let Some(target) = target {
            self.show_cursor = self
                .show_rows
                .iter()
                .position(|row| row.source == target.start && row.folded)
                .or_else(|| {
                    self.show_rows
                        .iter()
                        .position(|row| row.source == target.start)
                })
                .unwrap_or(self.show_cursor);
        }
    }

    fn show_row_at_screen(&self, screen: usize) -> usize {
        if self.show_row_starts.is_empty() {
            return screen.min(self.show_rows.len().saturating_sub(1));
        }
        self.show_row_starts
            .partition_point(|&start| start <= screen)
            .saturating_sub(1)
            .min(self.show_rows.len().saturating_sub(1))
    }

    pub fn click_show_row(&mut self, visible_row: usize) {
        let screen = self.show_offset.saturating_add(visible_row);
        if self
            .show_row_starts
            .last()
            .is_some_and(|&total| screen >= total)
        {
            return;
        }
        let clicked = self.show_row_at_screen(screen);
        self.show_cursor = clicked.min(self.show_rows.len().saturating_sub(1));
        if self.show_rows.get(clicked).is_some_and(|row| row.folded) {
            self.toggle_show_file();
        }
    }

    fn insert_cache(&mut self, hash: String, text: String) {
        if self.cache.len() >= 8 {
            if let Some(old) = self.cache_order.pop_front() {
                self.cache.remove(&old);
            }
        }
        self.cache_order.push_back(hash.clone());
        self.cache.insert(hash, text);
    }

    pub fn begin_search(&mut self, reverse: bool) {
        self.search_input = Some(String::new());
        self.search_history_index = self.search_history.len();
        self.search_draft.clear();
        self.search_reverse = reverse;
    }
    pub fn recall_search(&mut self, older: bool) {
        let Some(input) = self.search_input.as_ref() else {
            return;
        };
        let end = self.search_history.len();
        let index = if older {
            self.search_history_index.saturating_sub(1)
        } else {
            (self.search_history_index + 1).min(end)
        };
        if index == self.search_history_index {
            return;
        }
        if self.search_history_index == end {
            self.search_draft.clone_from(input);
        }
        self.search_history_index = index;
        self.search_input = Some(if index == end {
            self.search_draft.clone()
        } else {
            self.search_history[index].clone()
        });
    }

    pub fn submit_search(&mut self) {
        if let Some(query) = self.search_input.take() {
            if !query.is_empty() {
                if self.search_history.last() != Some(&query) {
                    self.search_history.push(query.clone());
                }
                self.search = Some(query);
                self.search_match = None;
                self.next_match(self.search_reverse);
            }
        }
    }
    pub fn repeat_search(&mut self, opposite: bool) {
        self.next_match(self.search_reverse ^ opposite);
    }
    pub fn next_match(&mut self, reverse: bool) {
        let Some(query) = self.search.as_ref().map(|s| s.to_lowercase()) else {
            return;
        };
        match self.mode {
            Mode::Log => {
                let n = self.commits.len();
                let start = self
                    .search_match
                    .filter(|(mode, _)| *mode == Mode::Log)
                    .map_or(self.selected, |(_, index)| index);
                for step in 1..=n {
                    let i = if reverse {
                        (start + n - step % n) % n
                    } else {
                        (start + step) % n
                    };
                    let c = &self.commits[i];
                    if c.hash.to_lowercase().contains(&query)
                        || self.log_format.text(c).to_lowercase().contains(&query)
                    {
                        self.selected = i;
                        self.search_match = Some((Mode::Log, i));
                        self.status = None;
                        return;
                    }
                }
            }
            Mode::Show => {
                let lines: Vec<_> = self.show_text.lines().collect();
                let n = lines.len();
                let start = self
                    .search_match
                    .filter(|(mode, _)| *mode == Mode::Show)
                    .and_then(|(_, index)| self.show_rows.get(index))
                    .or_else(|| self.show_rows.get(self.show_cursor))
                    .map_or(0, |row| row.source);
                for step in 1..=n {
                    let i = if reverse {
                        (start + n - step % n) % n
                    } else {
                        (start + step) % n
                    };
                    if lines[i].to_lowercase().contains(&query) {
                        if let Some(file) = self.show_files.iter().find(|file| {
                            (file.lockfile || file.untracked) && (file.start..file.end).contains(&i)
                        }) {
                            self.expanded_folds.insert(file.path.clone());
                            self.rebuild_show_rows();
                        }
                        if let Some(visible) = self.show_rows.iter().position(|row| row.source == i)
                        {
                            self.show_cursor = visible;
                            self.search_match = Some((Mode::Show, visible));
                            self.show_scroll = Some(ShowScroll::Search);
                        }
                        self.status = None;
                        return;
                    }
                }
            }
        }
        self.status = Some(format!("Pattern not found: {query}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(subject: &str) -> Commit {
        Commit {
            kind: CommitKind::Revision,
            hash: subject.repeat(40).chars().take(40).collect(),
            short_hash: subject.to_owned(),
            decorations: String::new(),
            author: String::new(),
            author_email: String::new(),
            author_date: String::new(),
            collaborators: crate::git::Collaborators::default(),
            subject: subject.to_owned(),
            graph: vec!["* ".to_owned()],
        }
    }
    #[test]
    fn selection_is_bounded() {
        let mut app = App::new(vec![commit("a"), commit("b")]);
        app.move_by(-1, 1);
        assert_eq!(app.selected, 0);
        app.move_by(2, 20);
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn log_search_wraps() {
        let mut app = App::new(vec![commit("first"), commit("needle"), commit("last")]);
        app.selected = 2;
        app.search = Some("needle".to_owned());
        app.next_match(false);
        assert_eq!(app.selected, 1);
        assert_eq!(app.search_match, Some((Mode::Log, 1)));
    }

    #[test]
    fn log_search_matches_full_and_partial_hashes_independently_of_display() {
        let hash = "0123456789abcdef0123456789abcdef01234567";
        let mut target = commit("target");
        target.hash = hash.to_owned();
        target.short_hash = hash[..7].to_owned();
        for format in ["%h %s", "%s"] {
            for query in [hash.to_owned(), hash[..12].to_uppercase()] {
                for reverse in [false, true] {
                    let mut app = App::new(vec![commit("first"), target.clone(), commit("last")]);
                    app.log_format = crate::log_format::LogFormat::parse(format).unwrap();
                    app.selected = if reverse { 0 } else { 2 };
                    app.begin_search(reverse);
                    app.search_input = Some(query.clone());
                    app.submit_search();
                    assert_eq!(
                        app.selected, 1,
                        "format={format}, query={query}, reverse={reverse}"
                    );
                    assert_eq!(app.search_match, Some((Mode::Log, 1)));
                    assert_eq!(app.status, None);
                }
            }
        }
    }

    #[test]
    fn adjacent_selection_stops_at_history_boundaries() {
        let mut app = App::new(vec![commit("newer"), commit("older")]);
        assert!(!app.move_selection(-1));
        assert!(app.move_selection(1));
        assert_eq!(app.selected, 1);
        assert!(!app.move_selection(1));
        assert!(app.move_selection(-1));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn refresh_preserves_selected_commit_identity() {
        let mut app = App::new(vec![commit("head"), commit("selected")]);
        app.selected = 1;

        app.replace_commits(vec![commit("new-head"), commit("head"), commit("selected")]);

        assert_eq!(app.selected, 2);
        assert_eq!(app.commits[app.selected].subject, "selected");
    }

    #[test]
    fn watch_refresh_preserves_log_viewport_when_commits_are_prepended() {
        use ratatui::{backend::TestBackend, Terminal};

        let commits: Vec<_> = (0..12).map(|i| commit(&format!("commit {i}"))).collect();
        let mut app = App::new(commits.clone());
        app.watch = true;
        app.selected = 5;
        app.log_offset = 4;
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let before = terminal.backend().buffer().clone();
        let selected_hash = app.commits[app.selected].hash.clone();

        // This is the same refresh entry point used by the watch event loop.
        app.replace_commits([vec![commit("new head")], commits].concat());
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();

        assert_eq!(app.commits[app.selected].hash, selected_hash);
        assert_eq!(
            terminal.backend().buffer(),
            &before,
            "new commits above the viewport must not move the text being read"
        );
    }

    #[test]
    fn watch_refresh_preserves_historical_show_viewport() {
        use ratatui::{backend::TestBackend, Terminal};

        let selected = commit("selected");
        let mut app = App::new(vec![selected.clone()]);
        app.watch = true;
        let patch = (0..30).map(|i| format!("line {i}\n")).collect::<String>();
        app.insert_cache(selected.hash.clone(), patch);
        app.switch_mode();
        app.show_cursor = 10;
        app.show_offset = 8;
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let before = terminal.backend().buffer().clone();

        app.replace_commits(vec![commit("new head"), selected.clone()]);
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();

        assert_eq!(app.commits[app.selected].hash, selected.hash);
        assert_eq!(app.show_cursor, 10);
        assert_eq!(
            terminal.backend().buffer(),
            &before,
            "refresh must preserve the viewport of an unchanged historical patch"
        );
    }

    #[test]
    fn watch_refresh_clears_show_when_the_selected_entry_disappears() {
        use ratatui::{backend::TestBackend, Terminal};

        let selected = commit("selected");
        let mut app = App::new(vec![selected.clone()]);
        app.watch = true;
        app.insert_cache(selected.hash, "old patch".to_owned());
        app.switch_mode();
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        app.replace_commits(Vec::new());
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert_eq!(app.show_rows.len(), 1);
        assert_eq!(
            app.show_rows[0].text,
            "No commits matched the supplied arguments."
        );
        assert_eq!(app.show_cursor, 0);
        assert_eq!(app.show_offset, 0);
    }

    #[test]
    fn lockfiles_start_folded_and_can_be_expanded() {
        let mut app = App::new(Vec::new());
        app.show_text = "commit metadata\ndiff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n-old\n+new\ndiff --git a/Cargo.lock b/Cargo.lock\n--- a/Cargo.lock\n+++ b/Cargo.lock\n-old dep\n+new dep\n"
            .to_owned();
        app.reset_show_folds();

        assert!(app.show_rows.iter().any(|row| row.folded));
        assert_eq!(
            app.show_rows
                .iter()
                .filter(|row| row.fold_separator)
                .count(),
            2
        );
        assert!(!app.show_rows.iter().any(|row| row.text == "+new dep"));

        app.show_cursor = app.show_rows.iter().position(|row| row.folded).unwrap();
        app.toggle_show_file();
        assert!(app.show_rows.iter().any(|row| row.text == "+new dep"));
    }

    #[test]
    fn untracked_files_start_folded_individually() {
        let mut app = App::new(Vec::new());
        app.show_text = "diff --git a/one.txt b/one.txt\nnew file mode 100644\n--- /dev/null\n+++ b/one.txt\n@@ -0,0 +1 @@\n+one\ndiff --git a/two.txt b/two.txt\nnew file mode 100644\n--- /dev/null\n+++ b/two.txt\n@@ -0,0 +1 @@\n+two\n"
            .to_owned();
        app.reset_show_folds();

        let folded: Vec<_> = app.show_rows.iter().filter(|row| row.folded).collect();
        assert_eq!(folded.len(), 2);
        assert!(folded.iter().any(|row| row.text.contains("one.txt")));
        assert!(folded.iter().any(|row| row.text.contains("two.txt")));
        assert!(!app.show_rows.iter().any(|row| row.text == "+one"));

        app.show_cursor = app.show_rows.iter().position(|row| row.folded).unwrap();
        app.toggle_show_file();
        assert!(app.show_rows.iter().any(|row| row.text == "+one"));
        assert!(!app.show_rows.iter().any(|row| row.text == "+two"));
    }

    #[test]
    fn search_reveals_a_match_inside_a_folded_lockfile() {
        let mut app = App::new(Vec::new());
        app.mode = Mode::Show;
        app.show_text = "diff --git a/Cargo.lock b/Cargo.lock\n--- a/Cargo.lock\n+++ b/Cargo.lock\n+hidden-needle\n"
            .to_owned();
        app.reset_show_folds();
        app.search = Some("hidden-needle".to_owned());

        app.next_match(false);

        assert_eq!(app.show_rows[app.show_cursor].text, "+hidden-needle");
        assert_eq!(app.search_match, Some((Mode::Show, app.show_cursor)));
    }
}
