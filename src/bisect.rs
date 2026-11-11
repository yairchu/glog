//! Read-only landmarks of the current bisect session, connected by ancestry.
use std::{
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    path::PathBuf,
};

use crate::{
    app::{App, History, WatchSource},
    git::{self, Annotation},
};
use ratatui::{
    style::{Color, Style},
    text::Span,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    results: Vec<Outcome>,
    head: bool,
    pub testing: bool,
    culprit: bool,
    possible_culprit: bool,
    intermediate: bool,
    pub gaps: Vec<Control>,
}

/// A recorded step, classified when parsed because sessions can rename terms.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Outcome {
    step: usize,
    term: String,
    verdict: Verdict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    New,
    Old,
    Skip,
}

/// The session's names for new (bad) and old (good) commits.
struct Terms {
    new: String,
    old: String,
}

impl Terms {
    fn load() -> Self {
        log_path()
            .ok()
            .and_then(|path| std::fs::read_to_string(path.with_file_name("BISECT_TERMS")).ok())
            .and_then(|text| {
                let mut lines = text.lines();
                Some(Self {
                    new: lines.next()?.to_owned(),
                    old: lines.next()?.to_owned(),
                })
            })
            .unwrap_or_else(|| Self {
                new: "bad".into(),
                old: "good".into(),
            })
    }

