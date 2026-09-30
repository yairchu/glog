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
    load_ancestry: AncestrySource,
    collapsed: HashSet<String>,
    // Merges already offered to --fold-merges.
    seen: HashSet<String>,
    // Ancestry through omitted commits stays valid while the loaded
    // revisions and their parents are unchanged.
    revisions: Vec<(String, Vec<String>)>,
    ancestry: Option<Ancestry>,
    graph: Graph,
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
            load_ancestry: Box::new(git::log_ancestry),
            collapsed: HashSet::new(),
            seen: HashSet::new(),
            revisions: Vec::new(),
            ancestry: None,
            graph: Graph::default(),
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
                layout
                    .graphs
                    .get(&index)
                    .is_some_and(|rows| !rows.is_empty())
            })
    }

    pub fn graph<'a>(&'a self, index: usize, commit: &'a Commit) -> &'a [String] {
        self.filtered
            .as_ref()
            .unwrap_or(&self.layout)
            .graphs
            .get(&index)
            .unwrap_or(&commit.graph)
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
            .map(|c| (c.hash.clone(), c.parents.clone()))
            .collect();
        if revisions != self.revisions {
            self.ancestry = None;
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
            match (self.load_ancestry)(commits) {
                Ok(ancestry) => {
                    let ancestry = leading_to_rows(ancestry, commits);
                    self.graph = Graph::new(commits, Some(&ancestry));
                    self.ancestry = Some(ancestry);
                    layout = Layout::new(&self.graph, commits, &self.collapsed);
                }
                Err(error) => {
                    self.layout = Layout::new(&self.graph, commits, &HashSet::new());
                    self.apply_filter(commits);
                    self.failed = true;
                    return Err(error);
                }
            }
        }
        self.layout = layout;
        self.apply_filter(commits);
        self.failed = false;
        Ok(())
    }

    pub fn filter_hidden(&self, commit: &Commit) -> bool {
        (self.hide_merges && commit.kind == CommitKind::Revision && commit.parents.len() > 1)
            || commit_type(commit).is_some_and(|kind| self.hidden_types.contains(kind))
    }

    pub fn toggle_type(&mut self, kind: Option<&str>, commits: &[Commit]) -> Result<(), String> {
        if let Some(kind) = kind {
            if !self.hidden_types.remove(kind) {
                self.hidden_types.insert(kind.to_owned());
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
        for (index, commit) in commits.iter().enumerate() {
            let hidden = self.filter_hidden(commit);
            self.hidden_count += usize::from(hidden);
            // Keep merge junctions in the drawing even when their text is
            // filtered out. Merge folds still control branch visibility.
            layout.visible[index] &= !hidden || commit.parents.len() > 1;
        }
        let mut folded: Vec<_> = commits
            .iter()
            .map(|c| self.collapsed.contains(&c.hash))
            .collect();
        self.layout.unfold_idle_merges(&mut folded);
        let reached = Layout::with_folds(&self.graph, &folded).1;
        layout.graphs = layout.draw(&self.graph, commits, &folded, &reached);
        for (index, commit) in commits.iter().enumerate() {
            if self.filter_hidden(commit) {
                layout.visible[index] = false;
                // The final row is the commit node. Keep only the incoming
                // routing rows; outgoing edges are attached to the next row.
                if let Some(rows) = layout.graphs.get_mut(&index) {
                    rows.pop();
                }
            }
        }
        self.filtered = Some(layout);
    }

    /// The nearest shown fold whose side history leads to a hidden row.
    fn containing_fold(&self, index: usize) -> Option<usize> {
        self.layout.owners.get(index).copied().flatten()
    }
}

/// Conventional Commit prefix, taken only from the subject of a revision.
/// Custom ASCII types and issue references before the scope are supported;
/// ordinary prose remains unclassified.
pub fn commit_type(commit: &Commit) -> Option<&str> {
    if commit.kind != CommitKind::Revision {
        return None;
    }
    let (prefix, description) = commit.subject.split_once(": ")?;
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
    normalize_type(kind)
}

/// Validate a filter type and use the same aliases in the CLI and TUI.
pub fn normalize_type(kind: &str) -> Option<&str> {
    (kind.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && kind
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'))
    .then_some(match kind {
        "doc" => "docs",
        "tests" => "test",
        "feature" => "feat",
        _ => kind,
    })
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
    // Each parent, and whether it is a row's side parent.
    parents: Vec<Vec<(usize, bool)>>,
    // Children before parents.
    order: Vec<usize>,
    tips: Vec<bool>,
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
            .flat_map(|c| &c.parents)
            .collect();
        let mut unknown_below = None;
        let mut parents = vec![Vec::new(); nodes];
        for (row, commit) in commits.iter().enumerate() {
            for (slot, parent) in commit.parents.iter().enumerate() {
                match index.get(parent) {
                    Some(&node) => parents[row].push((node, slot > 0)),
                    None if ancestry.is_none() && !boundary.contains(parent) => {
                        unknown_below = unknown_below.or(Some(row));
                    }
                    None => {}
                }
            }
        }
        for (hash, links) in ancestry.into_iter().flatten() {
            let node = index[hash];
            if node >= rows {
                parents[node] = links
                    .iter()
                    .filter_map(|parent| index.get(parent))
                    .map(|&parent| (parent, false))
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
struct Layout {
    visible: Vec<bool>,
    // The fold containing each hidden row.
    owners: Vec<Option<usize>>,
    folds: Vec<Fold>,
    graphs: HashMap<usize, Vec<String>>,
}

impl Layout {
    fn new(graph: &Graph, commits: &[Commit], collapsed: &HashSet<String>) -> Self {
        let mut folded: Vec<_> = commits
            .iter()
            .map(|c| c.parents.len() > 1 && collapsed.contains(&c.hash))
            .collect();
        let (mut layout, mut reached) = Self::with_folds(graph, &folded);
        if layout.unfold_idle_merges(&mut folded) {
            reached = Self::with_folds(graph, &folded).1;
        }
        if layout.visible.contains(&false) {
            layout.graphs = layout.draw(graph, commits, &folded, &reached);
        }
        layout
    }

    /// Normalize drawing choices without changing the saved merge folds.
    fn unfold_idle_merges(&self, folded: &mut [bool]) -> bool {
        // A shown fold that hides nothing, such as one inside a fold that
        // was since expanded, stays chosen: folding another merge could
        // make it hide something again. It is drawn as a merge, though.
        // Following its side parents reaches only shown rows.
        let mut idle = false;
        for (row, folded) in folded.iter_mut().enumerate() {
            if *folded && self.visible[row] && self.folds[row] == Fold::None {
                *folded = false;
                idle = true;
            }
        }
        idle
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
            graphs: HashMap::new(),
        };
        (layout, reached)
    }

    /// Redraw the graph over the shown rows, keeping Git's lane colours.
    fn draw(
        &self,
        graph: &Graph,
        commits: &[Commit],
        folded: &[bool],
        reached: &[bool],
    ) -> HashMap<usize, Vec<String>> {
        // Compute destinations parents first. Type-hidden loaded commits
        // preserve every unfolded parent; ancestry omitted by Git follows
        // first parents, matching the existing merge-fold projection.
        let mut leads: Vec<Vec<usize>> = vec![Vec::new(); graph.parents.len()];
        for &node in graph.order.iter().rev() {
            if node < graph.rows && self.visible[node] {
                leads[node].push(node);
            } else if reached[node] {
                for &(parent, side) in &graph.parents[node] {
                    if (node >= graph.rows || folded[node]) && side {
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
        let mut graphs = HashMap::new();
        // Each lane keeps a colour from Git's default palette while it lasts.
        let mut lanes = Vec::<(usize, usize)>::new();
        let mut colours = 0..;
        let mut pending = Vec::new();
        for (index, commit) in commits.iter().enumerate() {
            if !self.visible[index] {
                continue;
            }
            if commit.kind != CommitKind::Revision {
                graphs.insert(index, commit.graph.clone());
                continue;
            }
            let column = lanes
                .iter()
                .position(|&(row, _)| row == index)
                .unwrap_or_else(|| {
                    lanes.push((index, colours.next().unwrap()));
                    lanes.len() - 1
                });
            let node: String = lanes
                .iter()
                .enumerate()
                .map(|(i, &(_, colour))| {
                    if i == column {
                        "* ".to_owned()
                    } else {
                        format!("{} ", paint('|', colour))
                    }
                })
                .collect();
            pending.push(node);
            graphs.insert(index, std::mem::take(&mut pending));

            // A folded merge follows its first parent. Other edges pass
            // through omitted commits to the shown row they lead to.
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
            let mut next = lanes.clone();
            let (_, colour) = next.remove(column);
            let mut insert = column;
            for &parent in &projected {
                if !next.iter().any(|&(row, _)| row == parent) {
                    // The first new parent continues this lane's colour.
                    let colour = if insert == column {
                        colour
                    } else {
                        colours.next().unwrap()
                    };
                    next.insert(insert, (parent, colour));
                    insert += 1;
                }
            }
            let mut edges = Vec::new();
            for (from, &(row, _)) in lanes.iter().enumerate() {
                let targets = if from == column {
                    &projected[..]
                } else {
                    std::slice::from_ref(&row)
                };
                for &target in targets {
                    if let Some(to) = next.iter().position(|&(r, _)| r == target) {
                        edges.push((2 * from, 2 * to, next[to].1));
                    }
                }
            }
            pending = transitions(&edges, lanes.len().max(next.len()));
            lanes = next;
        }
        graphs
    }
}

// Git's default graph colours, cycled per lane.
const PALETTE: [&str; 12] = [
    "31", "32", "33", "34", "35", "36", "1;31", "1;32", "1;33", "1;34", "1;35", "1;36",
];

fn paint(symbol: char, colour: usize) -> String {
    format!("\x1b[{}m{symbol}\x1b[m", PALETTE[colour % PALETTE.len()])
}

// Route edges between columns two characters apart, each in its target
// lane's colour. As in Git, each row moves a route one lane, drawn between
// the two. Crossings retain their independent targets; '+' joins routes to
// the same target, 'X' crosses routes to different targets.
fn transitions(edges: &[(usize, usize, usize)], width: usize) -> Vec<String> {
    let lanes = |from: usize, to: usize| from.abs_diff(to) / 2;
    let distance = edges
        .iter()
        .map(|&(from, to, _)| lanes(from, to))
        .max()
        .unwrap_or(0);
    let mut rows = Vec::new();
    for step in 1..=distance {
        let mut cells = vec![(' ', 0); width * 2];
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn conventional_types_require_a_subject_prefix() {
        for (subject, expected) in [
            ("feat: add something", Some("feat")),
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
            assert_eq!(commit_type(&c), expected, "{subject}");
            c.kind = CommitKind::WorkingTree;
            assert_eq!(commit_type(&c), None);
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
        assert_eq!(plain(&transitions(&[(0, 0, 0), (0, 2, 1)], 2)), ["|\\ "]);
        assert_eq!(plain(&transitions(&[(0, 0, 0), (2, 0, 1)], 2)), ["|/ "]);
        assert_eq!(
            plain(&transitions(&[(0, 0, 0), (4, 0, 1)], 3)),
            ["|  / ", "|/ "]
        );
    }

    #[test]
    fn crossing_routes_do_not_turn_into_a_shared_parent() {
        assert_eq!(plain(&transitions(&[(0, 2, 0), (2, 0, 1)], 2)), [" X "]);
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
