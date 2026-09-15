use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Paragraph},
    Frame,
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    process::Command,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Group {
    Conflicts,
    Staged,
    Unstaged,
    Untracked,
}
impl Group {
    fn title(self) -> &'static str {
        match self {
            Self::Conflicts => "Conflicts",
            Self::Staged => "Staged",
            Self::Unstaged => "Unstaged",
            Self::Untracked => "Untracked",
        }
    }
    fn color(self) -> Color {
        match self {
            Self::Conflicts => Color::LightRed,
            Self::Staged => Color::Green,
            Self::Unstaged => Color::Red,
            Self::Untracked => Color::Yellow,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    group: Group,
    path: String,
}
#[derive(Clone, Debug)]
struct Entry {
    key: Key,
    label: String,
    original: Option<String>,
}
#[derive(Default)]
struct Snapshot {
    branch: String,
    upstream: String,
    ahead_behind: String,
    initial: bool,
    entries: Vec<Entry>,
}

fn parse(bytes: &[u8]) -> Result<Snapshot, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "Status contains a non-UTF-8 filename".to_owned())?;
    let mut snapshot = Snapshot::default();
    let mut records = text.split('\0').filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        if let Some(branch) = record.strip_prefix("# branch.head ") {
            snapshot.branch = branch.to_owned();
            continue;
        }
        if let Some(upstream) = record.strip_prefix("# branch.upstream ") {
            snapshot.upstream = upstream.to_owned();
            continue;
        }
        if let Some(ab) = record.strip_prefix("# branch.ab ") {
            snapshot.ahead_behind = ab
                .split_once(' ')
                .map(|(ahead, behind)| {
                    format!(
                        "ahead {}, behind {}",
                        ahead.trim_start_matches('+'),
                        behind.trim_start_matches('-')
                    )
                })
                .unwrap_or_else(|| ab.to_owned());
            continue;
        }
        if record == "# branch.oid (initial)" {
            snapshot.initial = true;
            continue;
        }
        if record.starts_with('#') {
            continue;
        }
        if let Some(path) = record.strip_prefix("? ") {
            snapshot.entries.push(Entry {
                key: Key {
                    group: Group::Untracked,
                    path: path.to_owned(),
                },
                label: "new".into(),
                original: None,
            });
            continue;
        }
        let fields: Vec<_> = record
            .splitn(
                match record.as_bytes()[0] {
                    b'1' => 9,
                    b'2' => 10,
                    b'u' => 11,
                    _ => return Err("Unrecognized Git status record".into()),
                },
                ' ',
            )
            .collect();
        let expected = match record.as_bytes()[0] {
            b'1' => 9,
            b'2' => 10,
            _ => 11,
        };
        if fields.len() != expected || fields[1].len() != 2 {
            return Err("Malformed Git status record".into());
        }
        let path = fields.last().unwrap().to_string();
        let original = if record.starts_with("2 ") {
            Some(records.next().ok_or("Missing rename source")?.to_owned())
        } else {
            None
        };
        if record.starts_with("u ") {
            snapshot.entries.push(Entry {
                key: Key {
                    group: Group::Conflicts,
                    path,
                },
                label: match fields[1] {
                    "DD" => "both deleted",
                    "AU" => "added by us",
                    "UD" => "deleted by them",
                    "UA" => "added by them",
                    "DU" => "deleted by us",
                    "AA" => "both added",
                    "UU" => "both modified",
                    _ => "unmerged",
                }
                .to_owned(),
                original,
            });
            continue;
        }
        for (index, group) in [Group::Staged, Group::Unstaged].into_iter().enumerate() {
            let code = fields[1].as_bytes()[index];
            if code != b'.' {
                let label = match code {
                    b'M' => "modified",
                    b'A' => "added",
                    b'D' => "deleted",
                    b'R' => "renamed",
                    b'C' => "copied",
                    b'T' => "type changed",
                    _ => "changed",
                };
                snapshot.entries.push(Entry {
                    key: Key {
                        group,
                        path: path.clone(),
                    },
                    label: label.into(),
                    original: original.clone(),
                });
            }
        }
    }
    Ok(snapshot)
}