    fn verdict(&self, term: &str) -> Verdict {
        if term == self.new {
            Verdict::New
        } else if term == self.old {
            Verdict::Old
        } else {
            Verdict::Skip
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Control {
    pub edge: crate::compact_graph::Edge,
    pub count: usize,
    pub expanded: bool,
}

impl Control {
    pub fn label(&self) -> String {
        format!(
            "{} {} commit{} → {} ",
            if self.expanded { "▼" } else { "▶" },
            self.count,
            if self.count == 1 { "" } else { "s" },
            &self.edge.to[..7.min(self.edge.to.len())]
        )
    }
}

/// Display-only range vertices keep labels on their graph connections.
pub enum Target {
    Commit(usize),
    Gap { source: usize, control: Control },
}

pub struct Row {
    pub target: Target,
    commit: git::Commit,
}

#[derive(Default)]
pub struct Display {
    pub rows: Vec<Row>,
    drawing: crate::log_folds::LogFolds,
}

impl Display {
    fn new(
        commits: &[git::Commit],
        sections: &[crate::compact_graph::Section],
    ) -> Result<Self, String> {
        let mut rows = Vec::new();
        for (index, commit) in commits.iter().enumerate() {
            let mut vertex = commit.clone();
            let mut gaps = Vec::new();
            if let Some(Annotation::Bisect(entry)) = &commit.annotation {
                for control in &entry.gaps {
                    let section = sections
                        .iter()
                        .find(|section| section.edge == control.edge)
                        .ok_or("missing bisect range")?;
                    let roots: Vec<_> = commit
                        .parents
                        .iter()
                        .filter(|parent| {
                            **parent == control.edge.to || section.members.contains(parent)
                        })
                        .cloned()
                        .collect();
                    if roots.is_empty() {
                        continue;
                    }
                    let hash = format!("bisect-range/{}/{}", control.edge.from, control.edge.to);
                    vertex.parents.retain(|parent| !roots.contains(parent));
                    vertex.parents.push(hash.clone());
                    let mut gap = commit.clone();
                    gap.hash = hash;
                    gap.parents = roots;
                    gap.graph_node = '│';
                    gaps.push(Row {
                        target: Target::Gap {
                            source: index,
                            control: control.clone(),
                        },
                        commit: gap,
                    });
                }
            }
            rows.push(Row {
                target: Target::Commit(index),
                commit: vertex,
            });
            rows.extend(gaps);
        }
        let mut drawing = crate::log_folds::LogFolds::default();
        drawing.refresh(
            &rows
                .iter()
                .map(|row| row.commit.clone())
                .collect::<Vec<_>>(),
        )?;
        Ok(Self { rows, drawing })
    }

    pub fn selected(&self, commit: usize, focus: Option<&crate::compact_graph::Edge>) -> usize {
        self.rows
            .iter()
            .position(|row| match &row.target {
                Target::Commit(index) => *index == commit && focus.is_none(),
                Target::Gap { source, control } => {
                    *source == commit && focus == Some(&control.edge)
                }
            })
            .or_else(|| {
                self.rows
                    .iter()
                    .position(|row| matches!(row.target, Target::Commit(index) if index == commit))
            })
            .unwrap_or(0)
    }

    pub fn height(&self, index: usize) -> usize {
        self.drawing.display_height(index, &self.rows[index].commit)
    }

    pub fn graph(&self, index: usize) -> &[String] {
        self.drawing.display_graph(index, &self.rows[index].commit)
    }
}

/// The full topology stays cheap; metadata is loaded only for visible commits.
pub struct Graph {
    ancestry: crate::compact_graph::Graph,
    landmarks: HashSet<String>,
    sections: Vec<crate::compact_graph::Section>,
    expanded: HashSet<crate::compact_graph::Edge>,
    metadata: HashMap<String, git::Commit>,
    date_args: Vec<String>,
    pub focus: Option<crate::compact_graph::Edge>,
    pub display: Display,
    pub remaining_steps: Option<usize>,
}

impl Graph {
    fn new(
        ancestry: crate::compact_graph::Graph,
        rows: Vec<git::Commit>,
        date_args: Vec<String>,
    ) -> Self {
        let results: HashSet<_> = rows.iter().map(|row| &row.hash).collect();
        let shown = ancestry.with_junctions(
            &ancestry
                .nodes
                .iter()
                .map(|node| results.contains(&node.hash))
                .collect::<Vec<_>>(),
        );
        let landmarks: HashSet<_> = ancestry
            .nodes
            .iter()
            .zip(&shown)
            .filter(|(_, shown)| **shown)
            .map(|(node, _)| node.hash.clone())
            .collect();
        let sections = ancestry.sections(&shown);
        Self {
            ancestry,
            landmarks,
            sections,
            expanded: HashSet::new(),
            metadata: rows
                .into_iter()
                .map(|row| (row.hash.clone(), row))
                .collect(),
            date_args,
            focus: None,
            display: Display::default(),
            remaining_steps: None,
        }
    }

    fn revealed(&self) -> HashSet<String> {
        self.sections
            .iter()
            .filter(|section| self.expanded.contains(&section.edge))
            .flat_map(|section| section.members.iter().cloned())
            .collect()
    }

    pub fn containing(&self, hash: &str) -> Option<crate::compact_graph::Edge> {
        self.sections
            .iter()
            .find(|section| {
                self.expanded.contains(&section.edge)
                    && section.members.iter().any(|member| member == hash)
            })
            .map(|section| section.edge.clone())
    }

    pub fn inherit(&mut self, old: &Self) {
        let revealed = old.revealed();
        self.expanded = self
            .sections
            .iter()
            .filter(|section| {
                old.expanded.contains(&section.edge)
                    || section.members.iter().any(|hash| revealed.contains(hash))
            })
            .map(|section| section.edge.clone())
            .collect();
        self.focus = old
            .focus
            .as_ref()
            .filter(|edge| self.sections.iter().any(|section| &section.edge == *edge))
            .cloned();
    }

    pub fn rows(&mut self) -> Result<Vec<git::Commit>, String> {
        let mut shown = self.revealed();
        shown.extend(self.landmarks.iter().cloned());
        let shown: Vec<_> = self
            .ancestry
            .nodes
            .iter()
            .map(|node| shown.contains(&node.hash))
            .collect();
        let missing: Vec<_> = self
            .ancestry
            .nodes
            .iter()
            .enumerate()
            .filter(|(index, node)| shown[*index] && !self.metadata.contains_key(&node.hash))
            .map(|(_, node)| node.hash.clone())
            .collect();
        if !missing.is_empty() {
            self.metadata.extend(
                load_metadata(&missing, &self.date_args)?
                    .into_iter()
                    .map(|row| (row.hash.clone(), row)),
            );
        }
        let parents = self.ancestry.project(&shown, false);
        let mut rows = Vec::new();
        for (index, node) in self
            .ancestry
            .nodes
            .iter()
            .enumerate()
            .filter(|(index, _)| shown[*index])
        {
            let mut row = self
                .metadata
                .get(&node.hash)
                .ok_or("missing expanded commit metadata")?
                .clone();
            row.parents = parents[&index]
                .iter()
                .map(|parent| self.ancestry.nodes[*parent].hash.clone())
                .collect();
            let mut entry = match row.annotation.take() {
                Some(Annotation::Bisect(entry)) => entry,
                _ => Entry {
                    intermediate: true,
                    ..Entry::default()
                },
            };
            entry.gaps = self
                .sections
                .iter()
                .filter(|section| section.edge.from == node.hash)
                .map(|section| Control {
                    edge: section.edge.clone(),
                    count: section.members.len(),
                    expanded: self.expanded.contains(&section.edge),
                })
                .collect();
            row.annotation = Some(Annotation::Bisect(entry));
            rows.push(row);
        }
        self.display = Display::new(&rows, &self.sections)?;
        Ok(rows)
    }

    pub fn toggle(
        &mut self,
        edge: &crate::compact_graph::Edge,
    ) -> Result<Vec<git::Commit>, String> {
        if !self.sections.iter().any(|section| &section.edge == edge) {
            return self.rows();
        }
        let was_expanded = !self.expanded.insert(edge.clone());
        if was_expanded {
            self.expanded.remove(edge);
        }
        match self.rows() {
            Ok(rows) => Ok(rows),
            Err(error) => {
                if was_expanded {
                    self.expanded.insert(edge.clone());
                } else {
                    self.expanded.remove(edge);
                }
                Err(error)
            }
        }
    }
}

fn log_path() -> Result<PathBuf, String> {
    let output = git::read_command()
        .args(["rev-parse", "--git-path", "BISECT_LOG"])
        .output()
        .map_err(|e| format!("could not locate bisect log: {e}"))?;
    if !output.status.success() {
        return Err(git::stderr_message(
            "could not locate bisect log",
            &output.stderr,
        ));
    }
    Ok(git::raw_path(
        output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout),
    ))
}

fn read_log() -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(log_path()?) {
        Ok(log) => Ok(Some(log)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("could not read bisect log: {e}")),
    }
}

pub fn fingerprint() -> Result<u64, String> {
    let mut state = DefaultHasher::new();
    git::refs_fingerprint_at(None)?.hash(&mut state);
    read_log()?.hash(&mut state);
    std::fs::read(log_path()?.with_file_name("BISECT_HEAD"))
        .ok()
        .hash(&mut state);
    Ok(state.finish())
}

/// Decode Git's single-quoted argv format without evaluating a shell command.
fn session_paths(bytes: &[u8]) -> Option<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
            continue;
        }
        let mut word = Vec::new();
        while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
            match bytes[index] {
                b'\'' => {
                    index += 1;
                    while *bytes.get(index)? != b'\'' {
                        word.push(bytes[index]);
                        index += 1;
                    }
                    index += 1;
                }
                b'\\' => {
                    index += 1;
                    word.push(*bytes.get(index)?);
                    index += 1;
                }
                _ => return None,
            }
        }
        paths.push(git::raw_path(&word));
    }
    // Git versions may retain the original path separator in BISECT_NAMES.
    if paths
        .first()
        .is_some_and(|path| path == std::path::Path::new("--"))
    {
        paths.remove(0);
    }
    Some(paths)
}

