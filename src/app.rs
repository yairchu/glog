use std::collections::{HashMap, HashSet, VecDeque};

use crate::{
    diff::{self, FileSection},
    git::{self, Commit, CommitKind},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Status,
    Log,
    Show,
}

#[derive(Clone, Debug)]
pub struct ShowRow {
    pub preview: Option<crate::images::Row>,
    pub text: String,
    pub source: usize,
    pub file: Option<usize>,
    pub folded: bool,
    pub fold_separator: bool,
    pub summary: bool,
}

#[derive(Clone, Copy)]
pub enum ShowScroll {
    Cursor,
    Bottom,
    Search,
    PreserveCursorPosition(isize),
}

pub struct App {
    pub images: crate::images::Images,
    pub status_view: Option<crate::status::StatusView>,
    pub commits: Vec<Commit>,
    pub log_format: crate::log_format::LogFormat,
    pub selected: usize,
    pub mode: Mode,
    pub log_offset: usize,
    // Viewport offsets are screen rows; the cursor indexes logical Show rows.
    pub show_offset: usize,
    pub show_cursor: usize,
    pub show_text: String,
    pub show_stat: bool,
    // Status shares show_stat, so it can change while Show's rows are hidden.
    show_rows_stat: bool,
    stat_bookmark: Option<(Vec<u8>, usize, Option<crate::images::Row>)>,
    stat_submodule_bookmark: Option<(Vec<u8>, usize)>,
    pub show_rows: Vec<ShowRow>,
    // Start of each wrapped row, followed by the total screen height.
    pub show_row_starts: Vec<usize>,
    pub show_scroll: Option<ShowScroll>,
    pub status: Option<String>,
    pub search: Option<String>,
    pub search_input: Option<String>,
    pub search_reverse: bool,
    pub search_match: Option<(Mode, usize)>,
    // Several hidden gitlink lines share one visible summary row.
    show_search_location: Option<(Vec<Vec<u8>>, usize)>,
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
    expanded_folds: HashSet<Vec<u8>>,
    submodules: HashMap<Vec<u8>, Box<App>>,
    submodule_rows: HashMap<usize, (Vec<u8>, usize)>,
    submodule_root: Option<std::path::PathBuf>,
}

impl App {
    pub fn new(commits: Vec<Commit>) -> Self {
        Self {
            images: crate::images::Images::default(),
            status_view: None,
            commits,
            log_format: crate::log_format::LogFormat::default(),
            selected: 0,
            mode: Mode::Log,
            log_offset: 0,
            show_offset: 0,
            show_cursor: 0,
            show_text: String::new(),
            show_stat: false,
            show_rows_stat: false,
            stat_bookmark: None,
            stat_submodule_bookmark: None,
            show_rows: Vec::new(),
            show_row_starts: Vec::new(),
            show_scroll: None,
            status: None,
            search: None,
            search_input: None,
            search_reverse: false,
            search_match: None,
            show_search_location: None,
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
            submodules: HashMap::new(),
            submodule_rows: HashMap::new(),
            submodule_root: None,
        }
    }

    fn load_history(&mut self) {
        let Some(args) = self.pending_history.clone() else {
            return;
        };
        match if self.watch {
            git::load_watch_log()
        } else {
            git::load_log(&args)
        } {
            Ok(commits) => {
                self.pending_history = None;
                self.replace_commits(commits);
            }
            Err(error) => self.status = Some(format!("Could not load history: {error}")),
        }
    }

    pub fn open_status(&mut self) {
        let view = self.status_view.get_or_insert_with(|| {
            crate::status::StatusView::load().unwrap_or_else(crate::status::StatusView::unavailable)
        });
        view.enable_images(self.images.enabled);
        if view.show_stat != self.show_stat {
            view.toggle_stat();
        }
        self.search_input = None;
        self.mode = Mode::Status;
    }

    pub fn has_log_view(&self) -> bool {
        !self
            .commits
            .get(self.selected)
            .is_some_and(|commit| matches!(commit.kind, CommitKind::Comparison { .. }))
    }

