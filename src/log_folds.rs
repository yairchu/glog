//! Merge folds keep the full history available for search and patch loading.
//! Only the visible graph and navigation are projected onto the unfolded commits.
//!
//! A row is shown when an unfolded path from a branch tip reaches it: a
//! folded merge follows only its first parent. Everything the Log shows about
//! folds (visibility, markers, counts, the redrawn graph) comes from one
//! `Layout`, recomputed whenever the commits, their ancestry or the folds
//! change, so these can never disagree.
use std::collections::{HashMap, HashSet};

use crate::git::{self, Commit, CommitKind};

type Ancestry = HashMap<String, Vec<String>>;
type AncestrySource = Box<dyn Fn(&[Commit]) -> Result<Ancestry, String>>;

pub struct LogFolds {
    pub start_collapsed: bool,
    pub ascii_graph: bool,
    load_ancestry: AncestrySource,
    collapsed: HashSet<String>,
    // Merges already offered to --fold-merges.
    seen: HashSet<String>,
    // Ancestry through omitted commits stays valid while the loaded
    // revisions and their parents are unchanged.
    revisions: Vec<(String, Vec<String>, bool)>,
    ancestry: Option<Ancestry>,
    graph: Graph,
    // History omitted by Git, read at most once per set of revisions and
    // shared by folding and the Unicode graph.
    omitted: Option<Ancestry>,
    // Complete ancestry for drawing through Git-omitted rows, without changing
    // which merges the existing folding policy offers.
    unicode_graph: Option<Graph>,
    // Drawing falls back to Git's graph without retrying a failed read.
    // Folding still retries, as each fold is an explicit request.
    unicode_failed: bool,
    layout: Layout,
    // A failed ancestry read keeps fold choices for the next successful
    // refresh, but shows the Log unfolded.
    failed: bool,
    pub hidden_types: std::collections::BTreeSet<String>,
    filtered: Option<Layout>,
    pub hidden_count: usize,
    pub hide_merges: bool,
}

impl Default for LogFolds {
    fn default() -> Self {
        Self {
            start_collapsed: false,
            ascii_graph: false,
            load_ancestry: Box::new(git::log_ancestry),
            collapsed: HashSet::new(),
            seen: HashSet::new(),
            revisions: Vec::new(),
            ancestry: None,
            graph: Graph::default(),
            omitted: None,
            unicode_graph: None,
            unicode_failed: false,
            layout: Layout::default(),
            failed: false,
            hidden_types: Default::default(),
            filtered: None,
            hidden_count: 0,
            hide_merges: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fold {
    None,
    // Folding this merge would hide at least one commit.
    Foldable,
    // Folded, hiding this many commits, including those of nested folds.
    Folded(usize),
}

impl LogFolds {
    pub fn visible(&self, index: usize) -> bool {
        self.filtered
            .as_ref()
            .unwrap_or(&self.layout)
            .visible
            .get(index)
            .copied()
            .unwrap_or(true)
    }

    /// Graph-only merge rows participate in layout, but not selection/search.
    pub fn graph_visible(&self, index: usize) -> bool {
        self.visible(index)
            || self.filtered.as_ref().is_some_and(|layout| {
                let unicode = self.unicode_shown();
                layout
                    .graphs
                    .height(unicode, index)
                    .is_some_and(|rows| rows > 0)
            })
    }

    /// A row that merge folds show but filters hide. Its outgoing edges are
    /// drawn in the routing rows of the next shown commit.
    pub fn filtered_out(&self, index: usize) -> bool {
        self.filtered.as_ref().is_some_and(|filtered| {
            self.layout.visible.get(index) == Some(&true)
                && filtered.visible.get(index) == Some(&false)
        })
    }

    /// The ASCII graph, whichever style the Log shows.
    #[cfg(test)]
    pub fn graph<'a>(&'a self, index: usize, commit: &'a Commit) -> &'a [String] {
        self.drawn(false, index).unwrap_or(&commit.graph)
    }

    pub fn display_graph<'a>(&'a self, index: usize, commit: &'a Commit) -> &'a [String] {
        self.drawn(self.unicode_shown(), index)
            .unwrap_or(&commit.graph)
    }

    /// The number of rows `display_graph` returns, without drawing them.
    pub fn display_height(&self, index: usize, commit: &Commit) -> usize {
        let unicode = self.unicode_shown();
        self.drawing(unicode)
            .and_then(|drawing| drawing.height(unicode, index))
            .unwrap_or(commit.graph.len())
    }

    fn unicode_shown(&self) -> bool {
        // Keep Git's connectivity when its omitted ancestry is unavailable.
        !(self.ascii_graph || self.graph.unknown_below.is_some() && self.unicode_graph.is_none())
    }

    /// The redrawn graph, or `None` where Git's graph is shown unchanged.
    fn drawing(&self, unicode: bool) -> Option<&Drawing> {
        let layout = self.filtered.as_ref().unwrap_or(&self.layout);
        if unicode {
            Some(layout.unicode.as_ref().unwrap_or(&layout.graphs))
        } else {
            (!layout.git_ascii).then_some(&layout.graphs)
        }
    }

    fn drawn(&self, unicode: bool, index: usize) -> Option<&[String]> {
        self.drawing(unicode)?.rows(unicode, index)
    }

    fn fold(&self, index: usize) -> Fold {
        self.layout.folds.get(index).copied().unwrap_or(Fold::None)
    }

    pub fn marker(&self, index: usize) -> Option<&'static str> {
        match self.fold(index) {
            Fold::None => None,
            Fold::Foldable => Some("▼"),
            Fold::Folded(_) => Some("▶"),
        }
    }

    pub fn label(&self, index: usize) -> String {
        match self.fold(index) {
            Fold::Folded(count) => format!(
                " · {count} merged commit{}",
                if count == 1 { "" } else { "s" }
            ),
            _ => String::new(),
        }
    }

    pub fn refresh(&mut self, commits: &[Commit]) -> Result<(), String> {
        let revisions: Vec<_> = commits
            .iter()
            .filter(|c| c.kind == CommitKind::Revision)
            .map(|c| (c.hash.clone(), c.parents.clone(), c.first_parent_only))
            .collect();
        if revisions != self.revisions {
            self.ancestry = None;
            self.omitted = None;
            self.unicode_graph = None;
            self.unicode_failed = false;
            self.revisions = revisions;
        }
        self.graph = Graph::new(commits, self.ancestry.as_ref());
        let hashes: HashSet<_> = commits.iter().map(|c| c.hash.as_str()).collect();
        self.collapsed.retain(|hash| hashes.contains(hash.as_str()));
        self.seen.retain(|hash| hashes.contains(hash.as_str()));
        let new: HashSet<_> = merges(commits)
            .filter(|hash| !self.seen.contains(*hash))
            .map(str::to_owned)
            .collect();
        // Folds already chosen need the ancestry even where a partial
        // layout would hide nothing.
        self.update(commits, !self.collapsed.is_empty())?;
        if self.start_collapsed && !new.is_empty() {
            self.fold_many(&new, commits)?;
        }
        // After a failure, new merges start folded on the next refresh.
        self.seen.extend(new);
        Ok(())
    }

    pub fn toggle(&mut self, index: usize, commits: &[Commit]) -> Result<(), String> {
        let Some(commit) = commits.get(index).filter(|c| c.parents.len() > 1) else {
            return Ok(());
        };
        self.prepare_toggle(commits);
        if let Fold::Folded(_) = self.fold(index) {
            self.collapsed.remove(&commit.hash);
            return self.update(commits, false);
        }
        let previous = self.collapsed.clone();
        self.collapsed.insert(commit.hash.clone());
        // Folding on request resolves history a filter omitted, which a
        // disclosure marker does not.
        let result = self.update(commits, true);
        if result.is_ok() && matches!(self.fold(index), Fold::Folded(_)) {
            return Ok(());
        }
        self.collapsed = previous;
        let restored = self.update(commits, false);
        result.and(restored)?;
        Err(if self.graph.merged_rows(index) {
            "Nothing to fold: other shown history reaches this merge's commits".into()
        } else {
            "No merged commits in this history; revision and path filters still apply".into()
        })
    }

    /// Fold every merge that hides something, or expand all if they already
    /// are. Returns the selection, moved to its fold if that hides it.
    pub fn toggle_all(&mut self, selected: usize, commits: &[Commit]) -> Result<usize, String> {
        self.prepare_toggle(commits);
        let previous = std::mem::take(&mut self.collapsed);
        let all = merges(commits).map(str::to_owned).collect();
        let folded = self.fold_many(&all, commits).and_then(|()| {
            if self
                .layout
                .folds
                .iter()
                .any(|f| matches!(f, Fold::Folded(_)))
            {
                Ok(())
            } else {
                Err(
                    "No foldable merges in this history; revision and path filters still apply"
                        .to_owned(),
                )
            }
        });
        if folded.is_err() || self.collapsed == previous {
            self.collapsed = if folded.is_err() {
                previous
            } else {
                HashSet::new()
            };
            let restored = self.update(commits, false);
            return folded.and(restored).map(|()| selected);
        }
        Ok(if self.visible(selected) {
            selected
        } else {
            self.containing_fold(selected).unwrap_or(selected)
        })
    }

    pub fn reveal(&mut self, index: usize, commits: &[Commit]) {
        while !self.layout.visible.get(index).copied().unwrap_or(true) {
            let Some(fold) = self.containing_fold(index) else {
                return;
            };
            self.collapsed.remove(&commits[fold].hash);
            // A failure shows the Log unfolded, which reveals it too.
            let _ = self.update(commits, false);
        }
    }

    /// Add folds for `merges`, keeping only those that are hidden inside
    /// another fold or hide something themselves.
    fn fold_many(&mut self, merges: &HashSet<String>, commits: &[Commit]) -> Result<(), String> {
        self.collapsed.extend(merges.iter().cloned());
        // Read omitted history for merges whose side history is loaded, but
        // not for those, as under --first-parent, whose side parents a
        // filter omitted: that could walk the whole repository.
        let loaded_side = merges.iter().any(|hash| {
            self.graph.index.get(hash).is_some_and(|&merge| {
                self.graph.parents[merge]
                    .iter()
                    .any(|&(parent, side)| side && parent < self.graph.rows)
            })
        });
        self.update(commits, loaded_side)?;
        let before = self.collapsed.len();
        self.collapsed.retain(|hash| {
            !merges.contains(hash)
                || !self.graph.index.get(hash).is_some_and(|&i| {
                    self.layout.visible[i] && !matches!(self.layout.folds[i], Fold::Folded(_))
                })
        });
        // Expanding a fold that hides nothing shows the same rows, but can
        // change which other merges are foldable.
        if self.collapsed.len() != before {
            self.update(commits, false)?;
        }
        Ok(())
    }

    // Toggles act on what is shown, so after a failure they start unfolded.
    // A new App's commits have not been laid out yet.
    fn prepare_toggle(&mut self, commits: &[Commit]) {
        if self.graph.rows != commits.len() {
            let _ = self.refresh(commits);
        }
        if self.failed {
            self.collapsed.clear();
        }
    }

    fn read_omitted(&mut self, commits: &[Commit]) -> Result<Ancestry, String> {
        if let Some(ancestry) = &self.omitted {
            return Ok(ancestry.clone());
        }
        let ancestry = leading_to_rows((self.load_ancestry)(commits)?, commits);
        self.omitted = Some(ancestry.clone());
        Ok(ancestry)
    }

    /// Lay out the folds. Without the ancestry of commits a filter omitted,
    /// a fold could hide commits that other history reaches through them, so
    /// read it before hiding anything, or when `exact` asks for it anyway.
    fn update(&mut self, commits: &[Commit], exact: bool) -> Result<(), String> {
        let mut layout = Layout::new(&self.graph, commits, &self.collapsed);
        if self.graph.unknown_below.is_some()
            && (exact
                || layout.visible.contains(&false)
                || !self.hidden_types.is_empty()
                || self.hide_merges)
        {
            match self.read_omitted(commits) {
                Ok(ancestry) => {
                    self.graph = Graph::new(commits, Some(&ancestry));
                    self.ancestry = Some(ancestry);
                    layout = Layout::new(&self.graph, commits, &self.collapsed);
                }
                Err(error) => {
                    self.show(Layout::new(&self.graph, commits, &HashSet::new()), commits);
                    self.failed = true;
                    return Err(error);
                }
            }
        }
        if !self.ascii_graph
            && self.graph.unknown_below.is_some()
            && self.unicode_graph.is_none()
            && !self.unicode_failed
        {
            match self.read_omitted(commits) {
                Ok(ancestry) => self.unicode_graph = Some(Graph::new(commits, Some(&ancestry))),
                Err(_) => self.unicode_failed = true,
            }
        }
        self.show(layout, commits);
        self.failed = false;
        Ok(())
    }

    /// Show `layout`, drawing only the graph the Log displays: filters draw
    /// their own, and Git's graph is shown unchanged where it is exact.
    fn show(&mut self, layout: Layout, commits: &[Commit]) {
        self.layout = layout;
        self.apply_filter(commits);
        if self.filtered.is_some() {
            return;
        }
        let unicode = self.unicode_shown();
        if unicode && self.graph.unknown_below.is_some() {
            self.layout.unicode =
                (self.unicode_graph.as_ref()).map(|graph| self.layout.draw_all(graph, commits));
        }
        if !self.layout.git_ascii || unicode && self.layout.unicode.is_none() {
            self.layout.graphs = self.layout.draw_all(&self.graph, commits);
        }
    }

    pub fn filter_hidden(&self, index: usize, commit: &Commit) -> bool {
        // A shown folded merge stands in for its hidden side history and is
        // where `z` expands it, so filters leave it in place.
        if self.layout.visible.get(index) == Some(&true)
            && matches!(self.fold(index), Fold::Folded(_))
        {
            return false;
        }
        (self.hide_merges && commit.kind == CommitKind::Revision && commit.parents.len() > 1)
            || (self.graph.branch_points.get(index) != Some(&true)
                && commit.decorations.is_empty()
                && commit_type(commit)
                    .is_some_and(|kind| self.hidden_types.contains(kind.as_ref())))
    }

    pub fn toggle_type(&mut self, kind: Option<&str>, commits: &[Commit]) -> Result<(), String> {
        if let Some(kind) = kind {
            let kind =
                normalize_type(kind).ok_or_else(|| format!("invalid commit type {kind:?}"))?;
            if !self.hidden_types.remove(kind.as_ref()) {
                self.hidden_types.insert(kind.into_owned());
            }
        } else {
            self.hidden_types.clear();
            self.hide_merges = false;
        }
        if self.graph.rows != commits.len() {
            self.refresh(commits)
        } else {
            self.update(commits, false)
        }
    }

    pub fn toggle_merges(&mut self, commits: &[Commit]) -> Result<(), String> {
        self.hide_merges = !self.hide_merges;
        if self.graph.rows != commits.len() {
            self.refresh(commits)
        } else {
            self.update(commits, false)
        }
    }

    fn apply_filter(&mut self, commits: &[Commit]) {
        self.filtered = None;
        self.hidden_count = 0;
        if self.hidden_types.is_empty() && !self.hide_merges {
            return;
        }
        let mut layout = self.layout.clone();
        let hidden: Vec<_> = commits
            .iter()
            .enumerate()
            .map(|(index, commit)| self.filter_hidden(index, commit))
            .collect();
        for (index, commit) in commits.iter().enumerate() {
            self.hidden_count += usize::from(hidden[index]);
            // Keep merge junctions in the drawing even when their text is
            // filtered out. Merge folds still control branch visibility.
            layout.visible[index] &= !hidden[index] || commit.parents.len() > 1;
        }
        // Draw the folds the layout shows, which after a failed ancestry
        // read are none of the saved ones. A filtered junction keeps only
        // its incoming routing rows; outgoing edges are attached to the
        // next row.
        let reached = Layout::with_folds(&self.graph, &layout.folded).1;
        layout.graphs = layout.draw(&self.graph, commits, &layout.folded, &reached, &hidden);
        layout.git_ascii = false;
        layout.unicode = None;
        for (visible, hidden) in layout.visible.iter_mut().zip(hidden) {
            *visible &= !hidden;
        }
        self.filtered = Some(layout);
    }

    /// The nearest shown fold whose side history leads to a hidden row.
    fn containing_fold(&self, index: usize) -> Option<usize> {
        self.layout.owners.get(index).copied().flatten()
    }
}

/// Conventional Commit prefix, taken only from the subject of a revision.
/// Custom ASCII types and issue references before the scope are supported,
/// so any one-word prefix, such as `Note:` or `WIP:`, counts as a type.
pub fn commit_type(commit: &Commit) -> Option<std::borrow::Cow<'_, str>> {
    commit_type_prefix(commit).and_then(normalize_type)
}

/// The recognized type as written in the subject, before alias normalization.
pub fn commit_type_prefix(commit: &Commit) -> Option<&str> {
    if commit.kind != CommitKind::Revision {
        return None;
    }
    subject_type_prefix(&commit.subject)
}

/// Recognize a type in a subject, including subjects carried by reflog actions.
pub fn subject_type_prefix(subject: &str) -> Option<&str> {
    let (prefix, description) = subject.split_once(": ")?;
    if description.is_empty() || prefix.contains(['\n', '\r']) {
        return None;
    }
    let prefix = prefix.strip_suffix('!').unwrap_or(prefix);
    let kind = if let Some((kind, scope)) = prefix.split_once('(') {
        let scope = scope.strip_suffix(')')?;
        if scope.is_empty() || scope.contains(['(', ')']) {
            return None;
        }
        kind
    } else {
        prefix
    };
    let kind = if let Some((kind, issues)) = kind.split_once(' ') {
        // Accept project extensions such as `test #234(failing):` and
        // `fix #233 #234:` without treating arbitrary prose as a type.
        if issues.is_empty()
            || !issues.split(' ').all(|issue| {
                issue.strip_prefix('#').is_some_and(|number| {
                    !number.is_empty() && number.bytes().all(|c| c.is_ascii_digit())
                })
            })
        {
            return None;
        }
        kind
    } else {
        kind
    };
    normalize_type(kind).map(|_| kind)
}

/// Validate a filter type and use the same aliases in the CLI and TUI.
pub fn normalize_type(kind: &str) -> Option<std::borrow::Cow<'_, str>> {
    use std::borrow::Cow;
    if !kind.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        || !kind.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return None;
    }
    let lowercase = if kind.bytes().any(|c| c.is_ascii_uppercase()) {
        Cow::Owned(kind.to_ascii_lowercase())
    } else {
        Cow::Borrowed(kind)
    };
    Some(match lowercase.as_ref() {
        "doc" => Cow::Borrowed("docs"),
        "tests" => Cow::Borrowed("test"),
        "feature" => Cow::Borrowed("feat"),
        _ => lowercase,
    })
}

