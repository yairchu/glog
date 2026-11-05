//! Structured text matches, grouped by source, folder, and file.
use crate::{
    ansi,
    app::{App, History, Mode},
    git,
};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    process::Stdio,
};

#[derive(Clone, Debug)]
enum Source {
    Worktree,
    Index,
    Revision { label: String, hash: String },
}
impl Source {
    fn label(&self) -> &str {
        match self {
            Self::Worktree => "",
            Self::Index => "Index",
            Self::Revision { label, .. } => label,
        }
    }
}
#[derive(Debug)]
struct Hit {
    line: usize,
    text: String,
}
#[derive(Debug)]
struct File {
    source: usize,
    path: Vec<u8>,
    extension: Vec<u8>,
    hits: Vec<Hit>,
    number_width: usize,
}
#[derive(Clone, Debug)]
enum Kind {
    Source(usize),
    Directory(String),
    File(usize),
    Match { file: usize, hit: usize },
}
#[derive(Debug)]
struct Node {
    kind: Kind,
    parent: Option<usize>,
    children: Vec<usize>,
}
#[derive(Clone, Copy, Debug)]
struct Row {
    node: usize,
    depth: usize,
}
#[derive(Clone, Debug)]
pub enum Action {
    Fold,
    FoldAll,
    HideExtension,
    Reset,
    Extension(Vec<u8>),
}

pub struct View {
    repository: git::Repository,
    sources: Vec<Source>,
    files: Vec<File>,
    nodes: Vec<Node>,
    roots: Vec<usize>,
    rows: Vec<Row>,
    counts: Vec<(usize, usize)>, // visible files and matches beneath each node
    collapsed: HashSet<usize>,
    hidden: BTreeSet<Vec<u8>>,
    pub cursor: usize,
    pub offset: usize,
    pub height: usize,
    pub horizontal: usize,
    path_component: usize,
    path_pan: usize,
    reveal_path: bool,
    origin: u16,
    buttons: Vec<(Rect, Action)>,
    fold_buttons: Vec<(Rect, usize)>,
}

fn resolve(revision: &str, kind: &str) -> Option<String> {
    let output = git::read_command()
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
        .arg(format!("{revision}^{{{kind}}}"))
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Keep matching options and patterns intact, but reserve the output layout
/// for the tree. Pin commit revisions before grep so later ref moves cannot
/// make a result open a different version of its file.
fn arguments(args: &[String]) -> Result<(Vec<String>, Vec<Source>), String> {
    let mut output = Vec::new();
    let mut sources = vec![Source::Worktree];
    let mut pattern = false;
    let mut options = true;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" && (options || pattern) {
            output.push(arg.clone());
            if pattern {
                output.extend(args.cloned());
                break;
            }
            // Before the positional pattern, -- ends options, but Git still
            // accepts revisions and a later -- introducing literal pathspecs.
            options = false;
            continue;
        }
        let mut takes_value = false;
        let mut adds_pattern = false;
        if options && arg.starts_with("--") {
            let name = arg.split('=').next().unwrap();
            match name {
                "--regexp" | "--file" => { adds_pattern = true; takes_value = !arg.contains('='); }
                "--max-depth" | "--max-count" | "--threads" => takes_value = !arg.contains('='),
                "--cached" => sources[0] = Source::Index,
                "--untracked" | "--exclude-standard" | "--no-exclude-standard" |
                "--recursive" | "--no-recursive" | "--ignore-case" | "--word-regexp" |
                "--invert-match" | "--extended-regexp" | "--basic-regexp" | "--perl-regexp" |
                "--fixed-strings" | "--all-match" | "--and" | "--or" | "--not" |
                "--line-number" | "--null" | "--full-name" | "--color" | "--no-color" => {}
                _ => return Err(format!("unsupported grep option {arg:?}; use matching options such as -i, -w, -F, -e, --cached, or --untracked; glog supplies the results layout")),
            }
        } else if options && arg.starts_with('-') && arg != "-" {
            for (offset, flag) in arg[1..].char_indices() {
                match flag {
                    'e' | 'f' | 'm' => {
                        adds_pattern = flag != 'm';
                        takes_value = offset + flag.len_utf8() == arg.len() - 1;
                        break;
                    }
                    'i' | 'w' | 'v' | 'E' | 'G' | 'P' | 'F' | 'I' | 'n' | 'H' | 'z' => {}
                    _ => return Err(format!("unsupported grep option {arg:?}; glog displays matching lines in a file tree")),
                }
            }
        } else if options && (arg == "(" || arg == ")") {
            // Git's compound pattern expressions.
        } else if !pattern {
            pattern = true;
        } else if let Some(hash) = resolve(arg, "commit") {
            output.push(hash.clone());
            sources.push(Source::Revision {
                label: arg.clone(),
                hash,
            });
            continue;
        } else if resolve(arg, "tree").is_some() {
            return Err("grep file navigation requires a commit revision, rather than a standalone tree object".into());
        }
        output.push(arg.clone());
        if takes_value {
            output.push(
                args.next()
                    .ok_or_else(|| format!("{arg} requires a value"))?
                    .clone(),
            );
        }
        pattern |= adds_pattern;
    }
    Ok((output, sources))
}

pub fn load_app(args: &[String]) -> Result<App, String> {
    let repository = git::Repository::main()?;
    let (args, sources) = arguments(args)?;
    let output = git::read_command()
        .args([
            "-c",
            "color.grep.filename=",
            "-c",
            "color.grep.linenumber=",
            "-c",
            "color.grep.separator=",
        ])
        .args([
            "--no-pager",
            "grep",
            "-I",
            "--null",
            "--line-number",
            "--full-name",
            "--no-column",
            "--no-heading",
            "--no-break",
            "--no-recurse-submodules",
            "--color=always",
        ])
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run git grep: {e}"))?;
    if !matches!(output.status.code(), Some(0 | 1)) {
        return Err(git::stderr_message("git grep failed", &output.stderr));
    }
    let view = View::parse(repository.clone(), sources, &output.stdout)?;
    let mut app = App::new(Vec::new());
    app.mode = Mode::Grep;
    app.history = History::Grep;
    app.set_repository(Some(repository));
    app.grep_view = Some(view);
    Ok(app)
}

fn take<'a>(input: &mut &'a [u8], delimiter: u8) -> Result<&'a [u8], String> {
    let end = input
        .iter()
        .position(|byte| *byte == delimiter)
        .ok_or("Malformed Git grep output")?;
    let result = &input[..end];
    *input = &input[end + 1..];
    Ok(result)
}
fn extension(path: &[u8]) -> Vec<u8> {
    let path = git::raw_path(path);
    path.extension()
        .map(|ext| git::path_bytes(std::path::Path::new(ext)).into_owned())
        .unwrap_or_default()
}
fn extension_label(extension: &[u8]) -> String {
    if extension.is_empty() {
        "(no ext)".into()
    } else {
        format!(".{}", crate::diff::display_path(extension))
    }
}