    pub fn switch_mode(&mut self) {
        if !self.has_log_view() {
            return;
        }
        if self.mode == Mode::Log
            && self
                .commits
                .get(self.selected)
                .is_some_and(|commit| commit.kind == CommitKind::WorkingTree)
        {
            self.open_status();
            return;
        }
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
            Mode::Status => {}
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
        if !self.has_log_view() {
            return false;
        }
        let leaving_direct_diff = self.pending_history.is_some()
            && self.commits.get(self.selected).is_some_and(|commit| {
                matches!(commit.kind, CommitKind::Staged | CommitKind::Unstaged)
            });
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
        // Commit contents are immutable, but the decorations in cached Show text
        // follow mutable refs. Invalidate affected views even while Log is open.
        let decorations: HashMap<_, _> = commits
            .iter()
            .map(|commit| (&commit.hash, &commit.decorations))
            .collect();
        for old in &self.commits {
            if decorations.get(&old.hash).copied() != Some(&old.decorations) {
                self.cache.remove(&old.hash);
            }
        }
        self.cache_order
            .retain(|hash| self.cache.contains_key(hash));
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
        // Unchanged cached revisions retain their reading context without I/O.
        if commit.kind == CommitKind::Revision && self.cache.contains_key(&commit.hash) {
            return;
        }
        let text = match git::show(commit, &self.show_paths) {
            Ok(text) => text,
            Err(error) => {
                self.status = Some(error);
                return;
            }
        };
        if commit.kind == CommitKind::Revision {
            self.insert_cache(commit.hash.clone(), text.clone());
        }
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
        let nested_cursor = self.submodule_rows.get(&old_cursor).cloned();
        let old_submodules: HashMap<_, _> = self
            .show_files
            .iter()
            .filter_map(|file| {
                file.submodule
                    .as_ref()
                    .map(|ids| (file.path_bytes.clone(), ids.clone()))
            })
            .collect();
        let mut children = std::mem::take(&mut self.submodules);
        self.show_text = text;
        self.reset_show_folds();
        // Child patches describe immutable recorded commits. Retain their folds
        // and row indices when only the surrounding Show text has changed.
        children.retain(|path, _| {
            self.show_files.iter().any(|file| {
                &file.path_bytes == path
                    && file.submodule.as_ref() == old_submodules.get(path)
                    && file.submodule.is_some()
            })
        });
        let nested_cursor = nested_cursor.filter(|(path, _)| children.contains_key(path));
        self.submodules = children;
        // Reopen by path, loading fresh contents for lazy untracked files too.
        for path in expanded {
            if let Some(index) = self.show_rows.iter().position(|row| {
                row.folded
                    && row
                        .file
                        .is_some_and(|file| self.show_files[file].path_bytes == path)
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
                (Some(old), Some(index)) => self.show_files[index].path_bytes == old.path_bytes,
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
        let restored_cursor = candidates
            .iter()
            .filter(|(_, row)| {
                cursor_row.as_ref().is_some_and(|old| {
                    old.text == row.text && old.folded == row.folded && old.preview == row.preview
                })
            })
            .min_by_key(|(_, row)| distance(row))
            .or_else(|| candidates.iter().min_by_key(|(_, row)| distance(row)))
            .map(|(index, _)| *index);
        self.show_cursor = nested_cursor
            .and_then(|location| {
                self.submodule_rows
                    .iter()
                    .find_map(|(row, current)| (*current == location).then_some(*row))
            })
            .or(restored_cursor)
            .unwrap_or(old_cursor.min(self.show_rows.len().saturating_sub(1)));
        self.search_match = None;
        self.show_scroll = Some(ShowScroll::PreserveCursorPosition(screen_position));
    }

    pub fn top(&mut self) {
        match self.mode {
            Mode::Status => {}
            Mode::Log => self.selected = 0,
            Mode::Show => {
                self.show_cursor = 0;
                self.show_offset = 0;
            }
        }
    }
    pub fn bottom(&mut self) {
        match self.mode {
            Mode::Status => {}
            Mode::Log => self.selected = self.commits.len().saturating_sub(1),
            Mode::Show => {
                self.show_cursor = self.show_rows.len().saturating_sub(1);
                self.show_scroll = Some(ShowScroll::Bottom);
            }
        }
    }

    pub fn load_show(&mut self) {
        if self
            .commits
            .get(self.selected)
            .is_some_and(|commit| commit.kind == CommitKind::WorkingTree)
        {
            self.open_status();
            return;
        }
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
                } else if self.show_rows_stat != self.show_stat {
                    self.show_stat = self.show_rows_stat;
                    self.toggle_show_stat();
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
        self.show_search_location = None;
        self.show_files = diff::file_sections(&self.show_text);
        self.submodules.clear();
        self.expanded_folds.clear();
        self.stat_bookmark = None;
        self.stat_submodule_bookmark = None;
        self.rebuild_show_rows();
    }

    pub fn enable_images(&mut self) {
        self.images = crate::images::Images::from_env();
        self.rebuild_show_rows();
        if let Some(view) = &mut self.status_view {
            view.enable_images(self.images.enabled);
        }
    }

    pub fn ensure_show_rows(&mut self) {
        if self.show_rows.is_empty() && !self.show_text.is_empty() {
            self.reset_show_folds();
        }
    }

    fn rebuild_show_rows(&mut self) {
        self.show_rows_stat = self.show_stat;
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
                    summary: false,
                    preview: None,
                });
            }
            if self.show_stat || file.submodule.is_some() {
                let expanded = self.expanded_folds.contains(&file.path_bytes);
                let detail = if let Some((old, new)) = &file.submodule {
                    let dirty = lines[file.start..file.end].iter().any(|line| {
                        crate::ansi::plain(line)
                            .starts_with(&format!("+Subproject commit {new}-dirty"))
                    });
                    format!(
                        "submodule {} → {}{}",
                        &old[..8],
                        &new[..8],
                        if dirty { " (dirty)" } else { "" }
                    )
                } else if file.lazy_untracked_path.is_some() {
                    "contents not loaded".to_owned()
                } else if lines[file.start..file.end].iter().any(|line| {
                    let plain = crate::ansi::plain(line);
                    plain.starts_with("Binary files ") && plain.ends_with(" differ")
                }) {
                    "binary".to_owned()
                } else {
                    format!("+{} −{}", file.additions, file.deletions)
                };
                rows.push(ShowRow {
                    text: format!(
                        "{} {} | {}",
                        if expanded { "▼" } else { "▶" },
                        file.path,
                        detail
                    ),
                    source: file.start,
                    file: Some(file_index),
                    folded: !expanded,
                    fold_separator: false,
                    summary: true,
                    preview: None,
                });
                if expanded && file.submodule.is_none() {
                    for (index, line) in lines[file.start..file.end].iter().enumerate() {
                        rows.push(ShowRow {
                            text: (*line).to_owned(),
                            source: file.start + index,
                            file: Some(file_index),
                            folded: false,
                            fold_separator: false,
                            summary: false,
                            preview: None,
                        });
                    }
                }
            } else if (file.lockfile || file.untracked)
                && !self.expanded_folds.contains(&file.path_bytes)
            {
                rows.push(ShowRow {
                    text: String::new(),
                    source: file.start,
                    file: Some(file_index),
                    folded: false,
                    fold_separator: true,
                    summary: false,
                    preview: None,
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
                    summary: false,
                    preview: None,
                });
                rows.push(ShowRow {
                    text: String::new(),
                    source: file.start,
                    file: Some(file_index),
                    folded: false,
                    fold_separator: true,
                    summary: false,
                    preview: None,
                });
            } else {
                for (index, line) in lines[file.start..file.end].iter().enumerate() {
                    rows.push(ShowRow {
                        text: (*line).to_owned(),
                        source: file.start + index,
                        file: Some(file_index),
                        folded: false,
                        fold_separator: false,
                        summary: false,
                        preview: None,
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
                summary: false,
                preview: None,
            });
        }
        if self.images.enabled {
            let mut expanded = Vec::new();
            for row in rows {
                let previews = row
                    .file
                    .filter(|_| crate::images::is_binary(&crate::ansi::plain(&row.text)))
                    .map(|index| {
                        let file = &self.show_files[index];
                        crate::images::sources(
                            &lines[file.start..file.end].join("\n"),
                            git::raw_path(&file.path_bytes),
                            &self.images.root,
                            self.commits.get(self.selected).is_some_and(|c| {
                                matches!(
                                    c.kind,
                                    CommitKind::Unstaged
                                        | CommitKind::Comparison { worktree: true }
                                )
                            }),
                        )
                    })
                    .unwrap_or_default();
                expanded.push(row.clone());
                for (label, source) in previews {
                    let mut label_row = row.clone();
                    label_row.text = format!("{label} image");
                    expanded.push(label_row);
                    for y in 0..crate::images::HEIGHT {
                        let mut image_row = row.clone();
                        image_row.text.clear();
                        image_row.preview = Some(crate::images::Row {
                            source: source.clone(),
                            row: y,
                        });
                        expanded.push(image_row);
                    }
                }
            }
            rows = expanded;
        }
        // Insert child rows after image expansion so row routing uses final indices.
        self.submodule_rows.clear();
        let mut nested_rows = Vec::new();
        for row in rows {
            let child = row
                .file
                .filter(|_| row.summary && !row.folded)
                .and_then(|index| self.submodules.get(&self.show_files[index].path_bytes));
            nested_rows.push(row.clone());
            if let Some(child) = child {
                let path = self.show_files[row.file.unwrap()].path_bytes.clone();
                for (index, child_row) in child.show_rows.iter().enumerate() {
                    let mut nested = child_row.clone();
                    nested.text = format!("  {}", nested.text);
                    nested.source = row.source;
                    nested.file = row.file;
                    self.submodule_rows
                        .insert(nested_rows.len(), (path.clone(), index));
                    nested_rows.push(nested);
                }
            }
        }
        self.show_rows = nested_rows;
        self.show_cursor = self.show_cursor.min(self.show_rows.len().saturating_sub(1));
        self.show_row_starts.clear();
    }

    pub fn toggle_show_stat(&mut self) {
        self.ensure_show_rows();
        let nested_cursor = self.submodule_rows.get(&self.show_cursor).cloned();
        let current = self.show_rows.get(self.show_cursor).cloned();
        let file = current
            .as_ref()
            .and_then(|row| row.file)
            .map(|index| self.show_files[index].clone());
        if !self.show_stat {
            // Child rows keep their own indices while the parent is collapsed.
            // The flattened row's source points only to the gitlink header.
            self.stat_submodule_bookmark = nested_cursor.clone();
            self.stat_bookmark = file.as_ref().zip(current.as_ref()).map(|(file, row)| {
                (
                    file.path_bytes.clone(),
                    row.source.saturating_sub(file.start),
                    row.preview.clone(),
                )
            });
            self.expanded_folds.clear();
        }
        self.show_stat = !self.show_stat;
        if !self.show_stat {
            if let Some(file) = &file {
                if file.lazy_untracked_path.is_none()
                    && (file.submodule.is_none() || self.submodules.contains_key(&file.path_bytes))
                {
                    self.expanded_folds.insert(file.path_bytes.clone());
                }
            }
        }
        self.rebuild_show_rows();
        if let Some(file) = file {
            let source = if self.show_stat {
                file.start
            } else if let Some(row) = current.as_ref().filter(|row| !row.summary) {
                row.source
            } else {
                file.start
                    + self
                        .stat_bookmark
                        .as_ref()
                        .filter(|(path, _, _)| *path == file.path_bytes)
                        .map_or(0, |(_, line, _)| *line)
            };
            let preview = current
                .as_ref()
                .filter(|row| !row.summary)
                .and_then(|row| row.preview.as_ref())
                .or_else(|| {
                    self.stat_bookmark
                        .as_ref()
                        .filter(|(path, _, _)| *path == file.path_bytes)
                        .and_then(|(_, _, preview)| preview.as_ref())
                });
            self.show_cursor = self
                .show_rows
                .iter()
                .position(|row| {
                    row.file
                        .is_some_and(|index| self.show_files[index].path_bytes == file.path_bytes)
                        && (self.show_stat
                            || (row.source == source && row.preview.as_ref() == preview))
                })
                .unwrap_or(self.show_cursor.min(self.show_rows.len().saturating_sub(1)));
            if !self.show_stat {
                let nested = nested_cursor.as_ref().or_else(|| {
                    self.stat_submodule_bookmark
                        .as_ref()
                        .filter(|(path, _)| *path == file.path_bytes)
                });
                if let Some(index) = nested.and_then(|location| {
                    self.submodule_rows
                        .iter()
                        .find_map(|(row, current)| (current == location).then_some(*row))
                }) {
                    self.show_cursor = index;
                }
            }
        }
        self.search_match = None;
        self.show_scroll = Some(ShowScroll::Cursor);
    }

    fn searchable_show_lines(&self) -> Vec<(Vec<Vec<u8>>, usize, &str)> {
        let mut result = Vec::new();
        let children: HashMap<_, _> = self
            .show_files
            .iter()
            .filter_map(|file| {
                self.submodules
                    .get(&file.path_bytes)
                    .map(|child| (file.end, (&file.path_bytes, child)))
            })
            .collect();
        for (source, line) in self.show_text.lines().enumerate() {
            // The placeholder for unloaded untracked contents is internal.
            if !(line.contains("glog-lazy-untracked:")
                && crate::ansi::plain(line).starts_with("glog-lazy-untracked:"))
            {
                result.push((Vec::new(), source, line));
            }
            if let Some((path, child)) = children.get(&(source + 1)) {
                for (mut route, source, line) in child.searchable_show_lines() {
                    route.insert(0, (*path).clone());
                    result.push((route, source, line));
                }
            }
        }
        result
    }

    fn show_location(&self, row: usize) -> (Vec<Vec<u8>>, usize) {
        if let Some((path, index)) = self.submodule_rows.get(&row) {
            let (mut route, source) = self.submodules[path].show_location(*index);
            route.insert(0, path.clone());
            (route, source)
        } else {
            (
                Vec::new(),
                self.show_rows.get(row).map_or(0, |row| row.source),
            )
        }
    }

    fn reveal_show_location(&mut self, route: &[Vec<u8>], source: usize) -> Option<usize> {
        if let Some((path, rest)) = route.split_first() {
            let index = self
                .submodules
                .get_mut(path)?
                .reveal_show_location(rest, source)?;
            self.expanded_folds.insert(path.clone());
            self.rebuild_show_rows();
            return self
                .submodule_rows
                .iter()
                .find_map(|(row, (p, i))| (p == path && *i == index).then_some(*row));
        }
        let file = self
            .show_files
            .iter()
            .find(|f| (f.start..f.end).contains(&source));
        // Searching gitlink metadata selects its summary without loading
        // history, and unloaded untracked headers select their folded row.
        let unloaded = |f: &&crate::diff::FileSection| {
            f.submodule.is_some() || f.lazy_untracked_path.is_some()
        };
        let folded_start = file.filter(unloaded).map(|f| f.start);
        if let Some(file) = file.filter(|f| !unloaded(f)) {
            self.expanded_folds.insert(file.path_bytes.clone());
            self.rebuild_show_rows();
        }
        self.show_rows.iter().enumerate().position(|(index, row)| {
            !self.submodule_rows.contains_key(&index)
                && if let Some(start) = folded_start {
                    row.source == start && (row.summary || row.folded)
                } else {
                    row.source == source && !row.summary
                }
        })
    }

    pub fn toggle_show_file(&mut self) {
        if let Some((path, index)) = self.submodule_rows.get(&self.show_cursor).cloned() {
            let child = self.submodules.get_mut(&path).unwrap();
            child.show_cursor = index;
            child.toggle_show_file();
            self.status = child.status.clone();
            let cursor = child.show_cursor;
            self.rebuild_show_rows();
            // Collapsing a child changes the flattened row indices. Follow its
            // selected summary instead of retaining the old patch's row number.
            if let Some(row) = self
                .submodule_rows
                .iter()
                .find_map(|(row, (p, index))| (p == &path && *index == cursor).then_some(*row))
            {
                self.show_cursor = row;
            }
            self.search_match = None;
            self.show_scroll = Some(ShowScroll::Cursor);
            return;
        }
        let Some(file_index) = self
            .show_rows
            .get(self.show_cursor)
            .and_then(|row| row.file)
        else {
            return;
        };
        let file = &self.show_files[file_index];
        if !self.show_stat && !file.lockfile && !file.untracked && file.submodule.is_none() {
            return;
        }
        let path = file.path_bytes.clone();
        let source = file.start;
        if let Some((old, new)) = &file.submodule {
            if !self.submodules.contains_key(&path) {
                match git::show_submodule(
                    self.submodule_root.as_deref(),
                    git::raw_path(&path),
                    old,
                    new,
                ) {
                    Ok((root, text)) => {
                        let mut child = App::new(Vec::new());
                        child.images.enabled = self.images.enabled;
                        child.images.root = root.clone();
                        child.submodule_root = Some(root);
                        child.show_stat = true;
                        child.show_text = if text.is_empty() {
                            "No file changes between these commits.\n".to_owned()
                        } else {
                            text
                        };
                        child.ensure_show_rows();
                        self.submodules.insert(path.clone(), Box::new(child));
                        self.status = None;
                    }
                    Err(error) => {
                        self.status = Some(error);
                        return;
                    }
                }
            }
        }
        let loaded = if let Some(untracked_path) = file.lazy_untracked_path.clone() {
            match git::show_untracked(&git::raw_path(&untracked_path)) {
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
            .enumerate()
            .position(|(index, row)| {
                row.source == source && row.folded && !self.submodule_rows.contains_key(&index)
            })
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
            .map(|file| file.path_bytes.clone())
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
                    .any(|file| file.lockfile && file.path_bytes == *path)
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
            Mode::Status => {}
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
                let lines = self.searchable_show_lines();
                let n = lines.len();
                let previous = self.search_match.filter(|(mode, _)| *mode == Mode::Show);
                let (route, source) = previous
                    .and(self.show_search_location.clone())
                    .unwrap_or_else(|| {
                        self.show_location(previous.map_or(self.show_cursor, |(_, index)| index))
                    });
                let start = lines
                    .iter()
                    .position(|(r, s, _)| *r == route && *s == source)
                    .unwrap_or(0);
                let found = (1..=n).find_map(|step| {
                    let i = if reverse {
                        (start + n - step % n) % n
                    } else {
                        (start + step) % n
                    };
                    crate::ansi::plain(lines[i].2)
                        .to_lowercase()
                        .contains(&query)
                        .then(|| (lines[i].0.clone(), lines[i].1))
                });
                if let Some((route, source)) = found {
                    if let Some(visible) = self.reveal_show_location(&route, source) {
                        self.show_cursor = visible;
                        self.search_match = Some((Mode::Show, visible));
                        self.show_search_location = Some((route, source));
                        self.show_scroll = Some(ShowScroll::Search);
                    }
                    self.status = None;
                    return;
                }
            }
        }
        self.status = Some(format!("Pattern not found: {query}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write;

    fn commit(subject: &str) -> Commit {
        Commit {
            kind: CommitKind::Revision,
            diff_args: Vec::new(),
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
        let mut patch = String::new();
        for i in 0..30 {
            writeln!(patch, "line {i}").unwrap();
        }
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
    fn non_utf8_show_paths_keep_independent_folds() {
        // Git's quoted output also works on filesystems that cannot create
        // these names. The third name contains a literal backslash.
        let paths = [r"deps-\377.lock", r"deps-\376.lock", r"deps-\\377.lock"];
        let mut text = String::new();
        for (index, path) in paths.iter().enumerate() {
            writeln!(
                text,
                "diff --git \"a/{path}\" \"b/{path}\"\n--- \"a/{path}\"\n+++ \"b/{path}\"\n@@ -1 +1 @@\n-old\n+new-{index}"
            )
            .unwrap();
        }
        for summary in [false, true] {
            let mut app = App::new(Vec::new());
            app.show_stat = summary;
            app.show_text = text.clone();
            app.ensure_show_rows();
            assert_eq!(app.show_rows.iter().filter(|row| row.folded).count(), 3);
            assert!(!app
                .show_rows
                .iter()
                .any(|row| row.text.contains("changed file")));
            for index in 0..paths.len() {
                app.show_cursor = app
                    .show_rows
                    .iter()
                    .position(|row| row.file == Some(index) && row.folded)
                    .unwrap();
                app.toggle_show_file();
                for other in 0..paths.len() {
                    assert_eq!(
                        app.show_rows
                            .iter()
                            .any(|row| row.text == format!("+new-{other}")),
                        other == index,
                        "summary={summary}, expanded={index}, other={other}"
                    );
                }
                app.toggle_show_file();
                assert_eq!(app.show_rows.iter().filter(|row| row.folded).count(), 3);
            }
        }
    }

    #[test]
    fn stat_summaries_expand_collapse_and_search_regular_files() {
        let mut app = App::new(Vec::new());
        app.mode = Mode::Show;
        app.show_stat = true;
        app.show_text = "commit message\ndiff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n-old\n+needle\ndiff --git a/two.rs b/two.rs\n--- a/two.rs\n+++ b/two.rs\n-other\n+replacement\n".to_owned();
        app.ensure_show_rows();
        assert_eq!(app.show_rows.len(), 3);
        assert_eq!(app.show_rows[0].text, "commit message");
        assert_eq!(app.show_rows[1].text, "▶ one.rs | +1 −1");
        app.show_cursor = 1;
        app.toggle_show_file();
        assert!(app.show_rows[1].summary);
        assert!(!app.show_rows[1].folded);
        assert!(app.show_rows.iter().any(|row| row.text == "+needle"));
        app.toggle_show_file();
        assert_eq!(app.show_rows.len(), 3);
        app.search = Some("needle".to_owned());
        app.next_match(false);
        assert_eq!(app.show_rows[app.show_cursor].text, "+needle");
        assert!(app
            .show_rows
            .iter()
            .any(|row| row.folded && row.text.contains("two.rs")));
    }

    #[test]
    fn stat_summary_does_not_mistake_patch_text_for_a_binary_marker() {
        for (content, expected) in [
            (
                "+Binary files are labeled, and lazy untracked files show contents not loaded",
                "+1 −0",
            ),
            ("+Binary files a/example and b/example differ", "+1 −0"),
            ("-Binary files a/example and b/example differ", "+0 −1"),
            (" Binary files a/example and b/example differ", "+0 −0"),
            (
                "\x1b[1mBinary files a/README.md and b/README.md differ\x1b[m",
                "binary",
            ),
        ] {
            let mut app = App::new(Vec::new());
            app.show_stat = true;
            app.show_text = format!(
                "diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n{content}\n"
            );
            app.ensure_show_rows();
            assert_eq!(
                app.show_rows[0].text,
                format!("▶ README.md | {expected}"),
                "{content}"
            );
        }
    }

    #[test]
    fn repeated_search_advances_through_submodule_metadata() {
        for depth in [0, 1] {
            for summary in [false, true] {
                let mut app = App::new(Vec::new());
                app.show_stat = summary;
                app.show_text = format!(
                    "diff --git a/before b/before\n--- a/before\n+++ b/before\n@@ -1 +1 @@\n-old\n+Subproject before\ndiff --git a/module b/module\nindex {}..{} 160000\n--- a/module\n+++ b/module\n@@ -1 +1 @@\n-Subproject commit {}\n+Subproject commit {}\ndiff --git a/after b/after\n--- a/after\n+++ b/after\n@@ -1 +1 @@\n-old\n+Subproject after\n",
                    "1".repeat(40), "2".repeat(40), "1".repeat(40), "2".repeat(40)
                );
                app.ensure_show_rows();
                for _ in 0..depth {
                    let mut parent = App::new(Vec::new());
                    parent.show_text = format!(
                        "diff --git a/outer b/outer\nindex {}..{} 160000\n",
                        "3".repeat(40),
                        "4".repeat(40)
                    );
                    parent.ensure_show_rows();
                    parent.submodules.insert("outer".into(), Box::new(app));
                    parent.expanded_folds.insert("outer".into());
                    parent.rebuild_show_rows();
                    app = parent;
                }
                app.mode = Mode::Show;
                app.begin_search(false);
                app.search_input = Some("Subproject".into());
                app.submit_search();
                assert!(app.show_rows[app.show_cursor]
                    .text
                    .contains("Subproject before"));
                for reverse in [false, true] {
                    app.repeat_search(reverse);
                    assert!(app.show_rows[app.show_cursor].summary);
                    let summary_row = app.show_cursor;
                    app.repeat_search(reverse);
                    assert_eq!(app.show_cursor, summary_row);
                    app.repeat_search(reverse);
                    let expected = if reverse {
                        "Subproject before"
                    } else {
                        "Subproject after"
                    };
                    assert!(
                        app.show_rows[app.show_cursor].text.contains(expected),
                        "depth={depth}, summary={summary}, reverse={reverse}: expected {expected}"
                    );
                }
            }
        }
    }

    #[test]
    fn submodule_summary_preserves_worktree_dirty_marker() {
        let mut app = App::new(Vec::new());
        let old = "1".repeat(40);
        let new = "2".repeat(40);
        app.show_text = format!(
            "diff --git a/module b/module\nindex {old}..{new} 160000\n--- a/module\n+++ b/module\n@@ -1 +1 @@\n-Subproject commit {old}\n+Subproject commit {new}-dirty\n"
        );
        app.ensure_show_rows();
        assert!(app.show_rows[0].text.ends_with(" (dirty)"));
        app.toggle_show_stat();
        assert!(app.show_rows[0].text.ends_with(" (dirty)"));
    }

    #[test]
    fn collapsing_nested_patch_keeps_cursor_on_its_file_summary() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

        for depth in [1, 2] {
            for summary in [false, true] {
                for key in [KeyCode::Enter, KeyCode::Char('z')] {
                    let mut app = App::new(Vec::new());
                    app.show_stat = true;
                    app.show_text = "diff --git a/first.txt b/first.txt\n--- a/first.txt\n+++ b/first.txt\n@@ -1 +1 @@\n-before\n+after\ndiff --git a/second.txt b/second.txt\n--- a/second.txt\n+++ b/second.txt\n@@ -1 +1 @@\n-old\n+new\n".into();
                    app.ensure_show_rows();
                    app.toggle_show_file();
                    for _ in 0..depth {
                        let mut parent = App::new(Vec::new());
                        parent.show_stat = summary;
                        parent.show_text = format!(
                            "diff --git a/module b/module\nindex {}..{} 160000\n",
                            "1".repeat(40),
                            "2".repeat(40)
                        );
                        parent.ensure_show_rows();
                        parent.submodules.insert("module".into(), Box::new(app));
                        parent.expanded_folds.insert("module".into());
                        parent.rebuild_show_rows();
                        app = parent;
                    }
                    app.mode = Mode::Show;
                    app.show_cursor = app
                        .show_rows
                        .iter()
                        .position(|row| row.text.contains("+after"))
                        .unwrap();
                    crate::input::handle(
                        Event::Key(KeyEvent::new(key, KeyModifiers::NONE)),
                        &mut app,
                    );
                    let row = &app.show_rows[app.show_cursor];
                    assert!(
                        row.summary && row.folded && row.text.contains("first.txt"),
                        "depth={depth}, summary={summary}, key={key:?}: selected {row:?}"
                    );
                    assert!(!app.show_rows.iter().any(|row| row.text.contains("+after")));

                    // The next press must reopen the same file, not its sibling.
                    crate::input::handle(
                        Event::Key(KeyEvent::new(key, KeyModifiers::NONE)),
                        &mut app,
                    );
                    let row = &app.show_rows[app.show_cursor];
                    assert!(row.summary && !row.folded && row.text.contains("first.txt"));
                    assert!(app.show_rows.iter().any(|row| row.text.contains("+after")));
                }
            }
        }
    }

    #[test]
    fn stat_hotkey_preserves_nested_submodule_reading_lines() {
        fn wrap(child: App) -> App {
            let mut app = App::new(Vec::new());
            app.show_text = format!(
                "diff --git a/module b/module\nindex {}..{} 160000\n",
                "1".repeat(40),
                "2".repeat(40)
            );
            app.ensure_show_rows();
            app.submodules.insert("module".into(), Box::new(child));
            app.expanded_folds.insert("module".into());
            app.rebuild_show_rows();
            app
        }

        for depth in [1, 2] {
            for summary in [false, true] {
                let mut app = App::new(Vec::new());
                app.show_stat = true;
                app.show_text = "diff --git a/file.txt b/file.txt\n--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-before\n+after\n".into();
                app.ensure_show_rows();
                app.toggle_show_file();
                for _ in 0..depth {
                    app = wrap(app);
                }
                app.show_cursor = app
                    .show_rows
                    .iter()
                    .position(|row| {
                        if summary {
                            row.summary && row.text.contains("file.txt")
                        } else {
                            row.text.contains("+after")
                        }
                    })
                    .unwrap();
                let expected = app.show_rows[app.show_cursor].text.clone();
                app.toggle_show_stat();
                assert!(app.show_rows[app.show_cursor].summary);
                assert_eq!(app.show_rows.len(), 1);
                app.toggle_show_stat();
                assert_eq!(
                    app.show_rows[app.show_cursor].text, expected,
                    "depth={depth}, summary={summary}"
                );
                // Leaving summary mode while already inside an expanded child
                // must also preserve that child's selection.
                app.show_stat = true;
                app.rebuild_show_rows();
                app.toggle_show_stat();
                assert_eq!(app.show_rows[app.show_cursor].text, expected);
            }
        }
    }

    #[test]
    fn stat_hotkey_returns_to_the_line_being_read() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let mut app = App::new(Vec::new());
        app.mode = Mode::Show;
        app.show_text = "message\ndiff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n-old\n+reading-here\n".to_owned();
        app.ensure_show_rows();
        app.show_cursor = app
            .show_rows
            .iter()
            .position(|row| row.text == "+reading-here")
            .unwrap();
        for expected in [true, false] {
            crate::input::handle(
                Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)),
                &mut app,
            );
            assert_eq!(app.show_stat, expected);
            if expected {
                assert!(app.show_rows[app.show_cursor].summary);
            }
        }
        assert_eq!(app.show_rows[app.show_cursor].text, "+reading-here");
    }

    #[test]
    fn summary_mode_changed_in_status_applies_when_returning_to_show() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let text = "message\ndiff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n-old\n+reading-here\n";
        let mut app = App::new(vec![commit("x")]);
        app.insert_cache(app.commits[0].hash.clone(), text.to_owned());
        app.mode = Mode::Show;
        app.load_show();
        app.show_cursor = app
            .show_rows
            .iter()
            .position(|row| row.text == "+reading-here")
            .unwrap();
        // Pressing `s` in Status updates the shared setting while Show is hidden.
        app.mode = Mode::Status;
        app.show_stat = true;
        app.mode = Mode::Show;
        app.load_show();
        assert!(app.show_rows[app.show_cursor].summary);
        assert!(!app.show_rows.iter().any(|row| row.text == "+reading-here"));
        crate::input::handle(
            Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)),
            &mut app,
        );
        assert!(!app.show_stat);
        assert_eq!(app.show_rows[app.show_cursor].text, "+reading-here");
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