/// The byte position of a Conventional Commit's explicit breaking-change marker.
pub fn subject_breaking_marker(subject: &str) -> Option<usize> {
    subject_type_prefix(subject)?;
    let prefix = subject.split_once(": ")?.0;
    prefix.ends_with('!').then(|| prefix.len() - 1)
}

/// The omitted commits that lead to a row. The rest, such as the history
/// of a branch that diverged before the oldest row, connect no rows, and
/// would otherwise enlarge every layout.
fn leading_to_rows(mut ancestry: Ancestry, commits: &[Commit]) -> Ancestry {
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for (hash, parents) in &ancestry {
        for parent in parents {
            children.entry(parent).or_default().push(hash);
        }
    }
    let mut todo: Vec<_> = commits.iter().map(|c| c.hash.as_str()).collect();
    let mut leading = HashSet::new();
    while let Some(hash) = todo.pop() {
        for &child in children.get(hash).into_iter().flatten() {
            if leading.insert(child) {
                todo.push(child);
            }
        }
    }
    let leading: HashSet<String> = leading.into_iter().map(str::to_owned).collect();
    ancestry.retain(|hash, _| leading.contains(hash));
    ancestry
}

fn merges(commits: &[Commit]) -> impl Iterator<Item = &str> {
    commits
        .iter()
        .filter(|c| c.kind == CommitKind::Revision && c.parents.len() > 1)
        .map(|c| c.hash.as_str())
}

/// The loaded rows, then the commits a filter omitted between them, whose
/// parent links come from the ancestry walk.
#[derive(Default)]
struct Graph {
    rows: usize,
    index: HashMap<String, usize>,
    // Each parent, and whether it is a side parent.
    parents: Vec<Vec<(usize, bool)>>,
    // Children before parents.
    order: Vec<usize>,
    tips: Vec<bool>,
    // Commits with multiple children anchor the points where history forks.
    branch_points: Vec<bool>,
    // The first row with a parent that is neither loaded nor known to be
    // irrelevant. Omitted commits may connect it to any row below it.
    unknown_below: Option<usize>,
}

impl Graph {
    fn new(commits: &[Commit], ancestry: Option<&Ancestry>) -> Self {
        let mut index: HashMap<String, usize> = commits
            .iter()
            .enumerate()
            .map(|(i, c)| (c.hash.clone(), i))
            .collect();
        let rows = commits.len();
        let mut nodes = rows;
        for hash in ancestry.into_iter().flat_map(|a| a.keys()) {
            index.entry(hash.clone()).or_insert_with(|| {
                nodes += 1;
                nodes - 1
            });
        }
        // The ancestry walk stops below the oldest row, whose parents
        // cannot lead back to any row.
        let boundary: HashSet<_> = commits
            .iter()
            .rfind(|c| c.kind == CommitKind::Revision)
            .into_iter()
            .flat_map(|c| c.graph_parents())
            .collect();
        let mut unknown_below = None;
        let mut parents = vec![Vec::new(); nodes];
        for (row, commit) in commits.iter().enumerate() {
            for (slot, parent) in commit.graph_parents().iter().enumerate() {
                match index.get(parent) {
                    Some(&node) => parents[row].push((node, slot > 0)),
                    None if ancestry.is_none() && !boundary.contains(parent) => {
                        unknown_below = unknown_below.or(Some(row));
                    }
                    None => {}
                }
            }
        }
        let first_parent_only = commits.iter().any(|commit| commit.first_parent_only);
        for (hash, links) in ancestry.into_iter().flatten() {
            let node = index[hash];
            if node >= rows {
                let links = if first_parent_only {
                    &links[..links.len().min(1)]
                } else {
                    links.as_slice()
                };
                // Mark side parents before dropping those that lead to no
                // row, which may include the first parent.
                parents[node] = links
                    .iter()
                    .enumerate()
                    .filter_map(|(slot, parent)| Some((*index.get(parent)?, slot > 0)))
                    .collect();
            }
        }
        // Tips are the rows no other row reaches.
        let below = reachable(&parents, parents[..rows].iter().flatten().map(|&(p, _)| p));
        let tips = below[..rows].iter().map(|&below| !below).collect();
        let mut incoming = vec![0usize; nodes];
        for &(parent, _) in parents.iter().flatten() {
            incoming[parent] += 1;
        }
        let branch_points = incoming[..rows]
            .iter()
            .map(|&children| children > 1)
            .collect();
        let mut todo: Vec<_> = (0..nodes).filter(|&n| incoming[n] == 0).collect();
        let mut order = Vec::with_capacity(nodes);
        while let Some(node) = todo.pop() {
            order.push(node);
            for &(parent, _) in &parents[node] {
                incoming[parent] -= 1;
                if incoming[parent] == 0 {
                    todo.push(parent);
                }
            }
        }
        Self {
            rows,
            index,
            parents,
            order,
            tips,
            branch_points,
            unknown_below,
        }
    }

    /// Count distinct hidden rows reached through a merge's side history,
    /// including rows shared with other folds and inside nested folds.
    fn hidden_side_count(&self, merge: usize, reached: &[bool]) -> usize {
        let mut todo: Vec<_> = self.parents[merge]
            .iter()
            .filter(|(_, side)| *side)
            .map(|&(parent, _)| parent)
            .collect();
        let mut visited = HashSet::new();
        let mut rows = 0;
        while let Some(node) = todo.pop() {
            if reached[node] || !visited.insert(node) {
                continue;
            }
            if node < self.rows {
                rows += 1;
            }
            todo.extend(self.parents[node].iter().map(|&(parent, _)| parent));
        }
        rows
    }

    /// Whether a merge brought in any row: one its side parents reach but
    /// its first parent does not.
    fn merged_rows(&self, merge: usize) -> bool {
        let reach = |side| {
            let starts = self.parents[merge].iter().filter(move |p| p.1 == side);
            reachable(&self.parents, starts.map(|&(parent, _)| parent))
        };
        let (mainline, side) = (reach(false), reach(true));
        (0..self.rows).any(|row| side[row] && !mainline[row])
    }
}