/// Git's machine variable estimates further steps after the current test.
fn remaining_steps() -> Option<usize> {
    let state = log_path().ok()?;
    let paths = session_paths(&std::fs::read(state.with_file_name("BISECT_NAMES")).ok()?)?;
    let repository = git::Repository::main().ok()?;
    let mut command = git::read_command_at(Some(&repository));
    command.args(["rev-list", "--bisect-vars", "--bisect"]);
    if state.with_file_name("BISECT_FIRST_PARENT").exists() {
        command.arg("--first-parent");
    }
    command.arg("--").args(paths);
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("bisect_steps=")?.parse().ok())
}

fn parse(log: &str, terms: &Terms) -> HashMap<String, Entry> {
    let mut entries = HashMap::<String, Entry>::new();
    let mut step = 0;
    let mut has_outcome = false;
    for line in log.lines() {
        let Some((role, rest)) = line.strip_prefix("# ").and_then(|s| s.split_once(": [")) else {
            continue;
        };
        let Some((hash, _)) = rest.split_once(']') else {
            continue;
        };
        if !matches!(hash.len(), 40 | 64) || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        if role.starts_with("possible first ") && role.ends_with(" commit") {
            has_outcome = true;
            entries.entry(hash.into()).or_default().possible_culprit = true;
        } else if role.starts_with("first ") && role.ends_with(" commit") {
            has_outcome = true;
            entries.entry(hash.into()).or_default().culprit = true;
        } else if !role.contains(char::is_whitespace) {
            // Git permits another classification after reporting an outcome.
            // Only the latest outcome describes the current search.
            if has_outcome {
                for entry in entries.values_mut() {
                    entry.culprit = false;
                    entry.possible_culprit = false;
                }
                has_outcome = false;
            }
            step += 1;
            entries
                .entry(hash.into())
                .or_default()
                .results
                .push(Outcome {
                    step,
                    term: role.into(),
                    verdict: terms.verdict(role),
                });
        }
    }
    entries
}

pub fn load_app(args: &[String]) -> Result<App, String> {
    let (watch, args) = crate::app::watch_args(args);
    let args = args.strip_prefix(&["log".into()]).unwrap_or(&args);
    let mut date_args = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--date" {
            date_args.push(arg.clone());
            date_args.push(iter.next().ok_or("--date requires a value")?.clone());
        } else if arg.starts_with("--date=") {
            date_args.push(arg.clone());
        } else {
            return Err("use glog bisect log [--watch] [--date=STYLE]".into());
        }
    }
    let source = WatchSource::Bisect(args.to_vec());
    let snapshot = watch.then(|| source.snapshot(None));
    let mut rows = Vec::new();
    let mut bisect_graph = None;
    if let Some(log) = read_log()? {
        let mut entries = parse(&String::from_utf8_lossy(&log), &Terms::load());
        let output = git::read_command()
            .args(["rev-parse", "--verify", "HEAD^{commit}"])
            .output()
            .map_err(|e| format!("could not read HEAD: {e}"))?;
        if output.status.success() {
            let head = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            let finished = entries
                .values()
                .any(|entry| entry.culprit || entry.possible_culprit);
            let entry = entries.entry(head).or_default();
            entry.head = true;
            entry.testing = entry.results.is_empty() && !finished;
        }
        // In --no-checkout mode Git records the candidate separately from HEAD.
        let candidate = log_path()?.with_file_name("BISECT_HEAD");
        if let Ok(hash) = std::fs::read_to_string(candidate) {
            let hash = hash.trim();
            if matches!(hash.len(), 40 | 64) && hash.bytes().all(|c| c.is_ascii_hexdigit()) {
                for entry in entries.values_mut() {
                    entry.testing = false;
                }
                let finished = entries
                    .values()
                    .any(|entry| entry.culprit || entry.possible_culprit);
                entries.entry(hash.into()).or_default().testing = !finished;
            }
        }
        let completed = entries
            .values()
            .any(|entry| entry.culprit || entry.possible_culprit);
        let remaining = if completed { None } else { remaining_steps() };
        let (landmarks, ancestry) = crate::branches::selected_graph(entries.keys().cloned())?;
        if !landmarks.is_empty() {
            let hashes: Vec<_> = landmarks.iter().map(|row| row.hash.clone()).collect();
            let metadata = load_metadata(&hashes, &date_args)?;
            let metadata_rows = metadata
                .into_iter()
                .map(|mut row| {
                    row.annotation = Some(Annotation::Bisect(
                        entries.remove(&row.hash).unwrap_or_default(),
                    ));
                    row
                })
                .collect();
            let mut graph = Graph::new(ancestry, metadata_rows, date_args);
            graph.remaining_steps = remaining;
            rows = graph.rows()?;
            bisect_graph = Some(graph);
        }
    }

    let selected = selection(&rows).unwrap_or(0);
    let mut app = App::new(rows);
    app.history = History::Bisect;
    app.bisect_graph = bisect_graph;
    app.selected = selected;
    app.center_selection = true;
    app.watch = watch;
    app.set_watch_source(source, snapshot);
    Ok(app)
}

/// Prefer the candidate, then a completed culprit, then the actual checkout.
pub fn selection(rows: &[git::Commit]) -> Option<usize> {
    rows.iter()
        .enumerate()
        .filter_map(|(index, row)| {
            let Some(Annotation::Bisect(entry)) = &row.annotation else {
                return None;
            };
            let priority = if entry.testing {
                0
            } else if entry.culprit {
                1
            } else if entry.head {
                2
            } else {
                return None;
            };
            Some((priority, index))
        })
        .min()
        .map(|(_, index)| index)
}

#[derive(Clone, Copy)]
pub struct Columns {
    step: usize,
    role: usize,
    decision: usize,
}

impl Columns {
    pub fn measure(commits: &[git::Commit]) -> Self {
        let entries: Vec<_> = commits
            .iter()
            .filter_map(|commit| match &commit.annotation {
                Some(Annotation::Bisect(entry)) => Some(entry),
                _ => None,
            })
            .collect();
        let mut columns = Self {
            step: entries
                .iter()
                .flat_map(|entry| &entry.results)
                .map(|outcome| outcome.step.to_string().len())
                .max()
                .unwrap_or(1),
            role: entries
                .iter()
                .flat_map(|entry| &entry.results)
                .map(|outcome| Span::raw(&outcome.term).width())
                .max()
                .unwrap_or(0),
            decision: 0,
        };
        columns.decision = entries
            .iter()
            .map(|entry| {
                decision_spans(entry, Some(&columns))
                    .iter()
                    .map(Span::width)
                    .sum()
            })
            .max()
            .unwrap_or(0);
        columns
    }
}