fn visible(text: &str) -> String {
    text.chars()
        .flat_map(|ch| {
            if ch.is_control() {
                ch.escape_default().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}
#[derive(Clone, PartialEq, Eq)]
enum RowKey {
    Group(Group),
    File(Key),
    Patch(Key, usize),
}
#[derive(Clone)]
struct Row {
    key: RowKey,
    text: String,
    color: Option<Color>,
}
#[derive(Default)]
pub struct StatusView {
    root: PathBuf,
    snapshot: Snapshot,
    collapsed: HashSet<Group>,
    patches: HashMap<Key, String>,
    rows: Vec<Row>,
    pub cursor: usize,
    offset: usize,
    pub horizontal: u16,
    pub height: usize,
    pub origin: u16,
    pub error: Option<String>,
}
impl StatusView {
    pub fn load() -> Result<Self, String> {
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        let mut view = Self {
            root: PathBuf::from(String::from_utf8_lossy(&output.stdout).trim_end_matches('\n')),
            ..Self::default()
        };
        view.refresh()?;
        Ok(view)
    }
    fn patch(&self, entry: &Entry) -> Result<String, String> {
        if entry.key.group == Group::Untracked {
            return crate::git::show_untracked(&self.root.join(&entry.key.path).to_string_lossy());
        }
        let mut command = Command::new("git");
        command
            .arg("--literal-pathspecs")
            .arg("-C")
            .arg(&self.root)
            .args(["diff", "--color=always", "--no-ext-diff", "--no-textconv"]);
        if entry.key.group == Group::Staged {
            command.arg("--cached");
        }
        command.arg("--").arg(&entry.key.path);
        if let Some(original) = &entry.original {
            command.arg(original);
        }
        let output = command.output().map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
    pub fn refresh(&mut self) -> Result<(), String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args([
                "status",
                "--porcelain=v2",
                "--branch",
                "--untracked-files=all",
                "-z",
            ])
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        let snapshot = parse(&output.stdout)?;
        let mut patches = HashMap::new();
        for entry in &snapshot.entries {
            if self.patches.contains_key(&entry.key) {
                patches.insert(entry.key.clone(), self.patch(entry)?);
            }
        }
        self.snapshot = snapshot;
        self.patches = patches;
        self.error = None;
        self.rebuild();
        Ok(())
    }
    fn rebuild(&mut self) {
        let old = self.rows.get(self.cursor).cloned();
        let screen = self.cursor.saturating_sub(self.offset);
        self.rows.clear();
        for group in [
            Group::Conflicts,
            Group::Staged,
            Group::Unstaged,
            Group::Untracked,
        ] {
            let entries: Vec<_> = self
                .snapshot
                .entries
                .iter()
                .filter(|entry| entry.key.group == group)
                .collect();
            if group == Group::Conflicts && entries.is_empty() {
                continue;
            }
            self.rows.push(Row {
                key: RowKey::Group(group),
                text: format!(
                    "{} {} ({})",
                    if self.collapsed.contains(&group) {
                        "▶"
                    } else {
                        "▼"
                    },
                    group.title(),
                    entries.len()
                ),
                color: Some(group.color()),
            });
            if self.collapsed.contains(&group) {
                continue;
            }
            for entry in entries {
                let expanded = self.patches.contains_key(&entry.key);
                let name = match &entry.original {
                    Some(original) => {
                        format!("{} → {}", visible(original), visible(&entry.key.path))
                    }
                    None => visible(&entry.key.path),
                };
                self.rows.push(Row {
                    key: RowKey::File(entry.key.clone()),
                    text: format!(
                        "  {} {}  {}",
                        if expanded { "▼" } else { "▶" },
                        entry.label,
                        name
                    ),
                    color: Some(group.color()),
                });
                if let Some(patch) = self.patches.get(&entry.key) {
                    for (index, line) in patch.lines().enumerate() {
                        self.rows.push(Row {
                            key: RowKey::Patch(entry.key.clone(), index),
                            text: line.to_owned(),
                            color: None,
                        });
                    }
                }
            }
        }
        let position = old.as_ref().and_then(|old| {
            if let RowKey::Patch(key, _) = &old.key {
                self.rows
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| {
                        matches!(&row.key, RowKey::Patch(current, _) if current == key)
                            && row.text == old.text
                    })
                    .min_by_key(|(index, _)| index.abs_diff(self.cursor))
                    .map(|(index, _)| index)
                    .or_else(|| self.rows.iter().position(|row| row.key == old.key))
            } else {
                self.rows.iter().position(|row| row.key == old.key)
            }
        });
        self.cursor = position.unwrap_or(self.cursor.min(self.rows.len().saturating_sub(1)));
        self.offset = self.cursor.saturating_sub(screen);
    }
    pub fn move_by(&mut self, delta: isize) {
        self.cursor = self
            .cursor
            .saturating_add_signed(delta)
            .min(self.rows.len().saturating_sub(1));
    }
    pub fn bottom(&mut self) {
        self.cursor = self.rows.len().saturating_sub(1);
    }
    pub fn toggle(&mut self) {
        let Some(row) = self.rows.get(self.cursor) else {
            return;
        };
        match row.key.clone() {
            RowKey::Group(group) => {
                if !self.collapsed.remove(&group) {
                    self.collapsed.insert(group);
                }
            }
            RowKey::File(key) | RowKey::Patch(key, _) => {
                if self.patches.remove(&key).is_none() {
                    if let Some(entry) = self.snapshot.entries.iter().find(|entry| entry.key == key)
                    {
                        match self.patch(entry) {
                            Ok(patch) => {
                                self.patches.insert(key.clone(), patch);
                            }
                            Err(error) => {
                                self.error = Some(error);
                                return;
                            }
                        }
                    }
                }
                self.cursor = self
                    .rows
                    .iter()
                    .position(|row| row.key == RowKey::File(key.clone()))
                    .unwrap_or(self.cursor);
            }
        }
        self.error = None;
        self.rebuild();
    }
    pub fn click(&mut self, y: u16) {
        if let Some(relative) = y.checked_sub(self.origin) {
            let index = self.offset + usize::from(relative);
            if index < self.rows.len() && usize::from(relative) < self.height {
                self.cursor = index;
                self.toggle();
            }
        }
    }
    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let branch = if self.snapshot.branch == "(detached)" {
            "Detached HEAD"
        } else {
            &self.snapshot.branch
        };
        let mut heading = format!(
            "{branch}{}",
            if self.snapshot.initial {
                " · No commits yet"
            } else {
                ""
            }
        );
        if !self.snapshot.upstream.is_empty() {
            heading.push_str(&format!(
                " → {} {}",
                self.snapshot.upstream, self.snapshot.ahead_behind
            ));
        }
        if self.snapshot.entries.is_empty() {
            heading.push_str(" · Working tree clean");
        }
        frame.render_widget(
            Paragraph::new(visible(&heading)).style(Style::default().add_modifier(Modifier::BOLD)),
            Rect::new(area.x, area.y, area.width, area.height.min(1)),
        );
        self.origin = area.y.saturating_add(1);
        self.height = usize::from(area.height.saturating_sub(1));
        if self.cursor < self.offset {
            self.offset = self.cursor;
        }
        if self.cursor >= self.offset + self.height.max(1) {
            self.offset = self.cursor + 1 - self.height.max(1);
        }
        self.offset = self.offset.min(self.rows.len().saturating_sub(self.height));
        for (screen, row) in self
            .rows
            .iter()
            .skip(self.offset)
            .take(self.height)
            .enumerate()
        {
            let mut line = crate::ansi::normalized_line(&row.text);
            if let Some(color) = row.color {
                line = Line::raw(row.text.clone()).style(Style::default().fg(color));
            }
            if self.offset + screen == self.cursor {
                line.style = line.style.bg(Color::DarkGray);
            }
            let row_area = Rect::new(area.x, self.origin + screen as u16, area.width, 1);
            if self.offset + screen == self.cursor {
                frame.render_widget(
                    Block::default().style(Style::default().bg(Color::DarkGray)),
                    row_area,
                );
            }
            frame.render_widget(
                Paragraph::new(line).scroll((0, self.horizontal)),
                Rect::new(area.x, self.origin + screen as u16, area.width, 1),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn status_records_separate_index_worktree_untracked_and_conflicts() {
        let data = b"# branch.head feature\0# branch.upstream origin/main\0# branch.ab +2 -1\x001 MM N... 100644 100644 100644 a b both.txt\0? new file.txt\0u UU N... 100644 100644 100644 100644 a b c conflict.txt\0";
        let parsed = parse(data).unwrap();
        assert_eq!(parsed.branch, "feature");
        assert_eq!(parsed.upstream, "origin/main");
        assert_eq!(parsed.ahead_behind, "ahead 2, behind 1");
        assert_eq!(
            parsed
                .entries
                .iter()
                .map(|entry| entry.key.group)
                .collect::<Vec<_>>(),
            [
                Group::Staged,
                Group::Unstaged,
                Group::Untracked,
                Group::Conflicts
            ]
        );
        assert_eq!(parsed.entries[0].key.path, parsed.entries[1].key.path);
    }

    #[test]
    fn rename_records_keep_both_paths_without_splitting_whitespace() {
        let parsed = parse(
            b"2 R. N... 100644 100644 100644 a b R100 new\nname.txt\0old name.txt\0? other.txt\0",
        )
        .unwrap();
        assert_eq!(parsed.entries[0].key.path, "new\nname.txt");
        assert_eq!(parsed.entries[0].original.as_deref(), Some("old name.txt"));
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(visible(&parsed.entries[0].key.path), "new\\nname.txt");
        assert!(parse(b"2 R. N... 100644 100644 100644 a b R100 new.txt\0").is_err());
    }

    #[test]
    fn status_preserves_collapsed_groups_and_patch_line_after_insertions() {
        let snapshot = parse(b"1 .M N... 100644 100644 100644 a b file.txt\0").unwrap();
        let key = snapshot.entries[0].key.clone();
        let mut view = StatusView {
            snapshot,
            ..StatusView::default()
        };
        view.collapsed.insert(Group::Staged);
        view.patches
            .insert(key.clone(), "one\nreading\nafter".into());
        view.rebuild();
        view.cursor = view
            .rows
            .iter()
            .position(|row| row.text == "reading")
            .unwrap();
        view.offset = view.cursor - 1;
        view.patches
            .insert(key, "inserted\none\nreading\nafter".into());
        view.rebuild();
        assert_eq!(view.rows[view.cursor].text, "reading");
        assert_eq!(view.cursor - view.offset, 1);
        assert!(view.collapsed.contains(&Group::Staged));
        view.toggle();
        assert!(matches!(view.rows[view.cursor].key, RowKey::File(_)));
        assert!(view.patches.is_empty());
    }

    #[test]
    fn working_tree_detail_keyboard_mouse_and_conflict_sections() {
        use crate::{
            app::{App, Mode},
            input,
        };
        use crossterm::event::{
            Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
        };
        let mut view = StatusView { snapshot: parse(b"# branch.head feature\0u UU N... 100644 100644 100644 100644 a b c conflict.txt\0? new.txt\0").unwrap(), ..StatusView::default() };
        view.rebuild();
        let mut app = App::new(vec![crate::git::working_tree_commit(false)]);
        app.status_view = Some(view);
        app.mode = Mode::Status;
        let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("Log │ Status"));
        assert!(screen.contains("▼ Conflicts (1)"));
        assert!(screen.contains("both modified  conflict.txt"));
        input::handle(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &mut app,
        );
        assert!(app
            .status_view
            .as_ref()
            .unwrap()
            .collapsed
            .contains(&Group::Conflicts));
        input::handle(
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            &mut app,
        );
        assert_eq!(app.mode, Mode::Log);
        input::handle(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: app.show_tab_start + 2,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
            &mut app,
        );
        assert_eq!(app.mode, Mode::Status);
        assert!(app
            .status_view
            .as_ref()
            .unwrap()
            .collapsed
            .contains(&Group::Conflicts));
        for expected in [Mode::Log, Mode::Status, Mode::Log] {
            input::handle(
                Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
                &mut app,
            );
            assert_eq!(app.mode, expected);
        }
    }

    #[test]
    fn clean_and_unborn_status_render_without_commits() {
        for (record, expected) in [
            ("# branch.head main\0", "main · Working tree clean"),
            (
                "# branch.head main\0# branch.oid (initial)\0",
                "No commits yet",
            ),
            ("# branch.head (detached)\0", "Detached HEAD"),
        ] {
            let mut view = StatusView {
                snapshot: parse(record.as_bytes()).unwrap(),
                ..StatusView::default()
            };
            view.rebuild();
            let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
            terminal
                .draw(|frame| view.draw(frame, frame.area()))
                .unwrap();
            let heading: String = (0..80)
                .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
                .collect();
            assert!(heading.contains(expected), "{heading}");
            assert_eq!(view.rows.len(), 3);
        }
    }
}