fn reachable(parents: &[Vec<(usize, bool)>], starts: impl Iterator<Item = usize>) -> Vec<bool> {
    let mut seen = vec![false; parents.len()];
    let mut todo: Vec<_> = starts.collect();
    while let Some(node) = todo.pop() {
        if !std::mem::replace(&mut seen[node], true) {
            todo.extend(parents[node].iter().map(|&(parent, _)| parent));
        }
    }
    seen
}

/// Which shown rows, if any, reach a node through unfolded edges. A row
/// reached only by one merge's side parents is hidden by folding it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    None,
    One { row: usize, side: bool },
    Many,
}

impl Reach {
    fn join(self, other: Reach) -> Reach {
        match (self, other) {
            (Reach::None, reach) | (reach, Reach::None) => reach,
            (a, b) if a == b => a,
            _ => Reach::Many,
        }
    }
}

#[derive(Clone, Default)]
struct GraphRows {
    ascii: Vec<String>,
    unicode: Vec<String>,
}

impl GraphRows {
    fn get(&self, unicode: bool) -> &Vec<String> {
        if unicode {
            &self.unicode
        } else {
            &self.ascii
        }
    }
}

#[derive(Clone, Default)]
struct Layout {
    visible: Vec<bool>,
    // The fold containing each hidden row.
    owners: Vec<Option<usize>>,
    folds: Vec<Fold>,
    // The folds drawn, without those that hide nothing.
    folded: Vec<bool>,
    graphs: Drawing,
    // Git's own graph is exact while every row is shown.
    git_ascii: bool,
    // Unicode rows drawn through history Git omitted.
    unicode: Option<Drawing>,
}

impl Layout {
    fn new(graph: &Graph, commits: &[Commit], collapsed: &HashSet<String>) -> Self {
        let mut folded: Vec<_> = commits
            .iter()
            .map(|c| c.parents.len() > 1 && collapsed.contains(&c.hash))
            .collect();
        let mut layout = Self::with_folds(graph, &folded).0;
        layout.unfold_idle_merges(&mut folded);
        // The ASCII option retains Git's exact original layout.
        layout.git_ascii = !layout.visible.contains(&false);
        layout.folded = folded;
        layout
    }

    /// Normalize drawing choices without changing the saved merge folds.
    fn unfold_idle_merges(&self, folded: &mut [bool]) {
        // A shown fold that hides nothing, such as one inside a fold that
        // was since expanded, stays chosen: folding another merge could
        // make it hide something again. It is drawn as a merge, though.
        // Following its side parents reaches only shown rows.
        for (row, folded) in folded.iter_mut().enumerate() {
            if *folded && self.visible[row] && self.folds[row] == Fold::None {
                *folded = false;
            }
        }
    }

    /// The layout without its redrawn graph, and which nodes shown rows reach.
    fn with_folds(graph: &Graph, folded: &[bool]) -> (Self, Vec<bool>) {
        let rows = graph.rows;
        let mut reach = vec![Reach::None; graph.parents.len()];
        let mut visible = vec![false; rows];
        let mut foldable = vec![false; rows];
        for &node in &graph.order {
            let inherited = if node < rows {
                // Unless omitted commits could reach this row too.
                if let Reach::One { row, side: true } = reach[node] {
                    foldable[row] |= graph.unknown_below.is_none_or(|unknown| node <= unknown);
                }
                if !graph.tips[node] && reach[node] == Reach::None {
                    continue;
                }
                visible[node] = true;
                None
            } else if reach[node] == Reach::None {
                continue;
            } else {
                Some(reach[node])
            };
            for &(parent, side) in &graph.parents[node] {
                let incoming = match inherited {
                    Some(reach) => reach,
                    None if folded[node] && side => continue,
                    None => Reach::One { row: node, side },
                };
                reach[parent] = reach[parent].join(incoming);
            }
        }
        let reached: Vec<_> = (0..graph.parents.len())
            .map(|node| {
                if node < rows {
                    visible[node]
                } else {
                    reach[node] != Reach::None
                }
            })
            .collect();
        // Each hidden commit belongs to the nearest shown fold whose side
        // history leads to it. Expanding that fold reveals it, or the
        // nested fold that contains it.
        let mut owners = vec![None::<usize>; graph.parents.len()];
        for &node in &graph.order {
            // A shown fold claims its side history; hidden history passes
            // its owner on to all its parents.
            let (owner, side_only) = if reached[node] {
                if !folded.get(node).copied().unwrap_or(false) {
                    continue;
                }
                (Some(node), true)
            } else {
                (owners[node], false)
            };
            for &(parent, side) in &graph.parents[node] {
                if (side || !side_only) && !reached[parent] {
                    owners[parent] = owners[parent].max(owner);
                }
            }
        }
        let folds = (0..rows)
            .map(|row| {
                if !visible[row] {
                    Fold::None
                } else if !folded[row] {
                    if foldable[row] {
                        Fold::Foldable
                    } else {
                        Fold::None
                    }
                } else {
                    // A fold hides something when its side history reaches
                    // a hidden row. Count those another fold owns as well.
                    match graph.hidden_side_count(row, &reached) {
                        0 => Fold::None,
                        count => Fold::Folded(count),
                    }
                }
            })
            .collect();
        owners.truncate(rows);
        let layout = Self {
            visible,
            owners,
            folds,
            folded: folded.to_vec(),
            graphs: Drawing::default(),
            git_ascii: false,
            unicode: None,
        };
        (layout, reached)
    }

    /// Redraw the graph over every shown row.
    fn draw_all(&self, graph: &Graph, commits: &[Commit]) -> Drawing {
        let reached = Self::with_folds(graph, &self.folded).1;
        self.draw(graph, commits, &self.folded, &reached, &[])
    }

    /// Redraw the graph over the shown rows, keeping Git's lane colours.
    /// `trimmed` rows keep their routing rows but not their node.
    fn draw(
        &self,
        graph: &Graph,
        commits: &[Commit],
        folded: &[bool],
        reached: &[bool],
        trimmed: &[bool],
    ) -> Drawing {
        // Compute destinations parents first. Type-hidden loaded commits
        // preserve every unfolded parent; ancestry omitted by Git follows
        // first parents, matching the existing merge-fold projection.
        let mut leads: Vec<Vec<usize>> = vec![Vec::new(); graph.parents.len()];
        for &node in graph.order.iter().rev() {
            if node < graph.rows && self.visible[node] {
                leads[node].push(node);
            } else if reached[node] {
                for &(parent, side) in &graph.parents[node] {
                    if side && (node >= graph.rows || folded[node]) {
                        continue;
                    }
                    let parents = leads[parent].clone();
                    for row in parents {
                        if !leads[node].contains(&row) {
                            leads[node].push(row);
                        }
                    }
                }
            }
        }
        // A folded merge follows its first parent. Other edges pass
        // through omitted commits to the shown row they lead to.
        let projected = (0..commits.len())
            .map(|index| {
                let mut projected = Vec::new();
                for &(parent, side) in &graph.parents[index] {
                    if folded[index] && side {
                        continue;
                    }
                    for &row in &leads[parent] {
                        if !projected.contains(&row) {
                            projected.push(row);
                        }
                    }
                }
                projected
            })
            .collect();
        // Git colours each edge after the commit it leads to, so a lane
        // takes its row's colour. Git draws no edge into the first row or a
        // row directly below its child; those lanes use the palette instead.
        let mut colours: Vec<String> = PALETTE.iter().map(|&c| c.to_owned()).collect();
        let git_colours = (0..commits.len())
            .map(|row| {
                let colour = incoming_colour(commits, row)?;
                Some(colours.iter().position(|c| c == colour).unwrap_or_else(|| {
                    colours.push(colour.to_owned());
                    colours.len() - 1
                }))
            })
            .collect();
        let fixed = commits
            .iter()
            .enumerate()
            .filter(|&(index, commit)| self.visible[index] && commit.kind != CommitKind::Revision)
            .map(|(index, commit)| {
                let unicode = commit
                    .graph
                    .iter()
                    .map(|row| {
                        row.replacen(
                            commit.graph_node,
                            &unicode_node(commit.graph_node).to_string(),
                            1,
                        )
                    })
                    .collect();
                let rows = GraphRows {
                    ascii: commit.graph.clone(),
                    unicode,
                };
                (index, rows)
            })
            .collect();
        Drawing(std::rc::Rc::new(Plan {
            visible: self.visible.clone(),
            trimmed: trimmed.to_vec(),
            nodes: commits.iter().map(|c| c.graph_node).collect(),
            projected,
            colours,
            git_colours,
            fixed,
            ascii: std::cell::OnceCell::new(),
            unicode: std::cell::OnceCell::new(),
        }))
    }
}

/// Rows per checkpoint of the lane assignment.
const BLOCK: usize = 64;

/// The graph redrawn over the shown rows. Building the text of every row
/// took seconds in long histories, so only the lane assignment runs ahead,
/// recording each row's height and a checkpoint per block of rows. A block's
/// text is built when one of its rows is first displayed.
#[derive(Clone, Default)]
struct Drawing(std::rc::Rc<Plan>);

impl Drawing {
    /// The rows drawn for a shown row.
    fn rows(&self, unicode: bool, index: usize) -> Option<&[String]> {
        let plan = &self.0;
        let lanes = plan.lanes(unicode);
        let block = lanes.blocks.get(index / BLOCK)?;
        block.get_or_init(|| plan.draw_block(lanes, index / BLOCK))[index % BLOCK].as_deref()
    }

    /// The number of rows drawn for a shown row.
    fn height(&self, unicode: bool, index: usize) -> Option<usize> {
        self.0.visible.get(index).filter(|&&visible| visible)?;
        Some(self.0.lanes(unicode).heights[index])
    }
}

#[derive(Default)]
struct Plan {
    visible: Vec<bool>,
    trimmed: Vec<bool>,
    nodes: Vec<char>,
    // Each row's parent rows, past the commits it does not show.
    projected: Vec<Vec<usize>>,
    // Lanes refer to colours by their index.
    colours: Vec<String>,
    git_colours: Vec<Option<usize>>,
    // Rows Git drew without commits, such as the working tree.
    fixed: HashMap<usize, GraphRows>,
    // Each style's lanes, assigned when the style is first shown.
    ascii: std::cell::OnceCell<Lanes>,
    unicode: std::cell::OnceCell<Lanes>,
}

/// The lane assignment of one graph style.
#[derive(Default)]
struct Lanes {
    unicode: bool,
    heights: Vec<usize>,
    checkpoints: Vec<State>,
    blocks: Vec<std::cell::OnceCell<Vec<Option<Vec<String>>>>>,
}

/// The row each lane leads to, and its colour, or `None` for a gap.
type Lane = Option<(usize, usize)>;

/// The lane assignment before a row.
#[derive(Clone, Default, PartialEq)]
struct State {
    lanes: Vec<Lane>,
    palette: usize,
    // The edges out of the previous shown commit, drawn above the next.
    edges: Vec<(usize, usize, usize)>,
    width: usize,
}

/// A shown commit's place in the lanes.
struct Row {
    column: usize,
    lanes: Vec<Lane>,
    above: Vec<(usize, usize, usize)>,
    above_width: usize,
}

fn lane_to(lanes: &[Lane], row: usize) -> Option<usize> {
    lanes
        .iter()
        .position(|lane| lane.is_some_and(|(target, _)| target == row))
}

impl Plan {
    fn lanes(&self, unicode: bool) -> &Lanes {
        let lanes = if unicode { &self.unicode } else { &self.ascii };
        lanes.get_or_init(|| self.assign(unicode))
    }

    fn assign(&self, unicode: bool) -> Lanes {
        let mut lanes = Lanes {
            unicode,
            heights: Vec::with_capacity(self.visible.len()),
            ..Lanes::default()
        };
        let mut state = State::default();
        for index in 0..self.visible.len() {
            if index % BLOCK == 0 {
                lanes.checkpoints.push(state.clone());
                lanes.blocks.push(std::cell::OnceCell::new());
            }
            let height = if let Some(rows) = self.fixed.get(&index) {
                rows.get(unicode).len()
            } else if let Some(row) = self.step(&mut state, index, unicode) {
                let edges = self.edges(&row.above);
                let trim = usize::from(self.trimmed.get(index) == Some(&true));
                let transitions = if unicode {
                    route_count(&edges, row.above_width)
                } else {
                    transition_count(&edges)
                };
                transitions + 1 - trim
            } else {
                0
            };
            lanes.heights.push(height);
        }
        lanes
    }

    fn palette(&self, state: &mut State) -> usize {
        state.palette += 1;
        (state.palette - 1) % PALETTE.len()
    }