/// HEAD and the outcome have independent cells, including when they coincide.
pub fn prefix(entry: &Entry, columns: Option<&Columns>) -> crate::log_format::Row {
    let mut spans = vec![Span::styled(
        if entry.head { "HEAD " } else { "     " },
        Style::default().fg(Color::Cyan),
    )];
    let (marker, label, color) = if entry.culprit {
        ("! ", Some("culprit"), Color::LightRed)
    } else if entry.possible_culprit {
        ("? ", Some("possible culprit"), Color::Yellow)
    } else {
        ("  ", None, Color::Reset)
    };
    let mut stand_ins = crate::ui::StandIns::default();
    if let Some(label) = label {
        let start = spans[0].content.len();
        stand_ins.push(start..start + 1, label);
    }
    spans.push(Span::styled(marker, Style::default().fg(color)));
    let decisions = decision_spans(entry, columns);
    let width: usize = decisions.iter().map(Span::width).sum();
    spans.extend(decisions);
    spans.push(Span::raw(" ".repeat(
        columns.map_or(0, |columns| columns.decision.saturating_sub(width)) + 1,
    )));
    crate::log_format::Row {
        spans,
        stand_ins,
        links: Vec::new(),
    }
}

fn decision_spans(entry: &Entry, columns: Option<&Columns>) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if entry.testing {
        spans.push(Span::styled("testing", Style::default().fg(Color::Yellow)));
    }
    for Outcome {
        step,
        term: role,
        verdict,
    } in &entry.results
    {
        if !spans.is_empty() {
            spans.push(Span::raw(" / "));
        }
        let color = match verdict {
            Verdict::Old => Color::Green,
            Verdict::New => Color::Red,
            Verdict::Skip => Color::Yellow,
        };
        let step_width = columns.map_or(1, |columns| columns.step);
        let role = columns.map_or_else(
            || role.clone(),
            |columns| {
                format!(
                    "{role}{}",
                    " ".repeat(columns.role.saturating_sub(Span::raw(role).width()))
                )
            },
        );
        spans.push(Span::styled(
            format!("#{step:>step_width$} {role}"),
            Style::default().fg(color),
        ));
    }
    if !entry.head
        && !entry.testing
        && !entry.culprit
        && !entry.possible_culprit
        && !entry.intermediate
        && entry.results.is_empty()
    {
        spans.push(Span::styled(
            "shared ancestor",
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans
}

fn load_metadata(hashes: &[String], date_args: &[String]) -> Result<Vec<git::Commit>, String> {
    let mut args = date_args.to_vec();
    // Ask Git to omit only HEAD, retaining its attached branch and every other ref.
    args.push("--decorate-refs-exclude=HEAD".into());
    git::load_revisions(hashes, &args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::Mode,
        git::tests::{git, git_command, CurrentDirGuard, TestDirectory},
        input, ui,
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    fn commit(message: &str, parents: &[&str]) -> String {
        std::fs::write("file.txt", format!("{message}\n")).unwrap();
        git(&["add", "file.txt"]);
        let tree = git(&["write-tree"]);
        let mut args = vec!["commit-tree", &tree, "-m", message];
        for parent in parents {
            args.extend(["-p", parent]);
        }
        git(&args)
    }

    fn outcome(step: usize, term: &str, verdict: Verdict) -> Outcome {
        Outcome {
            step,
            term: term.into(),
            verdict,
        }
    }

    fn entry<'a>(app: &'a App, hash: &str) -> &'a Entry {
        match &app
            .commits
            .iter()
            .find(|row| row.hash == hash)
            .unwrap()
            .annotation
        {
            Some(Annotation::Bisect(entry)) => entry,
            _ => panic!("missing bisect annotation"),
        }
    }

    fn press(app: &mut App, code: KeyCode) {
        input::handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), app);
    }

    #[test]
    fn bisect_landmarks_connect_across_untested_commits_and_open_the_actual_patch() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        let old = commit("before range", &[]);
        let good = commit("good endpoint", &[&old]);
        let hidden = commit("untested", &[&good]);
        let middle = commit("candidate", &[&hidden]);
        let bad = commit("bad endpoint", &[&middle]);
        git(&["reset", "--hard", &bad]);
        git(&["bisect", "start", &bad, &good]);
        let head = git(&["rev-parse", "HEAD"]);
        let mut app = load_app(&["log".into()]).unwrap();
        assert_eq!(app.history, History::Bisect);
        assert_eq!(app.commits.len(), 3);
        assert_eq!(app.commits[app.selected].hash, head);
        assert!(entry(&app, &head).testing);
        assert_eq!(
            prefix(entry(&app, &head), None)
                .spans
                .iter()
                .find(|span| span.content == "HEAD ")
                .unwrap()
                .style
                .fg,
            Some(Color::Cyan)
        );
        assert_eq!(
            prefix(entry(&app, &good), None)
                .spans
                .iter()
                .find(|span| span.content.contains("good"))
                .unwrap()
                .style
                .fg,
            Some(Color::Green)
        );
        assert_eq!(entry(&app, &bad).results, [outcome(1, "bad", Verdict::New)]);
        assert_eq!(
            entry(&app, &good).results,
            [outcome(2, "good", Verdict::Old)]
        );
        assert_eq!(app.commits[0].parents, std::slice::from_ref(&head));
        assert_eq!(app.commits[1].parents, std::slice::from_ref(&good));
        let mut terminal = Terminal::new(TestBackend::new(120, 18)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("HEAD   testing"), "{screen}");
        assert!(screen.contains("#1 bad"), "{screen}");
        let selected = app.selected;
        let expected = git(&["show", "--format=", &head]);
        assert!(expected.contains("diff --git"));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Show);
        assert_eq!(app.status, None);
        assert_eq!(
            git::show(&app.commits[selected], &[]).unwrap(),
            git::show(
                &git::load_log(&["-1".into(), head.clone()]).unwrap()[0],
                &[]
            )
            .unwrap()
        );
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.mode, Mode::Log);
        assert_eq!(app.selected, selected);
        press(&mut app, KeyCode::Enter);
        assert!(crate::ansi::plain(&app.show_text).contains("(HEAD)"));
        git(&["bisect", "good", &head]);
        git(&["checkout", "-q", "--detach", &bad]);
        app.toggle_watch();
        assert!(app.poll_watch());
        assert_eq!(app.mode, Mode::Show);
        assert_eq!(app.commits[app.selected].hash, head);
        assert!(!crate::ansi::plain(&app.show_text).contains("(HEAD)"));
        assert_eq!(
            app.show_text,
            git::show(&app.commits[app.selected], &[]).unwrap()
        );
    }

    #[test]
    fn divergent_results_retain_the_shared_ancestor_without_unrelated_branches() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        let old = commit("old", &[]);
        let base = commit("shared", &[&old]);
        let good = commit("good side", &[&base]);
        let other = commit("other side", &[&base]);
        let tip = commit("merge", &[&good, &other]);
        git(&["reset", "--hard", &tip]);
        git(&["branch", "irrelevant", &old]);
        git(&["bisect", "start", "--no-checkout", &tip, &good]);
        let skipped = git_command(&["bisect", "skip", &other]).output().unwrap();
        assert_eq!(skipped.status.code(), Some(2));

        let app = load_app(&[]).unwrap();
        assert_eq!(app.commits.len(), 4);
        assert!(app.commits.iter().all(|row| row.hash != old));
        let base_row = app.commits.iter().find(|row| row.hash == base).unwrap();
        assert!(entry(&app, &base).results.is_empty());
        assert!(app.log_format.text(base_row).contains("shared ancestor"));
        assert_eq!(
            app.commits
                .iter()
                .find(|row| row.hash == other)
                .unwrap()
                .parents,
            [base]
        );
        assert!(entry(&app, &tip).head);
        assert!(entry(&app, &tip).possible_culprit);
        assert!(entry(&app, &other).possible_culprit);
        assert!(app
            .commits
            .iter()
            .all(|row| !matches!(&row.annotation, Some(Annotation::Bisect(e)) if e.testing)));
        assert!(entry(&app, &other)
            .results
            .iter()
            .any(|outcome| outcome.term == "skip"));
        // Resolving a skipped candidate replaces the earlier ambiguous outcome.
        git(&["bisect", "good", &other]);
        let resolved = load_app(&[]).unwrap();
        assert!(entry(&resolved, &tip).culprit);
        assert!(!entry(&resolved, &tip).possible_culprit);
        assert!(!entry(&resolved, &other).possible_culprit);
    }

    #[test]
    fn watches_custom_terms_and_completion_without_checkout_and_clears_after_reset() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        let good = commit("old", &[]);
        let middle = commit("new starts", &[&good]);
        let bad = commit("tip", &[&middle]);
        git(&["reset", "--hard", &bad]);
        let mut app = load_app(&["--watch".into()]).unwrap();
        assert!(app.commits.is_empty());
        git(&[
            "bisect",
            "start",
            "--no-checkout",
            "--term-old=works",
            "--term-new=broken",
            &bad,
            &good,
        ]);
        assert!(app.poll_watch());
        assert!(entry(&app, &bad).head);
        assert!(!entry(&app, &bad).testing);
        assert!(entry(&app, &middle).testing);
        assert_eq!(app.bisect_graph.as_ref().unwrap().remaining_steps, Some(0));
        assert_eq!(app.commits[app.selected].hash, middle);
        let before = fingerprint().unwrap();
        git(&["bisect", "broken"]);
        assert_ne!(fingerprint().unwrap(), before);
        assert!(app.poll_watch());
        assert!(entry(&app, &middle).culprit);
        assert_eq!(app.bisect_graph.as_ref().unwrap().remaining_steps, None);
        assert_eq!(app.commits[app.selected].hash, middle);
        assert!(!entry(&app, &middle).testing);
        assert!(entry(&app, &middle)
            .results
            .iter()
            .any(|outcome| outcome.term == "broken" && outcome.verdict == Verdict::New));
        assert_eq!(git(&["rev-parse", "HEAD"]), bad);
        git(&["bisect", "reset"]);
        assert!(app.poll_watch());
        assert!(app.commits.is_empty());
        assert_eq!(app.history, History::Bisect);
    }
    #[test]
    fn status_bisect_indicator_opens_the_graph_and_restores_status() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        // Keep HEAD attached in no-checkout mode to exercise wide heading text.
        git(&["init", "-qb", "main界"]);
        let good = commit("good", &[]);
        let middle = commit("candidate", &[&good]);
        let bad = commit("bad", &[&middle]);
        git(&["reset", "--hard", &bad]);
        let mut idle = crate::status::load_app(&[]).unwrap();
        idle.open_status();
        let mut terminal = Terminal::new(TestBackend::new(120, 18)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut idle)).unwrap();
        assert!(idle.status_view.as_ref().unwrap().bisect_button.is_none());
        press(&mut idle, KeyCode::Char('B'));
        assert_eq!(idle.mode, Mode::Status);
        assert!(!idle.has_reference_back());
        git(&["bisect", "start", "--no-checkout", &bad, &good]);
        std::fs::write("file.txt", "dirty\n").unwrap();
        for mouse in [false, true] {
            let mut app = crate::status::load_app(&[]).unwrap();
            assert_eq!(app.mode, Mode::Status);
            app.status_view.as_mut().unwrap().cursor = 1;
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            let cursor = app.status_view.as_ref().unwrap().cursor;
            let button = app.status_view.as_ref().unwrap().bisect_button.unwrap();
            // The hitbox starts at the rendered column after the wide branch name.
            assert_eq!(button.x, 9);
            assert_eq!(button.y, 1);
            if mouse {
                input::handle(
                    Event::Mouse(MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: button.x,
                        row: button.y,
                        modifiers: KeyModifiers::NONE,
                    }),
                    &mut app,
                );
            } else {
                press(&mut app, KeyCode::Char('B'));
            }
            assert_eq!(app.history, History::Bisect);
            assert_eq!(app.mode, Mode::Log);
            assert!(app.watch);
            assert_eq!(app.commits[app.selected].hash, middle);
            press(&mut app, KeyCode::Enter);
            assert_eq!(app.mode, Mode::Show);
            press(&mut app, KeyCode::Backspace);
            assert_eq!(app.mode, Mode::Log);
            assert_eq!(app.history, History::Bisect);
            press(
                &mut app,
                if mouse {
                    KeyCode::Esc
                } else {
                    KeyCode::Backspace
                },
            );
            assert_eq!(app.mode, Mode::Status);
            assert_eq!(app.status_view.as_ref().unwrap().cursor, cursor);
            assert!(!app.has_reference_back());
            // A clipped or absent indicator must not leave a stale hitbox.
            let mut tiny = Terminal::new(TestBackend::new(8, 6)).unwrap();
            tiny.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            assert!(app.status_view.as_ref().unwrap().bisect_button.is_none());
        }
        git(&["bisect", "reset"]);
        let mut app = crate::status::load_app(&[]).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert_eq!(app.mode, Mode::Status);
        assert!(app.status_view.as_ref().unwrap().bisect_button.is_none());
        press(&mut app, KeyCode::Char('B'));
        assert_eq!(app.mode, Mode::Status);
        assert!(!app.has_reference_back());
    }
    #[test]
    fn counted_gaps_expand_in_place_and_survive_new_bisect_landmarks() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        let mut hashes = vec![commit("base", &[])];
        for number in 1..12 {
            hashes.push(commit(
                &format!("change {number}"),
                &[hashes.last().unwrap()],
            ));
        }
        let bad = hashes.last().unwrap();
        git(&["reset", "--hard", bad]);
        git(&["bisect", "start", "--no-checkout", bad, &hashes[0]]);
        let mut app = load_app(&["--watch".into()]).unwrap();
        let compact: Vec<_> = app.commits.iter().map(|row| row.hash.clone()).collect();
        let control = app
            .commits
            .iter()
            .find_map(|row| match &row.annotation {
                Some(Annotation::Bisect(entry)) => entry.gaps.first(),
                _ => None,
            })
            .unwrap()
            .clone();
        let from = hashes
            .iter()
            .position(|hash| hash == &control.edge.from)
            .unwrap();
        let to = hashes
            .iter()
            .position(|hash| hash == &control.edge.to)
            .unwrap();
        assert_eq!(control.count, from - to - 1);
        assert!(!control.expanded);
        let expected: HashSet<_> = hashes[to + 1..from].iter().cloned().collect();
        let mut terminal = Terminal::new(TestBackend::new(160, 24)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let (rect, row, edge) = app
            .visible_bisect_gaps
            .iter()
            .find(|(_, _, edge)| edge == &control.edge)
            .unwrap()
            .clone();
        let source_screen = app
            .visible_log_rows
            .iter()
            .position(|visible| *visible == Some(row))
            .unwrap() as u16
            + app.log_row_origin;
        let destination = app
            .commits
            .iter()
            .position(|commit| commit.hash == edge.to)
            .unwrap();
        let destination_screen = app
            .visible_log_rows
            .iter()
            .position(|visible| *visible == Some(destination))
            .unwrap() as u16
            + app.log_row_origin;
        assert!(source_screen < rect.y && rect.y < destination_screen);
        let screen_line = |y| {
            (0..160)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        };
        assert!(screen_line(rect.y).contains(&format!("{} commits", control.count)));
        assert!(!screen_line(source_screen).contains(" commits"));
        assert_eq!(
            app.visible_log_rows[usize::from(rect.y - app.log_row_origin)],
            None
        );
        press(&mut app, KeyCode::Home);
        assert_eq!(app.selected, row);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.focused_bisect_gap(), Some(edge.clone()));
        // A one-row viewport follows the selected range, not its source commit.
        let mut short = Terminal::new(TestBackend::new(160, 3)).unwrap();
        short.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert!(app
            .visible_bisect_gaps
            .iter()
            .any(|(_, _, visible)| visible == &edge));
        press(&mut app, KeyCode::Down);
        assert_eq!(app.selected, destination);
        assert!(app.focused_bisect_gap().is_none());
        press(&mut app, KeyCode::Up);
        assert_eq!(app.focused_bisect_gap(), Some(edge.clone()));
        press(&mut app, KeyCode::Up);
        assert!(app.focused_bisect_gap().is_none());
        app.log_offset = 0;
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let click = |app: &mut App| {
            input::handle(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: rect.x,
                    row: rect.y,
                    modifiers: KeyModifiers::NONE,
                }),
                app,
            )
        };
        click(&mut app);
        assert_eq!(app.commits.len(), compact.len());
        assert_eq!(app.selected, row);
        assert_eq!(app.focused_bisect_gap(), Some(edge));
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert!(app
            .visible_bisect_gaps
            .iter()
            .any(|(new_rect, _, new_edge)| *new_rect == rect && *new_edge == control.edge));
        click(&mut app);
        assert_eq!(app.commits.len(), compact.len() + control.count);
        assert_eq!(app.commits[app.selected].hash, control.edge.from);
        for hash in &expected {
            assert!(entry(&app, hash).intermediate);
            let row = app.commits.iter().find(|row| &row.hash == hash).unwrap();
            assert!(!app.log_format.text(row).contains("shared ancestor"));
        }
        assert!(compact
            .iter()
            .all(|hash| app.commits.iter().any(|row| &row.hash == hash)));
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.commits
                .iter()
                .map(|row| row.hash.clone())
                .collect::<Vec<_>>(),
            compact
        );
        input::handle(
            Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT)),
            &mut app,
        );
        assert!(app.focused_bisect_gap().is_none());
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Show);
        press(&mut app, KeyCode::Backspace);
        input::handle(
            Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT)),
            &mut app,
        );
        assert!(app.focused_bisect_gap().is_some());
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(app.commits.len(), compact.len() + control.count);
        // Folding from within the list restores its source landmark.
        app.selected = app
            .commits
            .iter()
            .position(|row| expected.contains(&row.hash))
            .unwrap();
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(app.commits.len(), compact.len());
        assert_eq!(app.commits[app.selected].hash, control.edge.from);
        press(&mut app, KeyCode::Char('z'));
        // A new candidate splits an open gap. Previously revealed commits stay visible.
        git(&["bisect", "good"]);
        assert!(app.poll_watch());
        assert!(expected
            .iter()
            .all(|hash| app.commits.iter().any(|row| &row.hash == hash)));
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert!(!app.visible_bisect_gaps.is_empty());
        let mut narrow = Terminal::new(TestBackend::new(8, 10)).unwrap();
        narrow.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert!(app
            .visible_bisect_gaps
            .iter()
            .all(|(rect, _, _)| rect.right() <= 8));
    }

    fn projected_app(hashes: &[String]) -> App {
        let (landmarks, ancestry) =
            crate::branches::selected_graph(hashes.iter().cloned()).unwrap();
        let hashes: Vec<_> = landmarks.iter().map(|row| row.hash.clone()).collect();
        let metadata = load_metadata(&hashes, &[])
            .unwrap()
            .into_iter()
            .map(|mut row| {
                row.annotation = Some(Annotation::Bisect(Entry::default()));
                row
            })
            .collect();
        let mut graph = Graph::new(ancestry, metadata, Vec::new());
        let mut app = App::new(graph.rows().unwrap());
        app.bisect_graph = Some(graph);
        app
    }

    #[test]
    fn gaps_count_merge_paths_once_and_expansion_preserves_redundant_parent_routes() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        let good = commit("good", &[]);
        let shared = commit("shared hidden commit", &[&good]);
        let left = commit("left", &[&shared]);
        let right = commit("right", &[&shared]);
        let merge = commit("hidden merge", &[&left, &right]);
        let bad = commit("bad", &[&merge, &good]);
        let mut app = projected_app(&[bad.clone(), good.clone()]);
        assert_eq!(app.commits.len(), 2);
        let control = entry(&app, &bad).gaps[0].clone();
        assert_eq!(control.edge.to, good);
        assert_eq!(control.count, 4);
        app.selected = app.commits.iter().position(|row| row.hash == bad).unwrap();
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(app.commits.len(), 6);
        assert_eq!(
            app.commits
                .iter()
                .map(|row| &row.hash)
                .collect::<HashSet<_>>()
                .len(),
            6
        );
        let parents = |hash: &str| {
            app.commits
                .iter()
                .find(|row| row.hash == hash)
                .unwrap()
                .parents
                .iter()
                .cloned()
                .collect::<HashSet<_>>()
        };
        assert_eq!(parents(&bad), HashSet::from([merge.clone(), good.clone()]));
        assert_eq!(parents(&merge), HashSet::from([left, right]));
        assert_eq!(parents(&shared), HashSet::from([good]));
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(app.commits.len(), 2);
        // A merge splitting toward different landmarks is shown, not shared by two gaps.
        let left = commit("visible left", &[&shared]);
        let right = commit("visible right", &[&shared]);
        let junction = commit("junction", &[&left, &right]);
        let middle = commit("middle", &[&junction]);
        let top = commit("top", &[&middle]);
        let app = projected_app(&[top.clone(), left.clone(), right.clone()]);
        let controls = &entry(&app, &top).gaps;
        assert_eq!(controls.len(), 1);
        assert_eq!(controls[0].edge.to, junction);
        assert_eq!(controls[0].count, 1);
        assert!(entry(&app, &junction).intermediate);
        assert!(entry(&app, &junction).gaps.is_empty());
        let row = app.commits.iter().find(|row| row.hash == junction).unwrap();
        assert_eq!(
            row.parents.iter().cloned().collect::<HashSet<_>>(),
            HashSet::from([left, right])
        );
    }
    #[test]
    fn remaining_steps_follow_gits_scoped_session_estimate_and_skips() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        let good = commit("base", &[]);
        let mut side = good.clone();
        for number in 1..=4 {
            std::fs::write("selected 'tests' café.txt", format!("side {number}\n")).unwrap();
            git(&["add", "selected 'tests' café.txt"]);
            let tree = git(&["write-tree"]);
            side = git(&["commit-tree", &tree, "-p", &side, "-m", "side change"]);
        }
        git(&["read-tree", &good]);
        let mut bad = good.clone();
        for number in 1..=12 {
            if number % 3 == 0 {
                std::fs::write("selected 'tests' café.txt", format!("{number}\n")).unwrap();
                git(&["add", "selected 'tests' café.txt"]);
                let tree = git(&["write-tree"]);
                let mut args = vec!["commit-tree", &tree, "-p", &bad, "-m", "selected change"];
                if number == 12 {
                    args.extend(["-p", &side]);
                }
                bad = git(&args);
            } else {
                bad = commit(&format!("unrelated {number}"), &[&bad]);
            }
        }
        git(&["reset", "--hard", &bad]);
        let output = git(&[
            "bisect",
            "start",
            "--no-checkout",
            "--first-parent",
            "--term-old=works",
            "--term-new=broken",
            &bad,
            &good,
            "--",
            "selected 'tests' café.txt",
        ]);
        assert!(output.contains("(roughly 1 step)"), "{output}");
        let before = git(&["bisect", "log"]);
        let mut app = load_app(&["--watch".into()]).unwrap();
        assert_eq!(app.bisect_graph.as_ref().unwrap().remaining_steps, Some(1));
        assert_eq!(git(&["bisect", "log"]), before);
        assert_eq!(git(&["rev-parse", "HEAD"]), bad);
        let mut terminal = Terminal::new(TestBackend::new(160, 12)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let header: String = (0..160)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect();
        assert!(header.contains("~1 step after this test"), "{header}");
        let output = git(&["bisect", "skip"]);
        assert!(output.contains("(roughly 1 step)"), "{output}");
        assert!(app.poll_watch());
        assert_eq!(app.bisect_graph.as_ref().unwrap().remaining_steps, Some(1));
        std::fs::create_dir("nested").unwrap();
        std::env::set_current_dir("nested").unwrap();
        let nested = load_app(&[]).unwrap();
        assert_eq!(
            nested.bisect_graph.as_ref().unwrap().remaining_steps,
            Some(1)
        );
    }
    #[test]
    fn bisect_columns_align_head_and_culprit_and_keep_real_refs() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        let base = commit("base", &[]);
        let left = commit("left", &[&base]);
        let right = commit("right", &[&base]);
        let tip = commit("merge", &[&left, &right]);
        git(&["reset", "--hard", &tip]);
        git(&["branch", "feature/HEAD", &left]);
        git(&["update-ref", "refs/remotes/origin/main", &tip]);
        git(&["tag", "release/HEAD", &tip]);
        let mut app = projected_app(&[tip.clone(), left.clone(), right.clone(), base.clone()]);
        for row in &mut app.commits {
            let (results, author, date) = if row.hash == tip {
                (
                    vec![outcome(12, "bad", Verdict::New)],
                    "Dan Raviv",
                    "2026-11-10 22:00",
                )
            } else if row.hash == left {
                (
                    vec![
                        outcome(1, "good", Verdict::Old),
                        outcome(10, "good", Verdict::Old),
                    ],
                    "Alexandria ParticularlyLong Surname",
                    "2026-09-14",
                )
            } else if row.hash == right {
                (
                    vec![outcome(13, "good", Verdict::Old)],
                    "李 王",
                    "2026-09-04 23:34",
                )
            } else {
                (
                    vec![outcome(2, "good", Verdict::Old)],
                    "Nir Averbuch",
                    "2026-07-14 17:08 +0300",
                )
            };
            let Some(Annotation::Bisect(entry)) = &mut row.annotation else {
                unreachable!()
            };
            entry.head = row.hash == tip;
            entry.culprit = row.hash == tip;
            entry.results = results;
            row.author = author.into();
            row.author_date = date.into();
            if row.hash == right {
                row.collaborators.codex = true;
            }
        }
        let top = app.commits.iter().find(|row| row.hash == tip).unwrap();
        assert!(!top
            .decorations
            .split(", ")
            .any(|decoration| decoration == "HEAD" || decoration.starts_with("HEAD -> ")));
        for reference in ["main", "origin/main", "tag: release/HEAD"] {
            assert!(
                top.decorations
                    .split(", ")
                    .any(|decoration| decoration == reference),
                "{}",
                &*top.decorations
            );
        }
        assert!(app
            .commits
            .iter()
            .find(|row| row.hash == left)
            .unwrap()
            .decorations
            .contains("feature/HEAD"));
        assert!(git::load_log(&["-1".into(), tip.clone()]).unwrap()[0]
            .decorations
            .contains("HEAD -> main"));
        assert!(app.log_format.matches(top, None, "culprit", true));
        let mut terminal = Terminal::new(TestBackend::new(220, 24)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let mut aligned = None;
        let mut head_column = None;
        for (screen, source) in app.visible_log_rows.iter().enumerate() {
            let Some(source) = source else { continue };
            let row = &app.commits[*source];
            let y = app.log_row_origin + screen as u16;
            let line: String = (0..220)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect();
            let position = |text: &str| {
                Span::raw(
                    &line[..line
                        .find(text)
                        .unwrap_or_else(|| panic!("missing {text} in {line}"))],
                )
                .width()
            };
            let author = if row.hash == tip {
                "Dan Raviv"
            } else if row.hash == left {
                "Alexandria"
            } else if row.hash == right {
                "李"
            } else {
                "Nir Averbuch"
            };
            let columns = (
                position(&row.short_hash),
                position(&row.author_date),
                position(author),
            );
            if let Some(expected) = aligned {
                assert_eq!(columns, expected, "{line}");
            } else {
                aligned = Some(columns);
            }
            if row.hash == tip {
                assert!(line.contains("HEAD ! #12 bad"), "{line}");
                head_column = Some(position("HEAD !"));
                assert_eq!(
                    terminal.backend().buffer()[(head_column.unwrap() as u16, y)].fg,
                    Color::Cyan
                );
            }
            if row.hash == left {
                assert!(line.contains("# 1 good / #10 good"), "{line}");
            }
        }
        let head_column = head_column.unwrap();
        for (screen, source) in app.visible_log_rows.iter().enumerate() {
            if source.is_some_and(|source| app.commits[source].hash != tip) {
                let y = app.log_row_origin + screen as u16;
                for x in head_column..head_column + 6 {
                    assert_eq!(terminal.backend().buffer()[(x as u16, y)].symbol(), " ");
                }
            }
        }
        // Detached HEAD is omitted too; its tag and branch decorations remain.
        git(&["checkout", "-q", "--detach", &tip]);
        let detached = load_metadata(std::slice::from_ref(&tip), &[]).unwrap();
        assert!(!detached[0]
            .decorations
            .split(", ")
            .any(|reference| reference == "HEAD"));
        assert!(detached[0].decorations.contains("tag: release/HEAD"));
        assert!(detached[0].decorations.contains("main"));
    }
}
