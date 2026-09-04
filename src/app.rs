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

pub struct App {
    pub commits: Vec<Commit>,
    pub selected: usize,
    pub mode: Mode,
    pub log_offset: usize,
    pub show_offset: usize,
    pub show_text: String,
    pub show_rows: Vec<ShowRow>,
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
    pub redraw: bool,
    cache: HashMap<String, String>,
    cache_order: VecDeque<String>,
    show_files: Vec<FileSection>,
    expanded_lockfiles: HashSet<String>,
}

impl App {
    pub fn new(commits: Vec<Commit>) -> Self {
        Self {
            commits,
            selected: 0,
            mode: Mode::Log,
            log_offset: 0,
            show_offset: 0,
            show_text: String::new(),
            show_rows: Vec::new(),
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
            redraw: false,
            cache: HashMap::new(),
            cache_order: VecDeque::new(),
            show_files: Vec::new(),
            expanded_lockfiles: HashSet::new(),
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
                self.show_offset = if delta < 0 {
                    self.show_offset.saturating_sub(amount)
                } else {
                    self.show_offset.saturating_add(amount)
                };
            }
        }
    }

    pub fn move_selection(&mut self, delta: isize) -> bool {
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
        self.commits = commits;
        self.selected = selected
            .and_then(|key| {
                self.commits
                    .iter()
                    .position(|commit| commit.kind == key.0 && commit.hash == key.1)
            })
            .unwrap_or_else(|| self.selected.min(self.commits.len().saturating_sub(1)));
        self.log_offset = 0;
        if self.mode == Mode::Show {
            self.show_offset = 0;
            self.load_show();
        }
    }

    pub fn top(&mut self) {
        match self.mode {
            Mode::Log => self.selected = 0,
            Mode::Show => self.show_offset = 0,
        }
    }
    pub fn bottom(&mut self) {
        match self.mode {
            Mode::Log => self.selected = self.commits.len().saturating_sub(1),
            Mode::Show => self.show_offset = self.show_rows.len().saturating_sub(1),
        }
    }

    pub fn load_show(&mut self) {
        let Some(commit) = self.commits.get(self.selected).cloned() else {
            self.show_text = "No commits matched the supplied arguments.".to_owned();
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
        match git::show(&commit) {
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
        self.expanded_lockfiles.clear();
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
            if file.lockfile && !self.expanded_lockfiles.contains(&file.path) {
                rows.push(ShowRow {
                    text: String::new(),
                    source: file.start,
                    file: Some(file_index),
                    folded: false,
                    fold_separator: true,
                });
                rows.push(ShowRow {
                    text: format!(
                        "▶ {} — +{} −{} (lockfile folded; Enter/z to expand)",
                        file.path, file.additions, file.deletions
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
        self.show_offset = self.show_offset.min(self.show_rows.len().saturating_sub(1));
    }

    pub fn toggle_show_file(&mut self) {
        let Some(file_index) = self
            .show_rows
            .get(self.show_offset)
            .and_then(|row| row.file)
        else {
            return;
        };
        let file = &self.show_files[file_index];
        if !file.lockfile {
            return;
        }
        let path = file.path.clone();
        let source = file.start;
        if !self.expanded_lockfiles.remove(&path) {
            self.expanded_lockfiles.insert(path);
        }
        self.search_match = None;
        self.rebuild_show_rows();
        self.show_offset = self
            .show_rows
            .iter()
            .position(|row| row.source == source)
            .unwrap_or(self.show_offset);
    }

    pub fn toggle_all_lockfiles(&mut self) {
        let current_source = self.show_rows.get(self.show_offset).map(|row| row.source);
        let lockfiles: Vec<_> = self
            .show_files
            .iter()
            .filter(|file| file.lockfile)
            .map(|file| file.path.clone())
            .collect();
        if lockfiles
            .iter()
            .any(|path| !self.expanded_lockfiles.contains(path))
        {
            self.expanded_lockfiles.extend(lockfiles);
        } else {
            self.expanded_lockfiles.clear();
        }
        self.search_match = None;
        self.rebuild_show_rows();
        if let Some(source) = current_source {
            self.show_offset = self
                .show_rows
                .iter()
                .rposition(|row| row.source <= source)
                .unwrap_or(0);
        }
    }

    pub fn jump_show_file(&mut self, delta: isize) {
        let Some(current_source) = self.show_rows.get(self.show_offset).map(|row| row.source)
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
            self.show_offset = self
                .show_rows
                .iter()
                .position(|row| row.source == target.start)
                .unwrap_or(self.show_offset);
        }
    }

    pub fn click_show_row(&mut self, visible_row: usize) {
        let clicked = self.show_offset.saturating_add(visible_row);
        if self.show_rows.get(clicked).is_some_and(|row| row.folded) {
            self.show_offset = clicked;
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
        self.search_reverse = reverse;
    }
    pub fn submit_search(&mut self) {
        if let Some(query) = self.search_input.take() {
            if !query.is_empty() {
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
                    if format!("{} {} {}", c.short_hash, c.decorations, c.subject)
                        .to_lowercase()
                        .contains(&query)
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
                    .or_else(|| self.show_rows.get(self.show_offset))
                    .map_or(0, |row| row.source);
                for step in 1..=n {
                    let i = if reverse {
                        (start + n - step % n) % n
                    } else {
                        (start + step) % n
                    };
                    if lines[i].to_lowercase().contains(&query) {
                        if let Some(file) = self
                            .show_files
                            .iter()
                            .find(|file| file.lockfile && (file.start..file.end).contains(&i))
                        {
                            self.expanded_lockfiles.insert(file.path.clone());
                            self.rebuild_show_rows();
                        }
                        if let Some(visible) = self.show_rows.iter().position(|row| row.source == i)
                        {
                            self.show_offset = visible;
                            self.search_match = Some((Mode::Show, visible));
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

        app.show_offset = app.show_rows.iter().position(|row| row.folded).unwrap();
        app.toggle_show_file();
        assert!(app.show_rows.iter().any(|row| row.text == "+new dep"));
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

        assert_eq!(app.show_rows[app.show_offset].text, "+hidden-needle");
        assert_eq!(app.search_match, Some((Mode::Show, app.show_offset)));
    }
}