    /// Assign a shown commit its lane and route its parents' lanes.
    ///
    /// Like Git, ASCII lanes close up when one ends. That shifts every lane
    /// to its right, which Unicode routes could draw only one per row, so
    /// Unicode lanes keep their columns instead: an ended lane leaves a gap
    /// for a later one, and a commit's first parent moves into its column.
    fn step(&self, state: &mut State, index: usize, unicode: bool) -> Option<Row> {
        if !self.visible[index] || self.fixed.contains_key(&index) {
            return None;
        }
        let column = match lane_to(&state.lanes, index) {
            Some(column) => column,
            None => {
                let colour = match self.git_colours[index] {
                    Some(colour) => colour,
                    None => self.palette(state),
                };
                let gap = state.lanes.iter().position(Option::is_none);
                let column = gap.unwrap_or(state.lanes.len());
                if gap.is_none() {
                    state.lanes.push(None);
                }
                state.lanes[column] = Some((index, colour));
                column
            }
        };
        let projected = &self.projected[index];
        let mut next = state.lanes.clone();
        let (_, colour) = if unicode {
            next[column].take()
        } else {
            next.remove(column)
        }
        .unwrap();
        let mut insert = column;
        for &parent in projected {
            let existing = lane_to(&next, parent);
            if unicode && insert == column && existing.is_some_and(|lane| lane > column) {
                next.swap(column, existing.unwrap());
                insert += 1;
            } else if existing.is_none() {
                // Without Git's colour, the first new parent continues
                // this lane's colour.
                let colour = match self.git_colours[parent] {
                    Some(colour) => colour,
                    None if insert == column => colour,
                    None => self.palette(state),
                };
                if unicode {
                    // Take the nearest gap, or a new lane.
                    insert = (insert..next.len())
                        .find(|&lane| next[lane].is_none())
                        .unwrap_or(next.len());
                    if insert == next.len() {
                        next.push(None);
                    }
                    next[insert] = Some((parent, colour));
                } else {
                    next.insert(insert, Some((parent, colour)));
                }
                insert += 1;
            }
        }
        while next.last() == Some(&None) {
            next.pop();
        }
        // The lanes as they were above this row.
        let lanes = std::mem::take(&mut state.lanes);
        let mut edges = Vec::new();
        for (from, lane) in lanes.iter().enumerate() {
            let Some((row, _)) = lane else {
                continue;
            };
            let targets = if from == column {
                &projected[..]
            } else {
                std::slice::from_ref(row)
            };
            for &target in targets {
                if let Some(to) = lane_to(&next, target) {
                    edges.push((2 * from, 2 * to, next[to].unwrap().1));
                }
            }
        }
        let width = lanes.len().max(next.len());
        state.lanes = next;
        let above = std::mem::replace(&mut state.edges, edges);
        let above_width = std::mem::replace(&mut state.width, width);
        Some(Row {
            column,
            lanes,
            above,
            above_width,
        })
    }

    fn edges(&self, edges: &[(usize, usize, usize)]) -> Vec<(usize, usize, &str)> {
        edges
            .iter()
            .map(|&(from, to, colour)| (from, to, self.colours[colour].as_str()))
            .collect()
    }

    fn draw_block(&self, lanes: &Lanes, block: usize) -> Vec<Option<Vec<String>>> {
        let unicode = lanes.unicode;
        let mut state = lanes.checkpoints[block].clone();
        let start = block * BLOCK;
        (start..self.visible.len().min(start + BLOCK))
            .map(|index| {
                if let Some(rows) = self.fixed.get(&index) {
                    return Some(rows.get(unicode).clone());
                }
                let row = self.step(&mut state, index, unicode)?;
                let edges = self.edges(&row.above);
                let mut rows = if unicode {
                    connected_transitions(&edges, row.above_width)
                } else {
                    transitions(&edges, row.above_width)
                };
                if self.trimmed.get(index) != Some(&true) {
                    let (node, line) = if unicode {
                        (unicode_node(self.nodes[index]), '│')
                    } else {
                        (self.nodes[index], '|')
                    };
                    let row = row
                        .lanes
                        .iter()
                        .enumerate()
                        .map(|(i, lane)| match lane {
                            _ if i == row.column => format!("{node} "),
                            Some((_, colour)) => {
                                format!("{} ", paint(line, &self.colours[*colour]))
                            }
                            None => "  ".to_owned(),
                        })
                        .collect();
                    rows.push(row);
                }
                Some(rows)
            })
            .collect()
    }
}

// Git's default graph colours, cycled per lane.
const PALETTE: [&str; 12] = [
    "31", "32", "33", "34", "35", "36", "1;31", "1;32", "1;33", "1;34", "1;35", "1;36",
];

fn paint(symbol: char, colour: &str) -> String {
    format!("\x1b[{colour}m{symbol}\x1b[m")
}

/// The SGR colour of the edge Git drew into a row's node: the line above
/// it, or a diagonal ending beside it.
fn incoming_colour(commits: &[Commit], row: usize) -> Option<&str> {
    let graph = &commits[row].graph;
    let node = graph.last()?;
    let above = match graph.len() {
        0 | 1 => commits.get(row.checked_sub(1)?)?.graph.last()?,
        len => &graph[len - 2],
    };
    let marker = commits[row].graph_node;
    let column = cells(node)
        .iter()
        .position(|&(symbol, _)| symbol == marker)?;
    let above = cells(above);
    let edge = |at: Option<usize>, expected| {
        at.and_then(|at| above.get(at))
            .filter(|&&(symbol, _)| symbol == expected)
            .and_then(|&(_, colour)| colour)
    };
    edge(Some(column), '|')
        .or_else(|| edge(column.checked_sub(1), '\\'))
        .or_else(|| edge(Some(column + 1), '/'))
}

/// Each character of a graph line with the SGR colour it is drawn in.
fn cells(line: &str) -> Vec<(char, Option<&str>)> {
    let mut cells = Vec::new();
    let mut colour = None;
    let mut rest = line;
    while let Some(c) = rest.chars().next() {
        if let Some(sequence) = rest.strip_prefix("\x1b[") {
            let Some(end) = sequence.find('m') else {
                break;
            };
            colour = Some(&sequence[..end]).filter(|code| !matches!(*code, "" | "0"));
            rest = &sequence[end + 1..];
        } else {
            cells.push((c, colour));
            rest = &rest[c.len_utf8()..];
        }
    }
    cells
}

// Route edges between columns two characters apart, each in its target
// lane's colour. As in Git, each row moves a route one lane, drawn between
// the two. Crossings retain their independent targets; '+' joins routes to
// the same target, 'X' crosses routes to different targets.
fn transitions(edges: &[(usize, usize, &str)], width: usize) -> Vec<String> {
    let lanes = |from: usize, to: usize| from.abs_diff(to) / 2;
    let mut rows = Vec::new();
    for step in 1..=transition_count(edges) {
        let mut cells = vec![(' ', ""); width * 2];
        let mut targets = vec![None; width * 2];
        for &(from, to, colour) in edges {
            let (position, symbol) = if step > lanes(from, to) {
                (to, '|')
            } else if from < to {
                (from + 2 * step - 1, '\\')
            } else {
                (from + 1 - 2 * step, '/')
            };
            let symbol = match cells[position].0 {
                ' ' => symbol,
                existing if targets[position] == Some(to) && existing == symbol => symbol,
                _ if targets[position] == Some(to) => '+',
                _ => 'X',
            };
            cells[position] = (symbol, colour);
            targets[position] = Some(to);
        }
        let used = cells
            .iter()
            .rposition(|&(c, _)| c != ' ')
            .map_or(0, |i| i + 1);
        let row: String = cells[..used]
            .iter()
            .map(|&(symbol, colour)| {
                if symbol == ' ' {
                    " ".to_owned()
                } else {
                    paint(symbol, colour)
                }
            })
            .collect();
        rows.push(format!("{row} "));
    }
    rows
}

/// The number of rows `transitions` draws: one per lane of the longest move.
fn transition_count<C>(edges: &[(usize, usize, C)]) -> usize {
    edges
        .iter()
        .map(|&(from, to, _)| from.abs_diff(to) / 2)
        .max()
        .unwrap_or(0)
}

/// The Unicode symbol for one of Git's node markers.
pub fn unicode_node(node: char) -> char {
    match node {
        '*' => '●',
        'o' => '○',
        marker => marker,
    }
}

/// Route one connection per row. Occupied destinations move out of the way
/// first, so unrelated routes never share a vertical track. A spare track
/// breaks cycles such as two lanes exchanging places. Horizontal paths pass
/// over vertical tracks with `╪`; only endpoints are actual junctions.
fn connected_transitions(edges: &[(usize, usize, &str)], width: usize) -> Vec<String> {
    route(edges, width)
        .into_iter()
        .map(|cells| {
            let row: String = cells
                .iter()
                .map(|&(directions, colour)| {
                    let symbol = match directions {
                        0 => return " ".to_owned(),
                        3 => '│',
                        12 => '─',
                        9 => '╰',
                        5 => '╯',
                        10 => '╭',
                        6 => '╮',
                        11 => '├',
                        7 => '┤',
                        15 => '╪',
                        _ => unreachable!("invalid graph connection {directions}"),
                    };
                    paint(symbol, colour)
                })
                .collect();
            format!("{row} ")
        })
        .collect()
}

/// The rows of `connected_transitions`, each cell the directions it
/// connects and its colour.
fn route<'a>(edges: &[(usize, usize, &'a str)], width: usize) -> Vec<Vec<(u8, &'a str)>> {
    const UP: u8 = 1;
    const DOWN: u8 = 2;
    const LEFT: u8 = 4;
    const RIGHT: u8 = 8;
    let mut rows = Vec::new();
    route_moves(edges, width, |tracks, step| {
        let Move {
            from,
            to,
            colour,
            continues,
            occupied,
        } = step;
        let size = width.max(1) * 2;
        let size = size
            .max(to + 2)
            .max(tracks.last_key_value().map_or(0, |(&column, _)| column + 2));
        let mut cells = vec![(0, ""); size];
        for (&column, routes) in tracks {
            cells[column] = (UP | DOWN, routes[0].1);
        }
        let (outgoing, incoming) = if from < to {
            (RIGHT, LEFT)
        } else {
            (LEFT, RIGHT)
        };
        cells[from] = (UP | if continues { DOWN } else { 0 } | outgoing, colour);
        cells[to] = (DOWN | if occupied { UP } else { 0 } | incoming, colour);
        for cell in &mut cells[from.min(to) + 1..from.max(to)] {
            cell.0 |= LEFT | RIGHT;
            if cell.1.is_empty() {
                cell.1 = colour;
            }
        }
        let used = cells
            .iter()
            .rposition(|&(directions, _)| directions != 0)
            .map_or(0, |i| i + 1);
        cells.truncate(used);
        rows.push(cells);
    });
    rows
}

/// The number of rows `route` draws, without drawing them.
fn route_count(edges: &[(usize, usize, &str)], width: usize) -> usize {
    let mut rows = 0;
    route_moves(edges, width, |_, _| rows += 1);
    rows
}

/// Each track's routes: their target column and colour.
type Tracks<'a> = std::collections::BTreeMap<usize, Vec<(usize, &'a str)>>;

/// A connection `route` moves in one row.
struct Move<'a> {
    from: usize,
    to: usize,
    colour: &'a str,
    // Other routes continue down from `from`.
    continues: bool,
    // Other routes already pass through `to`.
    occupied: bool,
}

