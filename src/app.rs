use std::collections::{HashMap, VecDeque};

use crate::git::{self, Commit, CommitKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Log,
    Show,
}

pub struct App {
    pub commits: Vec<Commit>,
    pub selected: usize,
    pub mode: Mode,
    pub log_offset: usize,
    pub show_offset: usize,
    pub show_text: String,
    pub status: Option<String>,
    pub search: Option<String>,
    pub search_input: Option<String>,
    pub search_reverse: bool,
    pub search_match: Option<(Mode, usize)>,
    pub show_help: bool,
    pub log_row_origin: u16,
    pub visible_log_rows: Vec<Option<usize>>,
    pub quit: bool,
    cache: HashMap<String, String>,
    cache_order: VecDeque<String>,
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
            status: None,
            search: None,
            search_input: None,
            search_reverse: false,
            search_match: None,
            show_help: false,
            log_row_origin: 0,
            visible_log_rows: Vec::new(),
            quit: false,
            cache: HashMap::new(),
            cache_order: VecDeque::new(),
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

    pub fn top(&mut self) {
        match self.mode {
            Mode::Log => self.selected = 0,
            Mode::Show => self.show_offset = 0,
        }
    }
    pub fn bottom(&mut self) {
        match self.mode {
            Mode::Log => self.selected = self.commits.len().saturating_sub(1),
            Mode::Show => self.show_offset = self.show_text.lines().count().saturating_sub(1),
        }
    }

    pub fn load_show(&mut self) {
        let Some(commit) = self.commits.get(self.selected).cloned() else {
            self.show_text = "No commits matched the supplied arguments.".to_owned();
            return;
        };
        if commit.kind == CommitKind::Revision {
            if let Some(text) = self.cache.get(&commit.hash) {
                self.show_text = text.clone();
                return;
            }
        }
        self.show_offset = 0;
        match git::show(&commit) {
            Ok(text) => {
                self.show_text = text.clone();
                if commit.kind == CommitKind::Revision {
                    self.insert_cache(commit.hash, text);
                }
                self.status = None;
            }
            Err(error) => {
                self.show_text = error.clone();
                self.status = Some(error);
            }
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
                    .map_or(self.show_offset, |(_, index)| index);
                for step in 1..=n {
                    let i = if reverse {
                        (start + n - step % n) % n
                    } else {
                        (start + step) % n
                    };
                    if lines[i].to_lowercase().contains(&query) {
                        self.show_offset = i;
                        self.search_match = Some((Mode::Show, i));
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
}