impl View {
    fn parse(
        repository: git::Repository,
        sources: Vec<Source>,
        mut input: &[u8],
    ) -> Result<Self, String> {
        let mut files: Vec<File> = Vec::new();
        let mut indices = HashMap::new();
        while !input.is_empty() {
            let name = take(&mut input, 0)?;
            let number = take(&mut input, 0)?;
            let line = std::str::from_utf8(number)
                .ok()
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|n| *n > 0)
                .ok_or("Malformed Git grep line number")?;
            let text = take(&mut input, b'\n')?;
            let mut source = 0;
            let mut path = name;
            for (index, candidate) in sources.iter().enumerate() {
                if let Source::Revision { hash, .. } = candidate {
                    if let Some(rest) = name
                        .strip_prefix(hash.as_bytes())
                        .and_then(|rest| rest.strip_prefix(b":"))
                    {
                        source = index;
                        path = rest;
                        break;
                    }
                }
            }
            let index = *indices.entry((source, path.to_vec())).or_insert_with(|| {
                files.push(File {
                    source,
                    path: path.to_vec(),
                    extension: extension(path),
                    hits: Vec::new(),
                    number_width: 1,
                });
                files.len() - 1
            });
            let file = &mut files[index];
            file.number_width = file.number_width.max(line.to_string().len());
            file.hits.push(Hit {
                line,
                text: String::from_utf8_lossy(text).into_owned(),
            });
        }
        let mut view = Self {
            repository,
            sources,
            files,
            nodes: Vec::new(),
            roots: Vec::new(),
            rows: Vec::new(),
            counts: Vec::new(),
            collapsed: HashSet::new(),
            hidden: BTreeSet::new(),
            cursor: 0,
            offset: 0,
            height: 0,
            horizontal: 0,
            path_component: 0,
            path_pan: 0,
            reveal_path: true,
            origin: 0,
            buttons: Vec::new(),
            fold_buttons: Vec::new(),
        };
        view.build();
        view.rebuild();
        Ok(view)
    }
    pub fn match_count(&self) -> usize {
        self.files.iter().map(|file| file.hits.len()).sum()
    }
    fn node(&mut self, kind: Kind, parent: Option<usize>) -> usize {
        let index = self.nodes.len();
        self.nodes.push(Node {
            kind,
            parent,
            children: Vec::new(),
        });
        if let Some(parent) = parent {
            self.nodes[parent].children.push(index);
        }
        index
    }
    fn build(&mut self) {
        let mut directories = BTreeMap::new();
        for file in 0..self.files.len() {
            let source = self.files[file].source;
            let root = if let Some(&root) = directories.get(&(source, Vec::new())) {
                root
            } else {
                let root = self.node(Kind::Source(source), None);
                directories.insert((source, Vec::new()), root);
                self.roots.push(root);
                root
            };
            let path = self.files[file].path.clone();
            let mut parent = root;
            let mut prefix = Vec::new();
            let parts: Vec<_> = path.split(|b| *b == b'/').collect();
            for part in &parts[..parts.len().saturating_sub(1)] {
                if !prefix.is_empty() {
                    prefix.push(b'/');
                }
                prefix.extend_from_slice(part);
                parent = if let Some(&index) = directories.get(&(source, prefix.clone())) {
                    index
                } else {
                    let index = self.node(
                        Kind::Directory(crate::diff::display_path(&prefix)),
                        Some(parent),
                    );
                    directories.insert((source, prefix.clone()), index);
                    index
                };
            }
            let header = self.node(Kind::File(file), Some(parent));
            for hit in 0..self.files[file].hits.len() {
                self.node(Kind::Match { file, hit }, Some(header));
            }
        }
    }
    fn visible_rows(&self, expanded: bool) -> Vec<Row> {
        fn visit(view: &View, node: usize, depth: usize, expanded: bool, rows: &mut Vec<Row>) {
            if view.counts[node].1 == 0 {
                return;
            }
            // Expanded paths spend every row on a match. Folder components
            // in the inline path provide directory fold controls.
            if matches!(view.nodes[node].kind, Kind::File(_) | Kind::Directory(_))
                && (expanded || !view.collapsed.contains(&node))
            {
                for child in &view.nodes[node].children {
                    visit(view, *child, depth, expanded, rows);
                }
                return;
            }
            let shown = !matches!(view.nodes[node].kind, Kind::Source(source) if view.sources[source].label().is_empty());
            if shown {
                rows.push(Row { node, depth });
            }
            if expanded || !view.foldable(node) || !view.collapsed.contains(&node) || !shown {
                for child in &view.nodes[node].children {
                    visit(view, *child, depth + usize::from(shown), expanded, rows);
                }
            }
        }
        let mut rows = Vec::new();
        for root in &self.roots {
            visit(self, *root, 0, expanded, &mut rows);
        }
        rows
    }
    fn rebuild(&mut self) {
        self.fold_buttons.clear();
        let selected = self.rows.get(self.cursor).map(|row| row.node);
        self.counts = vec![(0, 0); self.nodes.len()];
        for node in (0..self.nodes.len()).rev() {
            self.counts[node] = match self.nodes[node].kind {
                Kind::Match { file, .. } => (
                    0,
                    usize::from(!self.hidden.contains(&self.files[file].extension)),
                ),
                _ => {
                    let mut count = (0, 0);
                    for child in &self.nodes[node].children {
                        count.0 += self.counts[*child].0;
                        count.1 += self.counts[*child].1;
                    }
                    if matches!(self.nodes[node].kind, Kind::File(_)) && count.1 > 0 {
                        count.0 = 1;
                    }
                    count
                }
            };
        }
        self.rows = self.visible_rows(false);
        let mut selected = selected;
        while let Some(node) = selected {
            if let Some(cursor) = self.rows.iter().position(|row| row.node == node) {
                self.cursor = cursor;
                return;
            }
            // Expanded headers disappear; keep the cursor on the first
            // visible descendant, including any still-collapsed subgroup.
            if matches!(self.nodes[node].kind, Kind::File(_) | Kind::Directory(_)) {
                if let Some(cursor) = self.rows.iter().position(|row| {
                    let mut parent = self.nodes[row.node].parent;
                    while let Some(candidate) = parent {
                        if candidate == node {
                            return true;
                        }
                        parent = self.nodes[candidate].parent;
                    }
                    false
                }) {
                    self.cursor = cursor;
                    return;
                }
            }
            selected = self.nodes[node].parent;
        }
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }
    fn foldable(&self, node: usize) -> bool {
        match self.nodes[node].kind {
            Kind::Match { .. } => false,
            Kind::Source(source) if self.sources[source].label().is_empty() => false,
            _ => {
                // Filters may shrink a collapsed group; keep its way back open.
                self.counts[node].1 > 1
                    || (self.counts[node].1 > 0 && self.collapsed.contains(&node))
            }
        }
    }
    fn selected_extension(&self) -> Option<Vec<u8>> {
        let node = self.rows.get(self.cursor)?.node;
        match self.nodes[node].kind {
            Kind::File(file) | Kind::Match { file, .. } => Some(self.files[file].extension.clone()),
            _ => None,
        }
    }
    pub fn action(&mut self, action: Action) {
        match action {
            Action::Fold => {
                if let Some(row) = self.rows.get(self.cursor) {
                    let node = self.focused_path_node().unwrap_or(row.node);
                    self.toggle_fold(node);
                }
            }
            Action::FoldAll => {
                let groups: Vec<_> = (0..self.nodes.len())
                    .filter(|node| self.foldable(*node))
                    .collect();
                if groups.iter().all(|node| self.collapsed.contains(node)) {
                    self.collapsed.clear();
                } else {
                    self.collapsed.extend(groups);
                }
            }
            Action::HideExtension => {
                if let Some(ext) = self.selected_extension() {
                    self.hidden.insert(ext);
                }
            }
            Action::Reset => self.hidden.clear(),
            Action::Extension(ext) => {
                if !self.hidden.remove(&ext) {
                    self.hidden.insert(ext);
                }
            }
        }
        self.path_component = 0;
        self.path_pan = 0;
        self.reveal_path = true;
        self.rebuild();
    }
    pub fn move_by(&mut self, delta: isize) {
        self.reveal_path = self.path_pan == 0;
        self.cursor = self
            .cursor
            .saturating_add_signed(delta)
            .min(self.rows.len().saturating_sub(1));
    }
    pub fn top(&mut self) {
        self.reveal_path = self.path_pan == 0;
        self.cursor = 0;
        self.offset = 0;
    }
    pub fn bottom(&mut self) {
        self.reveal_path = self.path_pan == 0;
        self.cursor = self.rows.len().saturating_sub(1);
    }
    fn filename(&self, file: usize) -> String {
        crate::diff::display_path(&self.files[file].path)
    }
    fn toggle_fold(&mut self, node: usize) {
        if self.foldable(node) && !self.collapsed.remove(&node) {
            self.collapsed.insert(node);
        }
    }
    fn path_node(&self, row: Row) -> Option<usize> {
        match self.nodes[row.node].kind {
            Kind::Match { .. } => self.nodes[row.node].parent,
            Kind::File(_) | Kind::Directory(_) => Some(row.node),
            _ => None,
        }
    }
    fn path_components(&self, node: usize) -> Vec<(String, usize)> {
        let mut nodes = Vec::new();
        let mut current = Some(node);
        while let Some(node) = current {
            let path = match &self.nodes[node].kind {
                Kind::File(file) => self.filename(*file),
                Kind::Directory(path) => format!("{path}/"),
                _ => break,
            };
            nodes.push((path, node));
            current = self.nodes[node].parent;
        }
        nodes.reverse();
        let mut prefix = 0;
        nodes
            .into_iter()
            .map(|(path, node)| {
                let component = path[prefix..].to_owned();
                prefix = path.len();
                (component, node)
            })
            .collect()
    }
    fn fold_path_components(&self, node: usize) -> Vec<(String, usize)> {
        let mut groups: Vec<(String, usize)> = Vec::new();
        for (component, node) in self.path_components(node) {
            // An ancestor with the same visible findings offers the same fold.
            // Keep the deepest node so a group ending in a filename still opens it.
            if let Some((path, previous)) = groups.last_mut() {
                if self.counts[*previous].1 == self.counts[node].1 {
                    path.push_str(&component);
                    *previous = node;
                    continue;
                }
            }
            groups.push((component, node));
        }
        groups
    }
    fn fold_components(&self, node: usize) -> Vec<usize> {
        self.fold_path_components(node)
            .into_iter()
            .map(|(_, node)| node)
            .filter(|node| self.foldable(*node))
            .collect()
    }
    fn focused_path_node(&self) -> Option<usize> {
        let node = self.path_node(*self.rows.get(self.cursor)?)?;
        let components = self.fold_components(node);
        let last = components.len().checked_sub(1)?;
        Some(components[last - self.path_component.min(last)])
    }
    pub fn folder_focused(&self) -> bool {
        self.focused_path_node()
            .is_some_and(|node| matches!(self.nodes[node].kind, Kind::Directory(_)))
    }
    pub fn move_horizontal(&mut self, right: bool) {
        if !right && self.path_pan > 0 {
            let step = self.path_pan.min(4);
            self.horizontal = self.horizontal.saturating_sub(step);
            self.path_pan -= step;
            return;
        }
        let count = self
            .rows
            .get(self.cursor)
            .and_then(|row| self.path_node(*row))
            .map(|node| self.fold_components(node).len())
            .unwrap_or(0);
        self.path_component = self.path_component.min(count.saturating_sub(1));
        if right && self.path_component == 0 {
            self.horizontal = self.horizontal.saturating_add(4);
            self.path_pan = self.path_pan.saturating_add(4);
        } else if count > 0 {
            self.path_component = if right {
                self.path_component - 1
            } else {
                (self.path_component + 1).min(count - 1)
            };
            self.reveal_path = true;
        } else {
            self.horizontal = self.horizontal.saturating_sub(4);
        }
    }
    fn path_spans(&self, row: Row, marker: &str, style: Style) -> Vec<Span<'static>> {
        let mut spans = vec![Span::styled(marker.to_owned(), style)];
        for (component, _) in self.fold_path_components(self.path_node(row).unwrap()) {
            spans.push(Span::styled(component, style));
        }
        spans
    }
    fn line(&self, row: Row) -> Line<'static> {
        self.line_with_filename(row, false)
    }
    fn line_with_filename(&self, row: Row, force_filename: bool) -> Line<'static> {
        let node = &self.nodes[row.node];
        let (files, matches) = self.counts[row.node];
        let file_count = format!("{files} {}", if files == 1 { "file" } else { "files" });
        let match_count = format!(
            "{matches} {}",
            if matches == 1 { "match" } else { "matches" }
        );
        let marker = if self.foldable(row.node) {
            if self.collapsed.contains(&row.node) {
                "▶ "
            } else {
                "▼ "
            }
        } else {
            "  "
        };
        let mut spans = vec![Span::raw("  ".repeat(row.depth))];
        match &node.kind {
            Kind::Source(source) => spans.push(Span::styled(
                format!(
                    "{marker}{} ({file_count}, {match_count})",
                    self.sources[*source].label()
                ),
                Style::default().fg(Color::Cyan),
            )),
            Kind::Directory(_) => {
                spans.extend(self.path_spans(row, marker, Style::default().fg(Color::Cyan)));
                spans.push(Span::styled(
                    format!(" ({file_count}, {match_count})"),
                    Style::default().fg(Color::Cyan),
                ));
            }
            Kind::File(_) => {
                spans.extend(self.path_spans(row, marker, Style::default().fg(Color::Yellow)));
                spans.push(Span::styled(
                    format!(" ({match_count})"),
                    Style::default().fg(Color::Yellow),
                ));
            }
            Kind::Match { file, hit } => {
                let name = self.filename(*file);
                if *hit == 0 || force_filename {
                    let marker = if self.foldable(self.path_node(row).unwrap()) {
                        "▼ "
                    } else {
                        "  "
                    };
                    spans.extend(self.path_spans(row, marker, Style::default().fg(Color::Yellow)));
                    spans.push(Span::styled(":", Style::default().fg(Color::Yellow)));
                } else {
                    spans.push(Span::raw(" ".repeat(2 + Span::raw(&name).width() + 1)));
                }
                let width = self.files[*file].number_width;
                let hit = &self.files[*file].hits[*hit];
                spans.push(Span::styled(
                    format!("{:>width$}: ", hit.line),
                    Style::default().fg(Color::DarkGray),
                ));
                spans.extend(ansi::normalized_line(&hit.text).spans);
            }
        }
        Line::from(spans)
    }
    pub fn find(&mut self, query: &str, reverse: bool) -> bool {
        let rows = self.visible_rows(true);
        let selected = self.rows.get(self.cursor).map(|row| {
            if matches!(self.nodes[row.node].kind, Kind::File(_)) {
                self.nodes[row.node].children[0]
            } else {
                row.node
            }
        });
        let start = rows
            .iter()
            .position(|row| Some(row.node) == selected)
            .unwrap_or(0);
        for step in 1..=rows.len() {
            let index = if reverse {
                (start + rows.len() - step % rows.len()) % rows.len()
            } else {
                (start + step) % rows.len()
            };
            let row = rows[index];
            if self.line(row).to_string().to_lowercase().contains(query) {
                let mut parent = self.nodes[row.node].parent;
                while let Some(node) = parent {
                    self.collapsed.remove(&node);
                    parent = self.nodes[node].parent;
                }
                self.rebuild();
                self.cursor = self
                    .rows
                    .iter()
                    .position(|candidate| candidate.node == row.node)
                    .unwrap();
                self.path_component = 0;
                self.reveal_path = self.path_pan == 0;
                return true;
            }
        }
        false
    }
    pub fn open(&self) -> Option<Result<App, String>> {
        let node = self.rows.get(self.cursor)?.node;
        let Kind::Match { file, hit } = self.nodes[node].kind else {
            return None;
        };
        let file = &self.files[file];
        let version = match &self.sources[file.source] {
            Source::Revision { hash, .. } => git::Version::Revision(hash.clone()),
            Source::Index => git::Version::Index,
            Source::Worktree => {
                let mut spec = b"HEAD:".to_vec();
                spec.extend_from_slice(&file.path);
                let tracked = git::read_command_at(Some(&self.repository))
                    .args(["cat-file", "-e"])
                    .arg(git::raw_path(&spec))
                    .output()
                    .is_ok_and(|out| out.status.success());
                if tracked {
                    git::Version::Worktree
                } else {
                    git::Version::Untracked
                }
            }
        };
        Some(crate::blame::load_grep_file(
            version,
            &file.path,
            self.repository.clone(),
            file.hits[hit].line,
        ))
    }
    /// Click filters or groups immediately; a second click on a result opens it.
    pub fn click(&mut self, column: u16, row: u16) -> bool {
        self.path_component = 0;
        self.reveal_path = self.path_pan == 0;
        if let Some((_, action)) = self
            .buttons
            .iter()
            .find(|(rect, _)| rect.contains((column, row).into()))
        {
            let action = action.clone();
            self.action(action);
            return false;
        }
        if let Some((_, node)) = self
            .fold_buttons
            .iter()
            .find(|(rect, _)| rect.contains((column, row).into()))
        {
            let node = *node;
            self.cursor = self.offset + usize::from(row - self.origin);
            self.toggle_fold(node);
            self.rebuild();
            return false;
        }
        if row < self.origin || usize::from(row - self.origin) >= self.height {
            return false;
        }
        let index = self.offset + usize::from(row - self.origin);
        if index >= self.rows.len() {
            return false;
        }
        let open = index == self.cursor
            && matches!(self.nodes[self.rows[index].node].kind, Kind::Match { .. });
        self.cursor = index;
        if !matches!(self.nodes[self.rows[index].node].kind, Kind::Match { .. }) {
            self.action(Action::Fold);
        }
        open
    }
    pub fn draw(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        query: Option<&str>,
        current: Option<usize>,
    ) {
        self.buttons.clear();
        self.fold_buttons.clear();
        let mut extensions = BTreeMap::<Vec<u8>, usize>::new();
        for file in &self.files {
            *extensions.entry(file.extension.clone()).or_default() += 1;
        }
        let mut controls = vec![("[T all] ".to_owned(), Action::Reset, false)];
        controls.extend(extensions.into_iter().map(|(ext, count)| {
            (
                format!("[{} {count}] ", extension_label(&ext)),
                Action::Extension(ext.clone()),
                self.hidden.contains(&ext),
            )
        }));
        let mut spans = Vec::new();
        let mut x = 0;
        for (label, action, hidden) in controls {
            let width = Span::raw(&label).width();
            let visible = width.min((area.width as usize).saturating_sub(x));
            if visible > 0 {
                self.buttons.push((
                    Rect::new(area.x + x as u16, area.y, visible as u16, 1),
                    action,
                ));
            }
            spans.push(Span::styled(
                label,
                if hidden {
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::CROSSED_OUT)
                } else {
                    Style::default().fg(Color::Cyan)
                },
            ));
            x += width;
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(area.x, area.y, area.width, area.height.min(1)),
        );
        self.origin = area.y.saturating_add(1);
        self.height = area.height.saturating_sub(1) as usize;
        self.offset = self
            .offset
            .min(self.rows.len().saturating_sub(self.height.max(1)));
        if self.cursor < self.offset {
            self.offset = self.cursor;
        }
        if self.cursor >= self.offset + self.height.max(1) {
            self.offset = self.cursor + 1 - self.height.max(1);
        }
        let rows_area = Rect::new(
            area.x,
            self.origin,
            area.width,
            area.height.saturating_sub(1),
        );
        if self.rows.is_empty() {
            frame.render_widget(
                Paragraph::new("All matches hidden. Press T to clear extension filters."),
                rows_area,
            );
            return;
        }
        if let Some(row) = self.rows.get(self.cursor) {
            if let Some(node) = self.path_node(*row).filter(|_| self.reveal_path) {
                let mut start = row.depth * 2 + 2;
                for (component, node) in self.fold_path_components(node) {
                    let end = start + Span::raw(component).width();
                    if self.focused_path_node() == Some(node) {
                        let width = usize::from(area.width).max(1);
                        if start < self.horizontal || end - start >= width {
                            self.horizontal = start;
                        } else if end > self.horizontal + width {
                            self.horizontal = end - width;
                        }
                        break;
                    }
                    start = end;
                }
            }
        }
        self.reveal_path = false;
        let mut lines = Vec::new();
        for index in self.offset..self.rows.len().min(self.offset + self.height) {
            let row = self.rows[index];
            // Repeat the filename at the viewport edge when its first match
            // has scrolled away, so continuation lines keep their context.
            let force_filename = index == self.offset || index == self.cursor;
            let mut line = self.line_with_filename(row, force_filename);
            let path_node = match self.nodes[row.node].kind {
                Kind::Match { hit, .. } if hit == 0 || force_filename => {
                    self.nodes[row.node].parent
                }
                Kind::File(_) | Kind::Directory(_) => Some(row.node),
                _ => None,
            };
            if let Some(path_node) = path_node {
                let mut start = row.depth * 2;
                for (component_index, (component, node)) in
                    self.fold_path_components(path_node).into_iter().enumerate()
                {
                    let end = start
                        + Span::raw(component).width()
                        + if component_index == 0 { 2 } else { 0 };
                    let left = start.saturating_sub(self.horizontal);
                    let right = end.saturating_sub(self.horizontal).min(area.width as usize);
                    if right > left && self.foldable(node) {
                        self.fold_buttons.push((
                            Rect::new(
                                area.x + left as u16,
                                self.origin + (index - self.offset) as u16,
                                (right - left) as u16,
                                1,
                            ),
                            node,
                        ));
                    }
                    start = end;
                }
            }
            if let Some(query) = query {
                line = crate::ui::highlight_matches(line, query, current == Some(index));
            }
            if index == self.cursor {
                if let Some(node) = self.path_node(row) {
                    // Styling uses UTF-8 byte offsets, including the optional fold marker.
                    let marker = if self.foldable(node) { "▼ " } else { "  " };
                    let mut start = row.depth * 2 + marker.len();
                    for (component, node) in self.fold_path_components(node) {
                        let end = start + component.len();
                        if self.focused_path_node() == Some(node) {
                            line = ansi::style_ranges(
                                line,
                                std::iter::once((
                                    start..end,
                                    Style::default()
                                        .fg(Color::Black)
                                        .bg(Color::Cyan)
                                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                                )),
                            );
                            break;
                        }
                        start = end;
                    }
                }
            }
            if index == self.cursor {
                line.style = Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD);
            }
            line.spans = ansi::display_spans(line.spans);
            lines.push(line);
        }
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .scroll((0, self.horizontal.min(u16::MAX as usize) as u16)),
            rows_area,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        git::tests::{git, CurrentDirGuard, TestDirectory},
        input, ui,
    };
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::{backend::TestBackend, Terminal};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }
    fn key(app: &mut App, code: KeyCode) {
        input::handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), app);
    }
    fn select(view: &mut View, predicate: impl Fn(&Kind) -> bool) {
        view.cursor = view
            .rows
            .iter()
            .position(|row| predicate(&view.nodes[row.node].kind))
            .unwrap();
    }
    fn fixture(root: &std::path::Path) -> View {
        View::parse(git::Repository::checked_out(root.to_owned()), vec![Source::Worktree],
            b"src/a.rs\0\x31\0needle one\nsrc/a.rs\0\x32\0needle two\nsrc/b.md\0\x31\0needle three\ndocs/c.md\0\x31\0needle four\nplain\0\x31\0needle five\n").unwrap()
    }
    fn has_hit(view: &View, file: usize) -> bool {
        view.rows.iter().any(|row| matches!(view.nodes[row.node].kind, Kind::Match { file: found, .. } if found == file))
    }

    #[test]
    fn filtered_single_file_folders_keep_explicit_fold_controls_and_state() {
        let directory = TestDirectory::new();
        let mut view = fixture(directory.path());
        let folder = view
            .nodes
            .iter()
            .position(|node| matches!(&node.kind, Kind::Directory(name) if name == "src"))
            .unwrap();
        view.toggle_fold(folder);
        view.rebuild();
        assert!(!has_hit(&view, 0));
        view.action(Action::Extension(b"md".to_vec()));
        assert!(!has_hit(&view, 0));
        let row = view
            .rows
            .iter()
            .find(
                |row| matches!(&view.nodes[row.node].kind, Kind::Directory(name) if name == "src"),
            )
            .unwrap();
        assert!(view.foldable(row.node));
        assert!(view.line(*row).to_string().contains("▶ src/"));
        view.action(Action::Reset);
        assert!(
            !has_hit(&view, 0),
            "restoring filters preserves the folder's fold choice"
        );
        view.action(Action::FoldAll);
        view.action(Action::FoldAll);
        assert!(has_hit(&view, 0));
        select(&mut view, |kind| {
            matches!(kind, Kind::Match { file: 0, .. })
        });
        view.action(Action::Fold);
        assert!(matches!(
            view.nodes[view.rows[view.cursor].node].kind,
            Kind::File(0)
        ));
        assert!(!has_hit(&view, 0));
        view.action(Action::HideExtension);
        assert!(view.hidden.contains(b"rs".as_slice()));
        view.action(Action::Reset);
        assert!(
            !has_hit(&view, 0),
            "filter reset does not expand folded files"
        );
        assert!(view.find("needle two", false));
        assert!(matches!(
            view.nodes[view.rows[view.cursor].node].kind,
            Kind::Match { file: 0, hit: 1 }
        ));
        assert!(view.find("needle one", true));
    }

    #[test]
    fn extension_buttons_and_empty_filters_use_visible_unicode_hit_regions() {
        let directory = TestDirectory::new();
        let mut view = View::parse(
            git::Repository::checked_out(directory.path().to_owned()),
            vec![Source::Worktree],
            "dir/first.界\0\x31\0needle\ndir/second.rs\0\x31\0needle\nplain\0\x31\0needle\n"
                .as_bytes(),
        )
        .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        terminal
            .draw(|frame| view.draw(frame, frame.area(), None, None))
            .unwrap();
        let (button, _) = view
            .buttons
            .iter()
            .find(|(_, action)| matches!(action, Action::Extension(ext) if ext == "界".as_bytes()))
            .unwrap()
            .clone();
        assert!(!view.click(button.x, button.y));
        assert!(view.hidden.contains("界".as_bytes()));
        for ext in [b"rs".to_vec(), Vec::new()] {
            view.action(Action::Extension(ext));
        }
        assert!(view.rows.is_empty());
        terminal
            .draw(|frame| view.draw(frame, frame.area(), None, None))
            .unwrap();
        assert!(terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .contains("All matches hidden"));
        let reset = view.buttons[0].0;
        view.click(reset.x, reset.y);
        assert_eq!(view.hidden.len(), 0);
        assert!(!view.rows.is_empty());
    }

    #[test]
    fn match_navigation_preserves_sources_paths_and_results_on_return() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        std::fs::create_dir("src").unwrap();
        std::fs::write("src/a.rs", "needle committed\nsame\nneedle initial\n").unwrap();
        std::fs::write("src/b.md", "needle notes\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        let original = git(&["rev-parse", "HEAD"]);
        std::fs::write("src/a.rs", "needle staged\nsame\nneedle staged second\n").unwrap();
        git(&["add", "src/a.rs"]);
        std::fs::write("src/a.rs", "needle working\nsame\nneedle working second\n").unwrap();
        std::fs::write("loose.rs", "needle untracked\n").unwrap();
        for (options, expected) in [
            (vec!["needle"], "needle working"),
            (vec!["--cached", "needle"], "needle staged"),
            (vec!["needle", "HEAD"], "needle committed"),
            (
                vec!["--untracked", "needle", "--", "loose.rs"],
                "needle untracked",
            ),
        ] {
            let mut app = load_app(&args(&options)).unwrap();
            let view = app.grep_view.as_mut().unwrap();
            let node = view
                .rows
                .iter()
                .position(|row| match view.nodes[row.node].kind {
                    Kind::Match { file, hit } => {
                        ansi::plain(&view.files[file].hits[hit].text) == expected
                    }
                    _ => false,
                })
                .unwrap();
            view.cursor = node;
            let rows = view.rows.iter().map(|row| row.node).collect::<Vec<_>>();
            key(&mut app, KeyCode::Enter);
            assert_eq!(app.history, History::Blame, "{:?}", app.status);
            let line = app.commits[app.selected].blame().unwrap();
            assert_eq!(
                line.code
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>(),
                expected
            );
            key(&mut app, KeyCode::Backspace);
            assert_eq!(app.mode, Mode::Grep);
            let view = app.grep_view.as_ref().unwrap();
            assert_eq!(view.cursor, node);
            assert_eq!(
                view.rows.iter().map(|row| row.node).collect::<Vec<_>>(),
                rows
            );
        }
        let mut historical = load_app(&args(&["needle", "HEAD"])).unwrap();
        git(&["add", "src/a.rs"]);
        git(&["commit", "-qm", "advance"]);
        let view = historical.grep_view.as_mut().unwrap();
        select(view, |kind| matches!(kind, Kind::Match { file: 0, hit: 0 }));
        historical.open_grep_match();
        assert_eq!(historical.commits[historical.selected].hash, original);
        std::env::set_current_dir(directory.path().join("src")).unwrap();
        let nested = load_app(&args(&["needle", "--", "a.rs"])).unwrap();
        assert_eq!(nested.grep_view.unwrap().files[0].path, b"src/a.rs");
    }
    #[test]
    fn revisions_after_an_option_terminator_open_the_pinned_file() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        for pattern in ["needle", "--cached"] {
            let before = format!("{pattern} before");
            std::fs::write("file.rs", format!("{before}\n")).unwrap();
            git(&["add", "."]);
            git(&["commit", "-qm", "before"]);
            let original = git(&["rev-parse", "HEAD"]);
            let mut app = load_app(&args(&["--", pattern, "HEAD", "--", "file.rs"])).unwrap();
            assert_eq!(app.grep_view.as_ref().unwrap().match_count(), 1);
            select(app.grep_view.as_mut().unwrap(), |kind| {
                matches!(kind, Kind::Match { .. })
            });
            std::fs::write("file.rs", format!("{pattern} after\n")).unwrap();
            git(&["add", "."]);
            git(&["commit", "-qm", "after"]);
            app.open_grep_match();
            assert_eq!(app.history, History::Blame, "{:?}", app.status);
            assert_eq!(app.commits[app.selected].hash, original);
            assert_eq!(
                app.commits[app.selected]
                    .blame()
                    .unwrap()
                    .code
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>(),
                before
            );
            key(&mut app, KeyCode::Backspace);
            assert_eq!(app.mode, Mode::Grep);
        }
    }

    fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
        input::handle(
            Event::Mouse(MouseEvent {
                kind,
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            }),
            app,
        );
    }

    #[test]
    fn keyboard_mouse_search_and_filters_share_the_same_tree_state() {
        let directory = TestDirectory::new();
        let mut app = App::new(Vec::new());
        app.mode = Mode::Grep;
        app.history = History::Grep;
        app.grep_view = Some(fixture(directory.path()));
        let mut terminal = Terminal::new(TestBackend::new(80, 7)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        for _ in 0..4 {
            mouse(&mut app, MouseEventKind::ScrollDown, 0, 3);
        }
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert!(app.grep_view.as_ref().unwrap().offset > 0);
        key(&mut app, KeyCode::Home);
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert_eq!(app.grep_view.as_ref().unwrap().offset, 0);
        key(&mut app, KeyCode::Char('m'));
        key(&mut app, KeyCode::Char('/'));
        for c in "needle two".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        key(&mut app, KeyCode::Enter);
        let view = app.grep_view.as_ref().unwrap();
        assert!(matches!(
            view.nodes[view.rows[view.cursor].node].kind,
            Kind::Match { file: 0, hit: 1 }
        ));
        key(&mut app, KeyCode::Char('z'));
        assert!(!has_hit(app.grep_view.as_ref().unwrap(), 0));
        key(&mut app, KeyCode::Char('t'));
        assert!(app
            .grep_view
            .as_ref()
            .unwrap()
            .hidden
            .contains(b"rs".as_slice()));
        key(&mut app, KeyCode::Char('T'));
        assert!(app.grep_view.as_ref().unwrap().hidden.is_empty());
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let (button, _) = app
            .grep_view
            .as_ref()
            .unwrap()
            .buttons
            .iter()
            .find(|(_, action)| matches!(action, Action::Extension(ext) if ext == b"md"))
            .unwrap()
            .clone();
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            button.x,
            button.y,
        );
        assert!(app
            .grep_view
            .as_ref()
            .unwrap()
            .hidden
            .contains(b"md".as_slice()));
        key(&mut app, KeyCode::Char('h'));
        let cursor = app.grep_view.as_ref().unwrap().cursor;
        mouse(&mut app, MouseEventKind::ScrollDown, 0, 3);
        assert_eq!(app.grep_view.as_ref().unwrap().cursor, cursor);
        key(&mut app, KeyCode::Esc);
        assert!(!app.show_help);
        key(&mut app, KeyCode::Esc);
        assert!(!app.quit, "Esc is not a quit key");
        key(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
    }

    #[test]
    fn matching_options_literal_flags_binary_files_and_git_errors_are_kept_distinct() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        std::fs::write("file.rs", "Needle\n--no-color-moved\n--cached").unwrap();
        std::fs::write("binary", b"Needle\0binary").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        git(&["config", "color.grep.filename", "red bold"]);
        git(&["config", "color.grep.linenumber", "green"]);
        git(&["config", "grep.column", "true"]);
        let load = |values: &[&str]| load_app(&args(values)).map(|app| app.grep_view.unwrap());
        assert_eq!(load(&["-i", "needle"]).unwrap().match_count(), 1);
        assert_eq!(load(&["-e", "--no-color-moved"]).unwrap().match_count(), 1);
        assert!(matches!(
            load(&["-e", "--cached"]).unwrap().sources[0],
            Source::Worktree
        ));
        let plain = load(&["--no-color", "-F", "Needle"]).unwrap();
        assert_eq!(plain.files[0].path, b"file.rs");
        assert_eq!(plain.files[0].hits[0].text, "Needle");
        assert_eq!(load(&["absent"]).unwrap().match_count(), 0);
        assert!(load(&["-E", "("]).is_err());
        assert!(load(&["--count", "Needle"]).is_err());
        assert!(load(&["-e"]).is_err());
        assert!(load(&["Needle", "HEAD^{tree}"]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn nul_delimited_paths_keep_colons_newlines_and_non_utf8_bytes() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        // Parsing must preserve arbitrary bytes even on filesystems that
        // only permit UTF-8 names (such as this test's macOS filesystem).
        let raw = b"odd:\n\xff.rs";
        let mut record = raw.to_vec();
        record.extend_from_slice(b"\0\x31\0needle raw path\n");
        let parsed = View::parse(
            git::Repository::checked_out(directory.path().to_owned()),
            vec![Source::Worktree],
            &record,
        )
        .unwrap();
        assert_eq!(parsed.files[0].path, raw);
        let path = "odd:\n界.rs".as_bytes();
        std::fs::write(git::raw_path(path), "needle raw path\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "odd path"]);
        let mut app = load_app(&args(&["needle"])).unwrap();
        assert_eq!(app.grep_view.as_ref().unwrap().files[0].path, path);
        select(app.grep_view.as_mut().unwrap(), |kind| {
            matches!(kind, Kind::Match { .. })
        });
        app.open_grep_match();
        assert_eq!(app.history, History::Blame, "{:?}", app.status);
        assert_eq!(app.commits[app.selected].blame().unwrap().source_path, path);
    }
    #[test]
    fn compact_matches_align_unicode_names_without_folding_single_findings() {
        let directory = TestDirectory::new();
        let mut view = View::parse(git::Repository::checked_out(directory.path().to_owned()), vec![Source::Worktree],
            "src/界.rs\0\x31\0needle one\nsrc/界.rs\0\x31\x32\0needle two\nsingle/a.md\0\x31\0needle solo\n".as_bytes()).unwrap();
        // Only matching lines: neither folders nor files spend header rows.
        assert_eq!(view.rows.len(), 3);
        assert!(!view
            .rows
            .iter()
            .any(|row| matches!(view.nodes[row.node].kind, Kind::File(_))));
        let text: Vec<_> = view
            .rows
            .iter()
            .map(|row| view.line(*row).to_string())
            .collect();
        let first = text
            .iter()
            .find(|text| text.contains("needle one"))
            .unwrap();
        let second = text
            .iter()
            .find(|text| text.contains("needle two"))
            .unwrap();
        assert!(first.contains("▼ src/界.rs: 1:"), "{first}");
        assert!(!second.contains("界.rs"));
        assert_eq!(
            Span::raw(&first[..first.find("needle").unwrap()]).width(),
            Span::raw(&second[..second.find("needle").unwrap()]).width()
        );
        select(&mut view, |kind| {
            matches!(kind, Kind::Match { file: 1, .. })
        });
        view.action(Action::Fold);
        assert!(has_hit(&view, 1), "one finding needs no file fold");
        assert!(
            view.focused_path_node().is_none(),
            "single-finding ancestors need no cursor"
        );
    }

    #[test]
    fn path_cursor_skips_single_findings_and_updates_after_filtering() {
        let directory = TestDirectory::new();
        let mut view = View::parse(
            git::Repository::checked_out(directory.path().to_owned()), vec![Source::Worktree],
            b"src/deep/a.rs\0\x31\0needle one\nsrc/b.md\0\x31\0needle two\nplain\0\x31\0needle three\n",
        ).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        terminal
            .draw(|frame| view.draw(frame, frame.area(), None, None))
            .unwrap();
        let focused = view.focused_path_node().unwrap();
        assert!(matches!(&view.nodes[focused].kind, Kind::Directory(path) if path == "src"));
        let highlighted: String = (0..80)
            .filter_map(|x| {
                let cell = &terminal.backend().buffer()[(x, view.origin)];
                (cell.bg == Color::Cyan).then(|| cell.symbol())
            })
            .collect();
        assert_eq!(highlighted, "src/");
        assert!(view.fold_buttons.iter().all(|(_, node)| *node == focused));
        view.move_horizontal(false);
        assert_eq!(view.focused_path_node(), Some(focused));
        view.move_horizontal(true); // The folder is the last useful target: pan immediately.
        assert_eq!(view.horizontal, 4);
        view.action(Action::Extension(b"md".to_vec()));
        assert!(view.focused_path_node().is_none());
        terminal
            .draw(|frame| view.draw(frame, frame.area(), None, None))
            .unwrap();
        assert!(view.fold_buttons.is_empty());
        assert!(!(0..80).any(|x| terminal.backend().buffer()[(x, view.origin)].bg == Color::Cyan));
        view.action(Action::Fold);
        assert!(has_hit(&view, 0));
        select(&mut view, |kind| {
            matches!(kind, Kind::Match { file: 2, .. })
        });
        assert!(view.focused_path_node().is_none());
    }

    #[test]
    fn equivalent_folder_and_file_folds_share_one_cursor_and_mouse_target() {
        let directory = TestDirectory::new();
        let mut view = View::parse(
            git::Repository::checked_out(directory.path().to_owned()), vec![Source::Worktree],
            b"docs/nested/index.html\0\x31\0reflog one\ndocs/nested/index.html\0\x32\0reflog two\ndocs/notes.md\0\x31\0reflog notes\n",
        ).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let file = view.path_node(view.rows[0]).unwrap();
        assert_eq!(
            view.fold_components(file).len(),
            2,
            "docs has an additional matching file"
        );
        assert_eq!(view.fold_path_components(file)[1].0, "nested/index.html");
        view.action(Action::Extension(b"md".to_vec()));
        assert_eq!(view.fold_components(file), vec![file]);
        assert_eq!(
            view.fold_path_components(file)[0].0,
            "docs/nested/index.html"
        );
        terminal
            .draw(|frame| view.draw(frame, frame.area(), None, None))
            .unwrap();
        let highlighted: String = (0..80)
            .filter_map(|x| {
                let cell = &terminal.backend().buffer()[(x, view.origin)];
                (cell.bg == Color::Cyan).then(|| cell.symbol())
            })
            .collect();
        assert_eq!(highlighted, "docs/nested/index.html");
        assert!(
            !view.folder_focused(),
            "Enter must still open the match for a group ending in a file"
        );
        view.move_horizontal(false);
        assert_eq!(view.focused_path_node(), Some(file));
        view.move_horizontal(true);
        assert_eq!(
            view.horizontal, 4,
            "there is no redundant folder stop before panning"
        );
        view.move_horizontal(false);
        view.action(Action::Fold);
        assert!(matches!(
            view.nodes[view.rows[view.cursor].node].kind,
            Kind::File(0)
        ));
        view.action(Action::Fold);
        terminal
            .draw(|frame| view.draw(frame, frame.area(), None, None))
            .unwrap();
        let (button, target) = view.fold_buttons[0];
        assert_eq!(target, file);
        view.click(button.x + 2, button.y); // Clicking docs folds the same group as the filename.
        assert!(matches!(
            view.nodes[view.rows[view.cursor].node].kind,
            Kind::File(0)
        ));
        view.action(Action::Fold);
        view.action(Action::Reset);
        assert_eq!(
            view.fold_components(file).len(),
            2,
            "restoring sibling findings splits the targets again"
        );
    }

    #[test]
    fn visible_path_cursor_selects_folders_and_pans_beyond_the_filename() {
        let directory = TestDirectory::new();
        let mut app = App::new(Vec::new());
        app.mode = Mode::Grep;
        app.history = History::Grep;
        app.grep_view = Some(View::parse(
            git::Repository::checked_out(directory.path().to_owned()), vec![Source::Worktree],
            "界/sub/a.rs\0\x31\0needle one\n界/sub/a.rs\0\x32\0needle two\n界/sub/b.rs\0\x31\0needle sibling\n界/other.rs\0\x31\0needle three\n".as_bytes(),
        ).unwrap());
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        key(&mut app, KeyCode::Down); // The selected continuation gains a path cursor too.
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let view = app.grep_view.as_ref().unwrap();
        let y = view.origin + 1;
        let highlighted: String = (0..80)
            .filter_map(|x| {
                let cell = &terminal.backend().buffer()[(x, y)];
                (cell.bg == Color::Cyan).then(|| cell.symbol())
            })
            .collect();
        assert_eq!(highlighted, "a.rs");
        for _ in 0..2 {
            key(&mut app, KeyCode::Right);
        }
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert_eq!(
            app.grep_view.as_ref().unwrap().horizontal,
            8,
            "drawing must not undo manual panning"
        );
        key(&mut app, KeyCode::Up);
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert_eq!(app.grep_view.as_ref().unwrap().horizontal, 8);
        key(&mut app, KeyCode::Down);
        for expected in [4, 0] {
            key(&mut app, KeyCode::Left);
            assert_eq!(app.grep_view.as_ref().unwrap().horizontal, expected);
            assert!(!app.grep_view.as_ref().unwrap().folder_focused());
        }
        key(&mut app, KeyCode::Left);
        assert!(app.grep_view.as_ref().unwrap().folder_focused());
        key(&mut app, KeyCode::Right);
        assert!(!app.grep_view.as_ref().unwrap().folder_focused());
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.history, History::Grep);
        let view = app.grep_view.as_ref().unwrap();
        assert!(!has_hit(view, 0) && !has_hit(view, 1) && has_hit(view, 2));
        assert!(view
            .line(view.rows[view.cursor])
            .to_string()
            .contains("▶ 界/sub/"));
        key(&mut app, KeyCode::Char('z')); // Unfold the selected folder.
        assert!(has_hit(app.grep_view.as_ref().unwrap(), 0));
        key(&mut app, KeyCode::Char('z')); // Default cursor is back on the filename.
        let view = app.grep_view.as_ref().unwrap();
        assert!(matches!(
            view.nodes[view.rows[view.cursor].node].kind,
            Kind::File(0)
        ));
        key(&mut app, KeyCode::Char('z'));
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
    }

    #[test]
    fn focused_unicode_components_remain_visible_and_override_search_highlights() {
        let directory = TestDirectory::new();
        let mut view = View::parse(
            git::Repository::checked_out(directory.path().to_owned()),
            vec![Source::Worktree],
            "界/very-long-directory/a.rs\0\x31\0needle\n界/very-long-directory/a.rs\0\x32\0needle two\n界/very-long-directory/b.rs\0\x31\0needle sibling\n界/other.rs\0\x31\0needle other\n".as_bytes(),
        )
        .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(12, 5)).unwrap();
        terminal
            .draw(|frame| view.draw(frame, frame.area(), Some("a.rs"), Some(0)))
            .unwrap();
        assert!(view.horizontal > 0);
        let highlighted: String = (0..12)
            .filter_map(|x| {
                let cell = &terminal.backend().buffer()[(x, view.origin)];
                (cell.bg == Color::Cyan).then(|| cell.symbol())
            })
            .collect();
        assert_eq!(highlighted, "a.rs");
        let baseline = view.horizontal;
        view.move_horizontal(true);
        terminal
            .draw(|frame| view.draw(frame, frame.area(), None, None))
            .unwrap();
        assert_eq!(view.horizontal, baseline + 4);
        view.move_horizontal(false);
        assert_eq!(view.horizontal, baseline);
        view.move_horizontal(false);
        view.move_horizontal(false);
        view.move_horizontal(false); // Clamp at the first component.
        terminal
            .draw(|frame| view.draw(frame, frame.area(), None, None))
            .unwrap();
        assert_eq!(view.horizontal, 2);
        assert_eq!(terminal.backend().buffer()[(0, view.origin)].symbol(), "界");
        assert_eq!(
            terminal.backend().buffer()[(0, view.origin)].bg,
            Color::Cyan
        );
    }

    #[test]
    fn inline_nested_directories_fold_independently_and_restore_selection() {
        let directory = TestDirectory::new();
        let mut view = View::parse(
            git::Repository::checked_out(directory.path().to_owned()), vec![Source::Worktree],
            "界/sub/a.rs\0\x31\0needle one\n界/sub/b.rs\0\x31\0needle two\n界/other.rs\0\x31\0needle three\n".as_bytes(),
        ).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        assert_eq!(view.rows.len(), 3);
        assert!(view
            .line(view.rows[0])
            .to_string()
            .contains("界/sub/a.rs:1:"));
        terminal
            .draw(|frame| view.draw(frame, frame.area(), None, None))
            .unwrap();
        let (button, folder) = *view.fold_buttons.iter().find(|(_, node)| matches!(&view.nodes[*node].kind, Kind::Directory(path) if path == "界/sub")).unwrap();
        view.click(button.right() - 1, button.y);
        assert!(!has_hit(&view, 0) && !has_hit(&view, 1));
        assert!(has_hit(&view, 2));
        assert_eq!(view.rows[view.cursor].node, folder);
        assert!(view
            .line(view.rows[view.cursor])
            .to_string()
            .contains("▶ 界/sub/"));
        terminal
            .draw(|frame| view.draw(frame, frame.area(), None, None))
            .unwrap();
        let (button, _) = *view
            .fold_buttons
            .iter()
            .find(|(_, node)| *node == folder)
            .unwrap();
        view.click(button.x, button.y);
        assert!(matches!(
            view.nodes[view.rows[view.cursor].node].kind,
            Kind::Match { file: 0, hit: 0 }
        ));
        assert!(has_hit(&view, 1));
        // The outer path component folds both the nested folder and its sibling.
        terminal
            .draw(|frame| view.draw(frame, frame.area(), None, None))
            .unwrap();
        let (button, _) = *view.fold_buttons.iter().find(|(_, node)| matches!(&view.nodes[*node].kind, Kind::Directory(path) if path == "界")).unwrap();
        view.click(button.x, button.y);
        assert_eq!(view.rows.len(), 1);
        assert!(view.find("界/sub/b.rs", false));
        assert!(matches!(
            view.nodes[view.rows[view.cursor].node].kind,
            Kind::Match { file: 1, hit: 0 }
        ));
    }

    #[test]
    fn viewport_filenames_keep_fold_and_result_clicks_distinct_when_panned() {
        let directory = TestDirectory::new();
        let records = (1..=6)
            .map(|line| format!("界.rs\0{line}\0needle {line}\n"))
            .collect::<String>();
        let mut view = View::parse(
            git::Repository::checked_out(directory.path().to_owned()),
            vec![Source::Worktree],
            records.as_bytes(),
        )
        .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(70, 8)).unwrap();
        let area = Rect::new(3, 1, 60, 3);
        view.cursor = 1;
        view.offset = 1;
        view.horizontal = 1;
        view.reveal_path = false;
        terminal
            .draw(|frame| view.draw(frame, area, None, None))
            .unwrap();
        let y = view.origin;
        let text: String = (0..70)
            .map(|x| terminal.backend().buffer()[(x, y)].symbol())
            .collect();
        // The wide glyph's second terminal cell is blank in TestBackend.
        assert!(
            text.contains('界') && text.contains(".rs:2: needle 2"),
            "{text}"
        );
        let button = view.fold_buttons[0].0;
        assert!(matches!(
            view.nodes[view.fold_buttons[0].1].kind,
            Kind::File(0)
        ));
        assert!(!view.click(button.right() - 1, button.y));
        assert!(!has_hit(&view, 0));
        terminal
            .draw(|frame| view.draw(frame, area, None, None))
            .unwrap();
        assert!(!view.click(area.x + 15, view.origin)); // Expand the collapsed header.
        terminal
            .draw(|frame| view.draw(frame, area, None, None))
            .unwrap();
        assert_eq!(view.cursor, 0);
        let button = view.fold_buttons[0].0;
        assert!(view.click(button.right() + 3, button.y)); // First match's code opens it.
        assert!(has_hit(&view, 0));
    }

    #[test]
    fn search_from_a_collapsed_group_starts_at_its_position() {
        let directory = TestDirectory::new();
        let mut view = View::parse(
            git::Repository::checked_out(directory.path().to_owned()),
            vec![Source::Worktree],
            b"a.rs\x001\x00alpha\nsrc/x.rs\x001\x00alpha\nsrc/x.rs\x002\x00beta\nsrc/y.rs\x001\x00beta\nz.rs\x001\x00alpha\n",
        )
        .unwrap();
        let found = |view: &View| match view.nodes[view.rows[view.cursor].node].kind {
            Kind::Match { file, hit } => (file, hit),
            ref kind => panic!("{kind:?}"),
        };
        let folder = view
            .nodes
            .iter()
            .position(|node| matches!(&node.kind, Kind::Directory(name) if name == "src"))
            .unwrap();
        view.toggle_fold(folder);
        view.rebuild();
        select(&mut view, |kind| matches!(kind, Kind::Directory(_)));
        assert!(view.find("alpha", true));
        assert_eq!(found(&view), (0, 0), "backward from the folder");
        view.collapsed.clear();
        let file = view
            .nodes
            .iter()
            .position(|node| matches!(node.kind, Kind::File(1)))
            .unwrap();
        view.toggle_fold(file);
        view.rebuild();
        select(&mut view, |kind| matches!(kind, Kind::File(1)));
        assert!(view.find("alpha", false));
        assert_eq!(found(&view), (1, 0), "forward into the file");
    }

    #[test]
    fn revisions_naming_the_same_commit_share_one_source() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        std::fs::write("file.rs", "needle\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        git(&["branch", "other"]);
        let app = load_app(&args(&["needle", "HEAD", "other"])).unwrap();
        let view = app.grep_view.unwrap();
        assert_eq!(view.match_count(), 1);
        let labels: Vec<_> = view
            .roots
            .iter()
            .map(|root| match view.nodes[*root].kind {
                Kind::Source(source) => view.sources[source].label().to_owned(),
                ref kind => panic!("{kind:?}"),
            })
            .collect();
        assert_eq!(labels, ["HEAD, other"]);
    }
}