/// Visit the moves `route` draws, one per row, each with the tracks before it.
fn route_moves<'a>(
    edges: &[(usize, usize, &'a str)],
    width: usize,
    mut visit: impl FnMut(&Tracks<'a>, Move<'a>),
) {
    if edges.iter().all(|&(from, to, _)| from == to) {
        return;
    }
    let mut tracks = Tracks::new();
    for &(from, to, colour) in edges {
        let routes = tracks.entry(from).or_default();
        if !routes.iter().any(|&(target, _)| target == to) {
            routes.push((to, colour));
        }
    }
    loop {
        let moving = || {
            tracks.iter().flat_map(|(&from, routes)| {
                routes
                    .iter()
                    .filter_map(move |&(to, colour)| (from != to).then_some((from, to, colour)))
            })
        };
        let Some(first) = moving().next() else {
            break;
        };
        let (from, target, colour, to) = moving()
            .find_map(|(from, to, colour)| {
                tracks
                    .get(&to)
                    .is_none_or(|routes| routes.iter().all(|&(target, _)| target == to))
                    .then_some((from, to, colour, to))
            })
            .unwrap_or_else(|| {
                let (from, target, colour) = first;
                let spare = width.max(1) * 2;
                let spare = spare.max(tracks.last_key_value().map_or(0, |(&column, _)| column + 2));
                (from, target, colour, spare)
            });
        // A fork and a join in the same row would have downward paths at
        // both endpoints, visually inventing a connection in the reverse
        // direction. Split off this route before joining an occupied lane.
        let to = if tracks.contains_key(&to) && tracks[&from].len() > 1 {
            (width.max(1) * 2).max(tracks.last_key_value().map_or(0, |(&column, _)| column + 2))
        } else {
            to
        };
        // Each track holds one route per target.
        let continues = tracks[&from].len() > 1;
        let occupied = tracks.contains_key(&to);
        visit(
            &tracks,
            Move {
                from,
                to,
                colour,
                continues,
                occupied,
            },
        );
        if continues {
            let routes = tracks.get_mut(&from).unwrap();
            routes.retain(|&(destination, _)| destination != target);
        } else {
            tracks.remove(&from);
        }
        let routes = tracks.entry(to).or_default();
        if !routes.iter().any(|&(destination, _)| destination == target) {
            routes.push((target, colour));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    fn plain(rows: &[String]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                crate::ansi::parse_line(row)
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    fn commit(hash: &str, parents: &[&str]) -> Commit {
        let mut commit = crate::log_format::tests::commit();
        commit.hash = hash.into();
        commit.decorations.clear();
        commit.parents = parents.iter().map(|p| (*p).into()).collect();
        commit.graph = vec![format!("original graph {hash}")];
        commit
    }

    /// Folds whose omitted history comes from `ancestry`, failing when unset.
    fn folds_with(ancestry: Option<Ancestry>) -> LogFolds {
        LogFolds {
            load_ancestry: Box::new(move |_| ancestry.clone().ok_or_else(|| "no Git".to_owned())),
            ..LogFolds::default()
        }
    }

    fn shown<'a>(folds: &LogFolds, commits: &'a [Commit]) -> Vec<&'a str> {
        (0..commits.len())
            .filter(|&i| folds.visible(i))
            .map(|i| commits[i].hash.as_str())
            .collect()
    }

    /// The history of `git log --graph --color=always` over two merges,
    /// each commit with the graph rows Git drew for it.
    fn coloured_history() -> Vec<Commit> {
        let r = |c: char| format!("\x1b[31m{c}\x1b[m");
        let g = |c: char| format!("\x1b[32m{c}\x1b[m");
        let b = |c: char| format!("\x1b[34m{c}\x1b[m");
        let rows = [
            ("merge2", &["m2", "t1"][..], vec!["*   ".to_owned()]),
            (
                "t1",
                &["m1"],
                vec![format!("{}{}  ", r('|'), g('\\')), format!("{} * ", r('|'))],
            ),
            ("m2", &["merge1"], vec![format!("* {} ", g('|'))]),
            ("merge1", &["m1", "s1"], vec![format!("* {}   ", g('|'))]),
            (
                "s1",
                &["base"],
                vec![
                    format!("{}{} {}  ", g('|'), b('\\'), g('\\')),
                    format!("{} {}{}  ", g('|'), b('|'), g('/')),
                    format!("{}{}{}   ", g('|'), g('/'), b('|')),
                    format!("{} * ", g('|')),
                ],
            ),
            ("m1", &["base"], vec![format!("* {} ", b('|'))]),
            (
                "base",
                &[],
                vec![format!("{}{}  ", b('|'), b('/')), "* ".to_owned()],
            ),
        ];
        rows.into_iter()
            .map(|(hash, parents, graph)| {
                let mut commit = commit(hash, parents);
                commit.graph = graph;
                commit
            })
            .collect()
    }

    /// The colour of the edge Git draws into each row's node, when one
    /// ends there, following the rows in order.
    fn incoming_colours(graphs: &[&[String]]) -> Vec<Option<Style>> {
        let cells = |line: &str| -> Vec<(char, Style)> {
            crate::ansi::parse_line(line)
                .spans
                .iter()
                .flat_map(|span| span.content.chars().map(|c| (c, span.style)))
                .collect()
        };
        let mut above: Vec<(char, Style)> = Vec::new();
        let mut colours = Vec::new();
        for rows in graphs {
            let mut colour = None;
            for line in rows.iter() {
                let line = cells(line);
                if let Some(node) = line.iter().position(|&(c, _)| c == '*') {
                    let edge = |at: Option<usize>, symbol| {
                        at.and_then(|at| above.get(at))
                            .filter(|&&(c, _)| c == symbol)
                            .map(|&(_, style)| style)
                    };
                    colour = edge(Some(node), '|')
                        .or_else(|| edge(node.checked_sub(1), '\\'))
                        .or_else(|| edge(Some(node + 1), '/'));
                }
                above = line;
            }
            colours.push(colour);
        }
        colours
    }

    #[test]
    fn redrawn_lanes_keep_git_colours() {
        let commits = coloured_history();
        let git = incoming_colours(&commits.iter().map(|c| &c.graph[..]).collect::<Vec<_>>());
        // merge2 is the first row and m2 leads straight into merge1.
        assert_eq!(
            git.iter().map(Option::is_some).collect::<Vec<_>>(),
            [false, true, true, false, true, true, true]
        );
        for hide_merges in [false, true] {
            let mut folds = LogFolds::default();
            folds.refresh(&commits).unwrap();
            if hide_merges {
                folds.toggle_merges(&commits).unwrap();
            } else {
                // A filter matching nothing still redraws the graph.
                folds.toggle_type(Some("nonexistent"), &commits).unwrap();
            }
            let shown: Vec<_> = (0..commits.len())
                .filter(|&i| folds.graph_visible(i))
                .collect();
            let graphs: Vec<_> = shown.iter().map(|&i| folds.graph(i, &commits[i])).collect();
            for (&i, colour) in shown.iter().zip(incoming_colours(&graphs)) {
                if folds.visible(i) && git[i].is_some() {
                    assert_eq!(
                        colour, git[i],
                        "{} (hide merges: {hide_merges})",
                        commits[i].hash
                    );
                }
            }
        }
    }

    #[test]
    fn lanes_keep_git_colours_into_marked_nodes() {
        let commits = coloured_history();
        // As with --left-right, --cherry-mark and --boundary.
        for marker in ['<', '>', '=', '+', 'o'] {
            let marked: Vec<_> = commits
                .iter()
                .map(|commit| Commit {
                    graph_node: marker,
                    graph: commit
                        .graph
                        .iter()
                        .map(|row| row.replace('*', &marker.to_string()))
                        .collect(),
                    ..commit.clone()
                })
                .collect();
            for row in 0..commits.len() {
                assert_eq!(
                    incoming_colour(&marked, row),
                    incoming_colour(&commits, row),
                    "{marker} {}",
                    commits[row].hash
                );
            }
        }
    }

    #[test]
    fn conventional_types_require_a_subject_prefix() {
        for (subject, expected) in [
            ("feat: add something", Some("feat")),
            ("FIX: repair", Some("fix")),
            ("Fix(api)!: repair", Some("fix")),
            ("DoC(manual): guide", Some("docs")),
            ("TESTS #234(failing): reproduce", Some("test")),
            ("Custom-Type: thing", Some("custom-type")),
            ("feature(api)!: add something", Some("feat")),
            ("doc #290(manual): list Bypass", Some("docs")),
            ("docs: update guide", Some("docs")),
            ("tests #234(failing): reproduce bug", Some("test")),
            ("testing: custom type", Some("testing")),
            ("features: custom type", Some("features")),
            ("test(failing): reproduce bug", Some("test")),
            ("refactor(core)!: new API", Some("refactor")),
            ("feat!: breaking", Some("feat")),
            ("custom-type: thing", Some("custom-type")),
            ("test #234(failing): reproduce", Some("test")),
            ("docs #290(manual): list Bypass", Some("docs")),
            ("fix #233 #234: editor", Some("fix")),
            ("feat #42(api)!: breaking", Some("feat")),
            ("test #234: coverage", Some("test")),
            ("test #: missing number", None),
            ("test #abc: nonnumeric", None),
            ("test #234oops: malformed", None),
            ("test something: prose", None),
            ("ordinary subject", None),
            ("Fix typo: description", None),
            ("test(): empty scope", None),
            ("feat: ", None),
            ("feat(scope: malformed", None),
            ("subject\n\nrefactor: body", None),
        ] {
            let mut c = commit("a", &[]);
            c.subject = subject.into();
            assert_eq!(commit_type(&c).as_deref(), expected, "{subject}");
            c.kind = CommitKind::WorkingTree;
            assert_eq!(commit_type(&c), None);
        }
    }

    #[test]
    fn type_filter_preserves_refs_in_linear_history() {
        for decoration in ["main", "origin/main", "tag: v1.0", "HEAD"] {
            let mut commits = vec![
                commit("feature", &["intermediate"]),
                commit("intermediate", &["main"]),
                commit("main", &["base"]),
                commit("base", &[]),
            ];
            for commit in &mut commits {
                commit.subject = "test: context".into();
            }
            commits[0].decorations = "HEAD -> feature".into();
            commits[2].decorations = decoration.into();
            let mut folds = folds_with(None);
            folds.refresh(&commits).unwrap();
            folds.toggle_type(Some("test"), &commits).unwrap();
            assert_eq!(shown(&folds, &commits), ["feature", "main"]);
            assert_eq!(folds.hidden_count, 2);
            assert!(!folds.filter_hidden(2, &commits[2]));

            // Watch refresh must follow a moved ref, without retaining its old row.
            commits[2].decorations.clear();
            commits[1].decorations = decoration.into();
            folds.refresh(&commits).unwrap();
            assert_eq!(shown(&folds, &commits), ["feature", "intermediate"]);
            assert_eq!(folds.hidden_count, 2);
        }
    }

    #[test]
    fn type_filter_preserves_branch_points_through_omitted_history() {
        for omitted in [false, true] {
            let mut commits = vec![
                commit("left", &[if omitted { "omitted-left" } else { "fork" }]),
                commit("right", &["fork"]),
                commit("fork", &["base"]),
                commit("base", &[]),
            ];
            for commit in &mut commits {
                commit.subject = "test: context".into();
            }
            let ancestry = HashMap::from([("omitted-left".into(), vec!["fork".into()])]);
            let mut folds = folds_with(Some(ancestry));
            folds.refresh(&commits).unwrap();
            folds.toggle_type(Some("test"), &commits).unwrap();
            assert_eq!(shown(&folds, &commits), ["fork"]);
            assert_eq!(folds.hidden_count, 3);
            assert!(!folds.filter_hidden(2, &commits[2]));

            // A refreshed linear history no longer has a branch point.
            commits.remove(1);
            folds.refresh(&commits).unwrap();
            assert!(shown(&folds, &commits).is_empty());
            assert_eq!(folds.hidden_count, 3);
        }
    }

    #[test]
    fn type_filter_retains_hidden_merge_junctions_and_restores_graph() {
        let mut commits = vec![
            commit("tip", &["merge"]),
            commit("merge", &["main", "side"]),
            commit("side", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ];
        commits[1].subject = "refactor: merge".into();
        let mut folds = folds_with(None);
        folds.refresh(&commits).unwrap();
        folds.toggle_type(Some("refactor"), &commits).unwrap();
        assert_eq!(shown(&folds, &commits), ["tip", "side", "main", "base"]);
        assert!(!folds.visible(1));
        assert!(plain(folds.graph(1, &commits[1]))
            .iter()
            .all(|row| !row.contains('*')));
        // The hidden merge's two parents remain connected to its junction.
        let side = plain(folds.graph(2, &commits[2]));
        assert!(
            side.iter()
                .any(|row| row.contains('|') && row.contains('*')),
            "{side:?}"
        );
        folds.toggle_type(None, &commits).unwrap();
        for (i, c) in commits.iter().enumerate() {
            assert!(folds.visible(i));
            assert_eq!(folds.graph(i, c), c.graph);
        }
    }

    #[test]
    fn type_filters_and_merge_folds_are_independent() {
        let mut commits = vec![
            commit("merge", &["main", "side"]),
            commit("side", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ];
        commits[1].subject = "test: side".into();
        commits[2].subject = "test: main".into();
        let mut folds = folds_with(None);
        folds.refresh(&commits).unwrap();
        folds.toggle(0, &commits).unwrap();
        folds.toggle_type(Some("test"), &commits).unwrap();
        assert_eq!(shown(&folds, &commits), ["merge", "base"]);
        assert_eq!(folds.hidden_count, 2);
        folds.toggle_type(None, &commits).unwrap();
        assert_eq!(shown(&folds, &commits), ["merge", "main", "base"]);
        assert_eq!(folds.hidden_count, 0);
        folds.toggle_type(Some("test"), &commits).unwrap();
        folds.toggle(0, &commits).unwrap();
        assert_eq!(shown(&folds, &commits), ["merge", "base"]);
        assert_eq!(folds.hidden_count, 2);
    }

    #[test]
    fn folded_merge_reconnects_interleaved_lanes_and_restores_original_graph() {
        let commits = vec![
            commit("merge", &["main", "side"]),
            commit("unrelated", &["base"]),
            commit("side", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        folds.toggle(0, &commits).unwrap();
        assert!(!folds.visible(2));
        assert_eq!(plain(folds.graph(0, &commits[0])), ["* "]);
        assert_eq!(plain(folds.graph(1, &commits[1])), ["| * "]);
        assert_eq!(plain(folds.graph(3, &commits[3])), ["* | "]);
        assert_eq!(plain(folds.graph(4, &commits[4])), ["|/ ", "* "]);
        folds.toggle(0, &commits).unwrap();
        for (i, commit) in commits.iter().enumerate() {
            assert!(folds.visible(i));
            assert_eq!(folds.graph(i, commit), commit.graph);
        }
    }

    #[test]
    fn revealing_shared_history_opens_its_nearest_fold_but_keeps_other_folds() {
        let commits = vec![
            commit("one", &["base", "shared"]),
            commit("two", &["base", "shared"]),
            commit("three", &["base", "other"]),
            commit("shared", &["base"]),
            commit("other", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        // Folding one merge alone cannot hide what the other still shows.
        assert_eq!(folds.marker(0), None);
        let error = folds.toggle(0, &commits).unwrap_err();
        assert!(error.contains("other shown history"), "{error}");
        folds.toggle_all(0, &commits).unwrap();
        assert_eq!(shown(&folds, &commits), ["one", "two", "three", "base"]);
        folds.reveal(3, &commits);
        assert_eq!(
            shown(&folds, &commits),
            ["one", "two", "three", "shared", "base"]
        );
        assert_eq!(folds.marker(2), Some("▶"));
    }

    #[test]
    fn shared_merge_folds_count_the_whole_hidden_topic() {
        let commits = vec![
            commit("one", &["base", "s3"]),
            commit("two", &["base", "s3"]),
            commit("s3", &["s2"]),
            commit("s2", &["s1"]),
            commit("s1", &["base"]),
            commit("base", &[]),
        ];
        for merge in [0, 1] {
            let mut folds = LogFolds::default();
            folds.refresh(&commits).unwrap();
            folds.toggle_all(merge, &commits).unwrap();
            assert_eq!(shown(&folds, &commits), ["one", "two", "base"]);
            assert_eq!(folds.label(merge), " · 3 merged commits");
            folds.toggle(merge, &commits).unwrap();
            assert!((0..commits.len()).all(|row| folds.visible(row)));
        }
    }

    #[test]
    fn loaded_side_history_shows_a_disclosure_marker() {
        let commits = vec![
            commit("merge", &["main", "side"]),
            commit("side", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        assert_eq!(folds.marker(0), Some("▼"));
    }

    #[test]
    fn a_fold_hiding_nothing_is_never_shown_as_folded() {
        // The topic continued after it was merged, and its newer commit
        // still shows everything the merge brought in.
        let commits = vec![
            commit("continued", &["side"]),
            commit("merge", &["main", "side"]),
            commit("main", &["base"]),
            commit("side", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds {
            start_collapsed: true,
            ..LogFolds::default()
        };
        folds.refresh(&commits).unwrap();
        assert_eq!(folds.marker(1), None);
        assert_eq!(folds.label(1), "");
        assert!(folds.toggle(1, &commits).is_err());
        assert!(folds.toggle_all(1, &commits).is_err());
        assert!((0..commits.len()).all(|i| folds.visible(i)));
    }

    #[test]
    fn folded_graph_colours_lanes_like_git() {
        let commits = vec![
            commit("merge", &["main", "side"]),
            commit("unrelated", &["base"]),
            commit("side", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        folds.toggle(0, &commits).unwrap();
        let row = &folds.graph(1, &commits[1])[0];
        assert!(row.contains("\x1b[3"), "{row:?}");
    }

    #[test]
    fn folding_all_without_foldable_merges_explains_why() {
        let commits = vec![commit("one", &["base"]), commit("base", &[])];
        assert!(LogFolds::default().toggle_all(0, &commits).is_err());
    }

    #[test]
    fn first_parent_history_offers_no_folds_without_an_ancestry_walk() {
        // As in a --first-parent log, side parents are never loaded and each
        // row below a merge is its mainline history.
        let commits = vec![
            commit("two", &["one", "side two"]),
            commit("one", &["base", "side one"]),
            commit("base", &[]),
        ];
        let mut folds = folds_with(None);
        folds.start_collapsed = true;
        folds.refresh(&commits).unwrap();
        assert!((0..commits.len()).all(|i| folds.marker(i).is_none()));
        let error = folds.toggle_all(0, &commits).unwrap_err();
        assert!(error.contains("No foldable merges"), "{error}");
    }

    #[test]
    fn nonmatching_type_filter_does_not_restore_an_omitted_merge_junction() {
        // Git omitted the merge (for example, with --grep), but both of its
        // parents are loaded. Redrawing must keep the first-parent projection
        // through omitted ancestry instead of adding a split below tip.
        let commits = vec![
            commit("tip", &["omitted merge"]),
            commit("main", &["base"]),
            commit("side", &["base"]),
            commit("base", &[]),
        ];
        let ancestry =
            Ancestry::from([("omitted merge".into(), vec!["main".into(), "side".into()])]);
        let mut folds = folds_with(Some(ancestry));
        folds.refresh(&commits).unwrap();
        folds.toggle_type(Some("nonexistent"), &commits).unwrap();

        assert_eq!(folds.hidden_count, 0);
        assert_eq!(shown(&folds, &commits), ["tip", "main", "side", "base"]);
        assert_eq!(plain(folds.graph(1, &commits[1])), ["* "]);
        assert_eq!(plain(folds.graph(2, &commits[2])), ["| * "]);
    }

    #[test]
    fn filtered_out_side_parents_are_not_resolved_by_bulk_folding() {
        // As in a --grep log, the side tip is omitted but an older side commit
        // matched. Only folding this merge on request reads the ancestry.
        let commits = vec![
            commit("merge", &["main", "omitted tip"]),
            commit("older side", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ];
        let ancestry = Ancestry::from([("omitted tip".into(), vec!["older side".into()])]);
        let mut folds = LogFolds {
            start_collapsed: true,
            ..folds_with(None)
        };
        folds.refresh(&commits).unwrap();
        assert_eq!(folds.marker(0), None);
        assert!(folds.visible(1));
        let error = folds.toggle_all(0, &commits).unwrap_err();
        assert!(error.contains("No foldable merges"), "{error}");
        folds.load_ancestry = folds_with(Some(ancestry)).load_ancestry;
        folds.toggle(0, &commits).unwrap();
        assert_eq!(shown(&folds, &commits), ["merge", "main", "base"]);
        assert_eq!(folds.hidden_count, 0);
        assert_eq!(folds.label(0), " · 1 merged commit");
    }

    #[test]
    fn new_commits_on_top_keep_folds_and_ancestry() {
        let commits = vec![
            commit("merge", &["main", "omitted"]),
            commit("side", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ];
        let ancestry = Ancestry::from([("omitted".into(), vec!["side".into()])]);
        let mut folds = folds_with(Some(ancestry));
        folds.refresh(&commits).unwrap();
        folds.toggle(0, &commits).unwrap();
        // An unchanged history keeps its ancestry, so this needs no Git.
        folds.load_ancestry = folds_with(None).load_ancestry;
        folds.refresh(&commits).unwrap();
        assert!(!folds.visible(1));
        let mut refreshed = vec![commit("new", &["merge"])];
        refreshed.extend(commits);
        // Changed history must be read again.
        assert!(folds.refresh(&refreshed).is_err());
        assert!((0..refreshed.len()).all(|i| folds.visible(i)));
    }

    #[test]
    fn side_parent_in_mainline_history_shows_no_disclosure_marker() {
        // As with rewritten parents under --full-history -- path, the loaded
        // side parent is also an ancestor of the first parent.
        let commits = vec![
            commit("merge", &["main", "old"]),
            commit("main", &["old"]),
            commit("unrelated", &["base"]),
            commit("old", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        assert_eq!(folds.marker(0), None);
        let error = folds.toggle(0, &commits).unwrap_err();
        assert!(error.contains("No merged commits"), "{error}");
    }

    #[test]
    fn folding_after_a_failed_refresh_folds_on_the_first_press() {
        let commits = vec![
            commit("merge", &["main", "side"]),
            commit("side", &["base"]),
            commit("main", &["filtered"]),
            commit("filtered", &["base", "omitted"]),
            commit("base", &[]),
        ];
        let ancestry = Ancestry::from([("omitted".into(), vec!["base".into()])]);
        let mut folds = folds_with(Some(ancestry.clone()));
        folds.refresh(&commits).unwrap();
        folds.toggle(0, &commits).unwrap();
        folds.load_ancestry = folds_with(None).load_ancestry;
        folds.revisions.clear();
        assert!(folds.refresh(&commits).is_err());
        // The failure leaves the Log unfolded, so z must fold what is shown.
        assert!(folds.visible(1));
        assert_eq!(folds.marker(0), Some("▼"));
        folds.load_ancestry = folds_with(Some(ancestry)).load_ancestry;
        folds.toggle(0, &commits).unwrap();
        assert!(!folds.visible(1));
        assert_eq!(folds.marker(0), Some("▶"));
    }

    #[test]
    fn filters_after_a_failed_refresh_draw_the_unfolded_log() {
        let commits = vec![
            commit("merge", &["main", "side"]),
            commit("side", &["base"]),
            commit("main", &["filtered"]),
            commit("filtered", &["base", "omitted"]),
            commit("base", &[]),
        ];
        let ancestry = Ancestry::from([("omitted".into(), vec!["base".into()])]);
        let mut folds = folds_with(Some(ancestry));
        folds.refresh(&commits).unwrap();
        folds.toggle(0, &commits).unwrap();
        folds.load_ancestry = folds_with(None).load_ancestry;
        folds.revisions.clear();
        assert!(folds.refresh(&commits).is_err());
        // The saved fold must not be drawn while the Log shows its side.
        assert!(folds.toggle_type(Some("nonexistent"), &commits).is_err());
        let mut expected = folds_with(None);
        expected.refresh(&commits).unwrap();
        assert!(expected.toggle_type(Some("nonexistent"), &commits).is_err());
        assert_same_layout(&folds, &expected, &commits);
    }

    #[test]
    fn nested_folds_keep_mainline_that_a_topic_merged_back() {
        // The topic merged master's m1 before master merged the topic, so
        // m1 is merged by x but is mainline history of the outer merge.
        let commits = vec![
            commit("outer", &["m2", "t2"]),
            commit("t2", &["x"]),
            commit("x", &["t1", "m1"]),
            commit("t1", &["base"]),
            commit("m2", &["m1"]),
            commit("m1", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        folds.toggle_all(0, &commits).unwrap();
        assert_eq!(shown(&folds, &commits), ["outer", "m2", "m1", "base"]);
        assert_eq!(folds.label(0), " · 3 merged commits");
        // Expanded, the outer merge shows x, whose merged m1 is still shown.
        folds.toggle(0, &commits).unwrap();
        assert!((0..commits.len()).all(|i| folds.visible(i)));
        assert_eq!(folds.marker(2), None);
    }

    fn assert_same_layout(folds: &LogFolds, expected: &LogFolds, commits: &[Commit]) {
        for (i, commit) in commits.iter().enumerate() {
            assert_eq!(folds.visible(i), expected.visible(i), "row {i}");
            assert_eq!(folds.marker(i), expected.marker(i), "row {i}");
            assert_eq!(folds.label(i), expected.label(i), "row {i}");
            assert_eq!(
                plain(folds.graph(i, commit)),
                plain(expected.graph(i, commit)),
                "row {i}"
            );
        }
    }

    #[test]
    fn filters_preserve_connections_of_a_fold_that_hides_nothing() {
        let commits = vec![
            commit("m4", &["m3", "o"]),
            commit("o", &["m3"]),
            commit("m3", &["m2", "t2"]),
            commit("t2", &["sync"]),
            commit("sync", &["t1", "m1"]),
            commit("m2", &["m1"]),
            commit("t1", &["base"]),
            commit("m1", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        folds.toggle_all(0, &commits).unwrap();
        folds.toggle(2, &commits).unwrap();
        // Expanding m3 leaves sync's saved fold with nothing to hide.
        let mut expected = LogFolds::default();
        expected.refresh(&commits).unwrap();
        expected.toggle(0, &commits).unwrap();
        assert_same_layout(&folds, &expected, &commits);

        // Even a filter matching no commits redraws the graph. It must
        // preserve sync's connection to m1, just like an unfolded merge.
        folds.toggle_type(Some("nonexistent"), &commits).unwrap();
        expected.toggle_type(Some("nonexistent"), &commits).unwrap();
        assert_eq!(folds.hidden_count, 0);
        assert_same_layout(&folds, &expected, &commits);

        folds.toggle_merges(&commits).unwrap();
        expected.toggle_merges(&commits).unwrap();
        assert_same_layout(&folds, &expected, &commits);
        folds.toggle_type(None, &commits).unwrap();
        expected.toggle_type(None, &commits).unwrap();
        assert_same_layout(&folds, &expected, &commits);
    }

    #[test]
    fn a_fold_that_hides_nothing_draws_like_an_unfolded_merge() {
        // The topic merged m1 back in with sync. m folds sync inside m3, but
        // once m3 is expanded, sync's fold hides nothing.
        let commits = vec![
            commit("m4", &["m3", "o"]),
            commit("o", &["m3"]),
            commit("m3", &["m2", "t2"]),
            commit("t2", &["sync"]),
            commit("sync", &["t1", "m1"]),
            commit("m2", &["m1"]),
            commit("t1", &["base"]),
            commit("m1", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        folds.toggle_all(0, &commits).unwrap();
        folds.toggle(2, &commits).unwrap();
        let mut expected = LogFolds::default();
        expected.refresh(&commits).unwrap();
        expected.toggle(0, &commits).unwrap();
        assert_same_layout(&folds, &expected, &commits);

        // A branch that continues a folded topic leaves m1 nothing to hide.
        let before = vec![
            commit("m2", &["m1", "s2"]),
            commit("s2", &["m1"]),
            commit("m1", &["main", "side"]),
            commit("side", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds.refresh(&before).unwrap();
        folds.toggle_all(0, &before).unwrap();
        let mut after = vec![commit("continued", &["side"])];
        after.extend(before);
        folds.refresh(&after).unwrap();
        let mut expected = LogFolds::default();
        expected.refresh(&after).unwrap();
        expected.toggle(1, &after).unwrap();
        assert_same_layout(&folds, &expected, &after);
    }

    #[test]
    fn ancestry_keeps_only_omitted_commits_that_lead_to_rows() {
        // As in a count-limited --all log, an old branch's long omitted
        // history leads to no row, so it must not grow every layout.
        let commits = vec![
            commit("merge", &["main", "omitted side"]),
            commit("old tip", &["old 0"]),
            commit("side", &["base"]),
            commit("main", &["base"]),
            commit("base", &["older"]),
        ];
        let mut ancestry: Ancestry = (0..1000)
            .map(|i| (format!("old {i}"), vec![format!("old {}", i + 1)]))
            .collect();
        ancestry.insert("omitted side".into(), vec!["side".into()]);
        let mut folds = folds_with(Some(ancestry));
        folds.refresh(&commits).unwrap();
        folds.toggle(0, &commits).unwrap();
        assert_eq!(
            shown(&folds, &commits),
            ["merge", "old tip", "main", "base"]
        );
        assert_eq!(folds.graph.parents.len(), commits.len() + 1);
        folds.refresh(&commits).unwrap();
        assert_eq!(folds.graph.parents.len(), commits.len() + 1);
    }

    #[test]
    fn lines_move_one_lane_per_row_like_git() {
        assert_eq!(
            plain(&transitions(&[(0, 0, "31"), (0, 2, "32")], 2)),
            ["|\\ "]
        );
        assert_eq!(
            plain(&transitions(&[(0, 0, "31"), (2, 0, "32")], 2)),
            ["|/ "]
        );
        assert_eq!(
            plain(&transitions(&[(0, 0, "31"), (4, 0, "32")], 3)),
            ["|  / ", "|/ "]
        );
    }

    #[test]
    fn crossing_routes_do_not_turn_into_a_shared_parent() {
        assert_eq!(
            plain(&transitions(&[(0, 2, "31"), (2, 0, "32")], 2)),
            [" X "]
        );
    }

    #[test]
    fn connected_routes_keep_forks_joins_and_crossings_distinct() {
        assert_eq!(
            plain(&connected_transitions(&[(0, 0, "31"), (0, 2, "32")], 2)),
            ["├─╮ "]
        );
        assert_eq!(
            plain(&connected_transitions(&[(0, 0, "31"), (2, 0, "31")], 2)),
            ["├─╯ "]
        );
        assert_eq!(
            plain(&connected_transitions(&[(0, 4, "31"), (2, 2, "32")], 3)),
            ["╰─╪─╮ "]
        );
        assert_eq!(
            plain(&connected_transitions(&[(0, 2, "31"), (2, 0, "32")], 2)),
            ["╰─╪─╮ ", "╭─╯ │ ", "│ ╭─╯ "]
        );
        // Both source lanes also continue straight: moving the fork to a
        // temporary lane must leave the original connection intact.
        assert_eq!(
            plain(&connected_transitions(
                &[(0, 0, "31"), (0, 2, "32"), (2, 2, "32"), (2, 0, "31")],
                2
            )),
            ["├─╪─╮ ", "│ ├─╪─╮ ", "│ ├─╯ │ ", "├─╪───╯ "]
        );
    }

    #[test]
    fn connected_routes_reach_exactly_their_original_destinations() {
        // Trace the rendered characters rather than the router's bookkeeping.
        // At a crossing, horizontal and vertical paths remain independent.
        let mut seed = 19_u32;
        for case in 0..300 {
            let width = 2 + case % 6;
            let mut edges = Vec::new();
            for from in 0..width {
                for to in 0..width {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    if (seed >> 16).is_multiple_of(4) {
                        edges.push((2 * from, 2 * to, "31"));
                    }
                }
            }
            let rows: Vec<Vec<char>> = plain(&connected_transitions(&edges, width))
                .iter()
                .map(|row| row.chars().collect())
                .collect();
            for from in 0..width {
                let expected: HashSet<_> = edges
                    .iter()
                    .filter_map(|&(source, to, _)| (source == 2 * from).then_some(to))
                    .collect();
                if expected.is_empty() {
                    continue;
                }
                // Entries identify the side through which a path entered a cell.
                let mut todo = vec![(2 * from, 0, 'u')];
                let mut seen = HashSet::new();
                let mut actual = HashSet::new();
                while let Some((x, y, entry)) = todo.pop() {
                    if y == rows.len() {
                        actual.insert(x);
                        continue;
                    }
                    if !seen.insert((x, y, entry)) {
                        continue;
                    }
                    let glyph = rows[y].get(x).copied().unwrap_or(' ');
                    let (up, down, left, right) = match glyph {
                        '│' => (true, true, false, false),
                        '─' => (false, false, true, true),
                        '╰' => (true, false, false, true),
                        '╯' => (true, false, true, false),
                        '╭' => (false, true, false, true),
                        '╮' => (false, true, true, false),
                        '├' => (true, true, false, true),
                        '┤' => (true, true, true, false),
                        '╪' => (true, true, true, true),
                        _ => panic!("broken route in case {case}: {edges:?}, {rows:?}"),
                    };
                    assert!(match entry {
                        'u' => up,
                        'l' => left,
                        'r' => right,
                        _ => false,
                    });
                    if down && (glyph != '╪' || entry == 'u') {
                        todo.push((x, y + 1, 'u'));
                    }
                    if left && entry != 'l' && (glyph != '╪' || entry == 'r') {
                        todo.push((x - 1, y, 'r'));
                    }
                    if right && entry != 'r' && (glyph != '╪' || entry == 'l') {
                        todo.push((x + 1, y, 'l'));
                    }
                }
                assert_eq!(actual, expected, "case {case}: {edges:?}, {rows:?}");
            }
        }
    }

    #[test]
    fn lazily_drawn_graph_blocks_continue_the_lane_assignment() {
        // Merges spanning several blocks of lazily drawn rows.
        let count = 3 * BLOCK + 10;
        let hash = |i: usize| format!("c{i}");
        let commits: Vec<_> = (0..count)
            .map(|i| {
                let parents: Vec<_> = [i + 1, i + 7]
                    .into_iter()
                    .take(if i % 5 == 0 { 2 } else { 1 })
                    .filter(|&parent| parent < count)
                    .map(hash)
                    .collect();
                commit(
                    &hash(i),
                    &parents.iter().map(String::as_str).collect::<Vec<_>>(),
                )
            })
            .collect();
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        let plan = &folds.layout.graphs.0;
        for unicode in [false, true] {
            let lanes = plan.lanes(unicode);
            assert_eq!(lanes.checkpoints.len(), 4);
            for (block, checkpoint) in lanes.checkpoints.iter().enumerate().skip(1) {
                let mut state = lanes.checkpoints[block - 1].clone();
                for index in (block - 1) * BLOCK..block * BLOCK {
                    plan.step(&mut state, index, unicode);
                }
                assert!(
                    state == *checkpoint,
                    "checkpoint {block} (unicode: {unicode})"
                );
            }
            folds.ascii_graph = !unicode;
            for (i, commit) in commits.iter().enumerate() {
                let rows = folds.display_graph(i, commit).len();
                assert_eq!(rows, folds.display_height(i, commit));
            }
        }
    }

    #[test]
    fn unicode_graph_restores_its_paths_after_folding_and_filtering() {
        let commits = vec![
            commit("merge", &["main", "side"]),
            commit("side", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        let original: Vec<_> = commits
            .iter()
            .enumerate()
            .map(|(i, commit)| plain(folds.display_graph(i, commit)))
            .collect();
        assert_eq!(
            original,
            [
                vec!["● "],
                vec!["├─╮ ", "│ ● "],
                vec!["● │ "],
                vec!["├─╯ ", "● "]
            ]
        );
        folds.toggle(0, &commits).unwrap();
        assert!(!folds.visible(1));
        assert_eq!(plain(folds.display_graph(2, &commits[2])), ["● "]);
        folds.toggle(0, &commits).unwrap();
        folds.toggle_merges(&commits).unwrap();
        assert_eq!(plain(folds.display_graph(1, &commits[1])), original[1]);
        folds.toggle_merges(&commits).unwrap();
        for (i, commit) in commits.iter().enumerate() {
            assert_eq!(plain(folds.display_graph(i, commit)), original[i]);
        }
        folds.ascii_graph = true;
        for (i, commit) in commits.iter().enumerate() {
            assert_eq!(folds.display_graph(i, commit), commit.graph);
        }
    }

    #[test]
    fn unicode_lanes_keep_their_columns_when_a_lane_ends() {
        let commits = vec![
            commit("x", &["base"]),
            commit("y", &["b"]),
            commit("z", &["c"]),
            commit("b", &["base"]),
            commit("c", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        let graph = |folds: &LogFolds| -> Vec<_> {
            (0..commits.len())
                .map(|i| plain(folds.display_graph(i, &commits[i])))
                .collect()
        };
        assert_eq!(
            graph(&folds)[3..],
            [
                vec!["│ ● │ "],
                vec!["├─╯ │ ", "│   ● "],
                vec!["├───╯ ", "● "]
            ]
        );
        // ASCII lanes close up like Git's.
        folds.ascii_graph = true;
        folds.refresh(&commits).unwrap();
        folds.toggle_merges(&commits).unwrap();
        assert_eq!(graph(&folds)[4], ["|/ / ", "| * "]);
    }

    #[test]
    fn unicode_graph_connects_history_omitted_by_git_filters() {
        let commits = vec![
            commit("merge", &["main", "omitted"]),
            commit("side", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = folds_with(Some(Ancestry::from([(
            "omitted".into(),
            vec!["side".into()],
        )])));
        folds.ascii_graph = false;
        folds.refresh(&commits).unwrap();
        assert_eq!(plain(folds.display_graph(1, &commits[1])), ["├─╮ ", "│ ● "]);
        assert_eq!(folds.marker(0), None);
        folds.toggle(0, &commits).unwrap();
        assert!(!folds.visible(1));
        folds.toggle(0, &commits).unwrap();
        assert_eq!(plain(folds.display_graph(1, &commits[1])), ["├─╮ ", "│ ● "]);
    }

    #[test]
    fn unicode_graph_follows_only_first_parents_of_omitted_merges() {
        let commits = vec![
            commit("top", &["omitted"]),
            commit("mid", &["side"]),
            commit("side", &[]),
        ];
        // The omitted merge's first parent leads to no row.
        let mut folds = folds_with(Some(Ancestry::from([(
            "omitted".into(),
            vec!["unloaded".into(), "side".into()],
        )])));
        folds.refresh(&commits).unwrap();
        assert_eq!(plain(folds.display_graph(1, &commits[1])), ["● "]);
    }

    #[test]
    fn unicode_graph_reads_omitted_history_once() {
        let commits = vec![
            commit("merge", &["main", "omitted"]),
            commit("side", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ];
        for available in [true, false] {
            let reads = std::rc::Rc::new(std::cell::Cell::new(0));
            let counter = reads.clone();
            let mut folds = LogFolds {
                load_ancestry: Box::new(move |_| {
                    counter.set(counter.get() + 1);
                    available
                        .then(|| Ancestry::from([("omitted".into(), vec!["side".into()])]))
                        .ok_or_else(|| "no Git".to_owned())
                }),
                ..LogFolds::default()
            };
            folds.refresh(&commits).unwrap();
            folds.refresh(&commits).unwrap();
            if available {
                // Folding needs the same history the graph already read.
                folds.toggle(0, &commits).unwrap();
                assert!(!folds.visible(1));
            }
            assert_eq!(reads.get(), 1, "available: {available}");
        }
    }

    #[test]
    fn unicode_graph_keeps_git_connectivity_when_filtering_without_omitted_history() {
        let commits = vec![
            commit("merge", &["main", "omitted"]),
            commit("side", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = folds_with(None);
        folds.ascii_graph = false;
        folds.hidden_types.insert("test".into());
        assert!(folds.refresh(&commits).is_err());
        for (i, commit) in commits.iter().enumerate() {
            assert_eq!(folds.display_graph(i, commit), folds.graph(i, commit));
        }
    }

    /// A deterministic history of `size` commits, each with parents older
    /// than itself, including octopus merges and several roots.
    fn history(seed: &mut u32, size: usize) -> Vec<Vec<usize>> {
        let mut random = |n: usize| {
            *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (*seed >> 8) as usize % n
        };
        (0..size)
            .map(|i| {
                let older = size - i - 1;
                if older == 0 || random(12) == 0 {
                    return Vec::new();
                }
                let mut parents = vec![i + 1 + random(older.min(3))];
                for _ in 0..[0, 0, 1, 1, 2][random(5)] {
                    let parent = i + 1 + random(older);
                    if !parents.contains(&parent) {
                        parents.push(parent);
                    }
                }
                parents
            })
            .collect()
    }

    /// The commits reached by following unfolded parent links from every
    /// loaded commit that no other loaded commit reaches.
    fn model_reached(parents: &[Vec<usize>], loaded: &[bool], folded: &[bool]) -> Vec<bool> {
        let reach = |from: &[usize], folded: &[bool]| {
            let mut seen = vec![false; parents.len()];
            let mut todo = from.to_vec();
            while let Some(node) = todo.pop() {
                if !std::mem::replace(&mut seen[node], true) {
                    let links = &parents[node];
                    let count = if loaded[node] && folded[node] {
                        1
                    } else {
                        links.len()
                    };
                    todo.extend(&links[..count]);
                }
            }
            seen
        };
        let rows: Vec<_> = (0..parents.len()).filter(|&i| loaded[i]).collect();
        let below = reach(
            &rows
                .iter()
                .flat_map(|&r| &parents[r])
                .copied()
                .collect::<Vec<_>>(),
            &vec![false; parents.len()],
        );
        let tips: Vec<_> = rows.iter().copied().filter(|&r| !below[r]).collect();
        reach(&tips, folded)
    }

    fn model_visible(parents: &[Vec<usize>], loaded: &[bool], folded: &[bool]) -> Vec<bool> {
        let reached = model_reached(parents, loaded, folded);
        (0..parents.len())
            .map(|i| loaded[i] && reached[i])
            .collect()
    }

    #[test]
    fn folds_match_a_brute_force_model() {
        let mut seed = 7;
        for case in 0..400 {
            let size = 8 + case % 25;
            let parents = history(&mut seed, size);
            let mut random = |n: u32| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 8) % n
            };
            // Some cases load everything; others omit commits, like --grep.
            let loaded: Vec<_> = (0..size).map(|_| case % 3 == 0 || random(4) != 0).collect();
            let rows: Vec<_> = (0..size).filter(|&i| loaded[i]).collect();
            let name = |i: usize| format!("c{i}");
            let commits: Vec<_> = rows
                .iter()
                .map(|&i| {
                    let links: Vec<_> = parents[i].iter().map(|&p| name(p)).collect();
                    commit(
                        &name(i),
                        &links.iter().map(String::as_str).collect::<Vec<_>>(),
                    )
                })
                .collect();
            let ancestry: Ancestry = (0..size)
                .filter(|&i| !loaded[i])
                .map(|i| (name(i), parents[i].iter().map(|&p| name(p)).collect()))
                .collect();
            let merge_rows: Vec<_> = (0..rows.len())
                .filter(|&r| commits[r].parents.len() > 1)
                .collect();
            let collapsed: HashSet<_> = merge_rows
                .iter()
                .filter(|_| random(2) == 0)
                .map(|&r| commits[r].hash.clone())
                .collect();
            let model = |collapsed: &HashSet<String>| {
                let folded: Vec<_> = (0..size).map(|i| collapsed.contains(&name(i))).collect();
                let visible = model_visible(&parents, &loaded, &folded);
                rows.iter().map(|&i| visible[i]).collect::<Vec<_>>()
            };
            let context =
                format!("case {case}: {parents:?} loaded {loaded:?} folded {collapsed:?}");

            let exact = |collapsed: &HashSet<String>| {
                let mut folds = folds_with(Some(ancestry.clone()));
                folds.refresh(&commits).unwrap();
                folds.collapsed = collapsed.clone();
                folds.update(&commits, true).unwrap();
                folds
            };
            let mut folds = exact(&collapsed);
            let visible = model(&collapsed);
            assert_eq!(folds.layout.visible, visible, "{context}");
            // Shown folds that hide nothing are drawn as unfolded merges.
            let hiding: HashSet<_> = collapsed
                .iter()
                .filter(|hash| {
                    let row = folds.graph.index[*hash];
                    !visible[row] || folds.fold(row) != Fold::None
                })
                .cloned()
                .collect();
            let drawn = exact(&hiding);
            for (row, commit) in commits.iter().enumerate().filter(|&(r, _)| visible[r]) {
                assert_eq!(
                    folds.graph(row, commit),
                    drawn.graph(row, commit),
                    "row {row}, {context}"
                );
            }
            for &row in &merge_rows {
                let hash = &commits[row].hash;
                let mut toggled = collapsed.clone();
                if !toggled.remove(hash) {
                    toggled.insert(hash.clone());
                }
                let after = model(&toggled);
                let expected = if !visible[row] || after == visible {
                    Fold::None
                } else if collapsed.contains(hash) {
                    match folds.fold(row) {
                        Fold::Folded(n) if n > 0 => Fold::Folded(n),
                        _ => Fold::Folded(1),
                    }
                } else {
                    Fold::Foldable
                };
                assert_eq!(folds.fold(row), expected, "row {row}, {context}");

                // z does what the marker promised, or explains why not.
                let mut toggling = exact(&collapsed);
                let result = toggling.toggle(row, &commits);
                if visible[row] && expected != Fold::None {
                    assert!(result.is_ok(), "row {row}, {context}");
                    assert_eq!(toggling.layout.visible, after, "row {row}, {context}");
                    let flipped = toggling.fold(row);
                    assert!(
                        matches!(
                            (expected, flipped),
                            (Fold::Folded(_), Fold::Foldable) | (Fold::Foldable, Fold::Folded(_))
                        ),
                        "row {row}, {flipped:?}, {context}"
                    );
                } else if visible[row] {
                    assert!(result.is_err(), "row {row}, {context}");
                    assert_eq!(toggling.layout.visible, visible, "row {row}, {context}");
                }
            }

            // Each hidden row belongs to the nearest shown fold whose side
            // history reaches it through rows and commits that are hidden.
            let folded: Vec<_> = (0..size).map(|i| collapsed.contains(&name(i))).collect();
            let shown_nodes = model_reached(&parents, &loaded, &folded);
            for &row in &merge_rows {
                if !visible[row] || !folded[rows[row]] {
                    continue;
                }
                // Count the complete hidden ancestry of each shown fold,
                // independently of which fold owns it for navigation.
                let mut hidden = vec![false; size];
                for node in rows[row]..size {
                    if shown_nodes[node] {
                        continue;
                    }
                    hidden[node] = parents[rows[row]][1..].contains(&node)
                        || (0..node).any(|child| hidden[child] && parents[child].contains(&node));
                }
                let count = rows.iter().filter(|&&node| hidden[node]).count();
                let expected = if count == 0 {
                    Fold::None
                } else {
                    Fold::Folded(count)
                };
                assert_eq!(folds.fold(row), expected, "row {row}, {context}");
            }
            let mut total = 0;
            for hidden in (0..rows.len()).filter(|&r| !visible[r]) {
                let owner = (0..rows.len())
                    .filter(|&m| visible[m] && collapsed.contains(&commits[m].hash))
                    .filter(|&m| {
                        let mut todo: Vec<_> = parents[rows[m]][1..].to_vec();
                        let mut visited = vec![false; size];
                        while let Some(n) = todo.pop() {
                            if shown_nodes[n] || std::mem::replace(&mut visited[n], true) {
                                continue;
                            }
                            if n == rows[hidden] {
                                return true;
                            }
                            todo.extend(&parents[n]);
                        }
                        false
                    })
                    .max();
                assert_eq!(
                    folds.containing_fold(hidden),
                    owner,
                    "row {hidden}, {context}"
                );
                total += 1;
            }
            let counted: usize = (0..rows.len())
                .map(|r| match folds.fold(r) {
                    Fold::Folded(n) => n,
                    _ => 0,
                })
                .sum();
            assert!(counted >= total, "{context}");

            // Every hidden row can be revealed without hiding another.
            for hidden in (0..rows.len()).filter(|&r| !visible[r]) {
                let mut revealing = exact(&collapsed);
                revealing.reveal(hidden, &commits);
                assert!(revealing.visible(hidden), "row {hidden}, {context}");
                assert!((0..rows.len()).all(|r| !visible[r] || revealing.visible(r)));
            }

            // m folds until nothing is foldable, and if that is already so,
            // expands everything.
            let selected = random(rows.len() as u32) as usize;
            let folded = |folds: &LogFolds| {
                let any = folds
                    .layout
                    .folds
                    .iter()
                    .any(|f| matches!(f, Fold::Folded(_)));
                any && !folds.layout.folds.contains(&Fold::Foldable)
            };
            let unfolded = |folds: &LogFolds| {
                folds.collapsed.is_empty() && !folds.layout.visible.contains(&false)
            };
            match folds.toggle_all(selected, &commits) {
                Ok(moved) => {
                    assert!(folds.visible(moved), "{context}");
                    assert!(folded(&folds) || unfolded(&folds), "{context}");
                    if folded(&folds) {
                        folds.toggle_all(moved, &commits).unwrap();
                        assert!(unfolded(&folds), "{context}");
                    }
                }
                Err(_) => assert!(!folds.layout.folds.contains(&Fold::Foldable), "{context}"),
            }

            // Before the ancestry is read, a marker may be missing, but
            // never wrong, and the Log is shown unfolded.
            let mut unread = folds_with(None);
            unread.refresh(&commits).unwrap();
            let unfolded = model(&HashSet::new());
            for &row in &merge_rows {
                if unread.fold(row) != Fold::None {
                    let folded = HashSet::from([commits[row].hash.clone()]);
                    assert_ne!(model(&folded), unfolded, "row {row}, {context}");
                }
            }
            unread.collapsed = collapsed.clone();
            if unread.update(&commits, false).is_ok() && unread.graph.unknown_below.is_some() {
                assert!(unread.layout.visible.iter().all(|&v| v), "{context}");
            }
        }
    }
}
