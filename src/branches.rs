//! Local branches, the remote default tip, and the commits where their histories meet.
use std::collections::{BTreeSet, HashMap, HashSet};

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::{
    app::App,
    git::{self, Annotation, Collaborators, Commit, CommitKind},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BranchKind {
    Local,
    Remote,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub current: bool,
    merged_into: Option<String>,
    kind: BranchKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchNode {
    // Multiple branches may name the same tip; the graph has one node per commit.
    pub branches: Vec<Branch>,
    // A genuine intersection of open histories can coincide with a merged tip.
    shared_ancestor: bool,
    updated_at: Option<String>,
    update_width: usize,
}

pub enum Launch {
    Print(String),
    Interactive(Box<App>),
}

pub fn load(args: &[String]) -> Result<Launch, String> {
    let mut date = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--date" {
            date = Some(args.next().ok_or("--date requires a value")?.clone());
        } else if let Some(value) = arg.strip_prefix("--date=") {
            date = Some(value.to_owned());
        } else {
            return Err("use glog branch [--date=STYLE] (lists local branches)".into());
        }
    }
    let output = git::read_command()
        .args([
            "for-each-ref",
            "--sort=refname",
            // A branch can point to an annotated tag. Name the commit it tags,
            // as the traversal does.
            "--format=%(if)%(*objectname)%(then)%(*objectname)%(else)%(objectname)%(end)\
              %00%(refname:strip=2)%00%(HEAD)",
            "refs/heads/",
        ])
        .output()
        .map_err(|error| format!("could not list branches: {error}"))?;
    if !output.status.success() {
        return Err(git::stderr_message(
            "could not list branches",
            &output.stderr,
        ));
    }
    let mut tips: HashMap<String, Vec<Branch>> = HashMap::new();
    let mut count = 0;
    let mut current = false;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let fields: Vec<_> = line.split('\0').collect();
        let [hash, name, head] = fields.as_slice() else {
            return Err("invalid Git branch list output".into());
        };
        let branch = Branch {
            name: (*name).into(),
            current: *head == "*",
            merged_into: None,
            kind: BranchKind::Local,
        };
        count += 1;
        current |= branch.current;
        tips.entry((*hash).into()).or_default().push(branch);
    }
    if count == 0 || (count == 1 && current) {
        // Match Git's own empty/unborn and single-current-branch output.
        let output = git::read_command()
            .args(["--no-pager", "branch", "--list"])
            .output()
            .map_err(|error| format!("could not list branches: {error}"))?;
        if !output.status.success() {
            return Err(git::stderr_message(
                "could not list branches",
                &output.stderr,
            ));
        }
        return Ok(Launch::Print(
            String::from_utf8_lossy(&output.stdout).into(),
        ));
    }
    let remote_ref = origin_default_ref();
    let remote = remote_default(remote_ref.as_deref());
    let primary_name = remote_ref
        .as_deref()
        .and_then(|reference| reference.strip_prefix("refs/remotes/"))
        .or_else(|| remote.as_ref().map(|(name, _)| name.as_str()));
    let primary = default_branch(&tips, primary_name);
    if let Some((name, hash)) = remote {
        tips.entry(hash).or_default().push(Branch {
            name,
            current: false,
            merged_into: None,
            kind: BranchKind::Remote,
        });
    }
    // Pin the refs before traversal so concurrent branch changes cannot mix snapshots.
    let mut hashes: Vec<_> = tips.keys().cloned().collect();
    hashes.sort();
    let mut input = hashes.join("\n") + "\n";
    // Everything below a common ancestor of every tip is shared by all of
    // them, so no fork can appear there. Stop the walk at that ancestor.
    let base = git::read_command()
        .args(["merge-base", "--octopus"])
        .args(&hashes)
        .env("GIT_NO_LAZY_FETCH", "1")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok());
    if let Some(base) = base
        .as_deref()
        .map(str::trim)
        .filter(|base| !base.is_empty())
    {
        input += &format!("^{base}^@\n");
    }
    let output = git::pipe_through(
        git::read_command()
            .args(["rev-list", "--topo-order", "--parents", "--stdin"])
            .env("GIT_NO_LAZY_FETCH", "1"),
        input.as_bytes(),
    )
    .ok_or("could not read branch ancestry")?;
    let mut rows = compact(&output, tips, primary.as_deref())?;
    let date = date
        .map(Ok)
        .unwrap_or_else(|| git::configured_log_date("format-local:%Y-%m-%d %H:%M"))?;
    load_metadata(&mut rows, &date)?;
    let selected = rows
        .iter()
        .position(|row| {
            row.branch()
                .is_some_and(|node| node.branches.iter().any(|b| b.current))
        })
        .unwrap_or(0);
    let mut app = App::new(rows);
    app.selected = selected;
    app.center_selection = true;
    Ok(Launch::Interactive(Box::new(app)))
}

fn origin_default_ref() -> Option<String> {
    git::read_command()
        .args(["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| name.starts_with("refs/remotes/origin/"))
}

/// Read only the stored tracking refs; never fetch to populate the graph.
fn remote_default(symbolic: Option<&str>) -> Option<(String, String)> {
    let mut candidates = vec![
        "refs/remotes/origin/main".to_owned(),
        "refs/remotes/origin/master".to_owned(),
    ];
    if let Some(reference) = symbolic {
        candidates.insert(0, reference.to_owned());
    }
    let output = git::read_command()
        .args([
            "for-each-ref",
            "--format=%(refname)%00\
             %(if)%(*objectname)%(then)%(*objectname)%(else)%(objectname)%(end)",
        ])
        .args(&candidates)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let refs: HashMap<_, _> = text
        .lines()
        .filter_map(|line| line.split_once('\0'))
        .collect();
    candidates.iter().find_map(|reference| {
        let hash = refs.get(reference.as_str())?;
        Some((
            reference.strip_prefix("refs/remotes/")?.to_owned(),
            (*hash).to_owned(),
        ))
    })
}

/// Prefer the local counterpart of origin's default branch, then conventional names.
fn default_branch(tips: &HashMap<String, Vec<Branch>>, remote: Option<&str>) -> Option<String> {
    let exists = |name: &str| {
        tips.values()
            .flatten()
            .any(|branch| branch.kind == BranchKind::Local && branch.name == name)
    };
    remote
        .and_then(|name| name.strip_prefix("origin/"))
        .filter(|name| exists(name))
        .map(str::to_owned)
        .or_else(|| {
            ["main", "master"]
                .into_iter()
                .find(|name| exists(name))
                .map(str::to_owned)
        })
}

impl BranchNode {
    pub fn style_graph(
        &self,
        mut line: Line<'static>,
        original: char,
        selected: bool,
    ) -> Line<'static> {
        let (marker, style) = if self.branches.iter().any(|branch| branch.current) {
            (
                '◆',
                Style::default()
                    .fg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            )
        } else if self
            .branches
            .iter()
            .any(|branch| branch.kind == BranchKind::Local)
        {
            ('●', Style::default().fg(Color::Cyan))
        } else if !self.branches.is_empty() {
            ('○', Style::default().fg(Color::Cyan))
        } else {
            // Keep the marker visible on the dark gray cursor background.
            let color = if selected {
                Color::Gray
            } else {
                Color::DarkGray
            };
            ('●', Style::default().fg(color))
        };
        let nodes = [original, crate::log_folds::unicode_node(original)];
        let mut offset = 0;
        for span in &mut line.spans {
            if let Some(start) = span.content.find(nodes) {
                span.content = span.content.replacen(nodes, &marker.to_string(), 1).into();
                let start = offset + start;
                // Keep adjacent graph lanes in their original colors.
                return crate::ansi::style_ranges(
                    line,
                    [(start..start + marker.len_utf8(), style)],
                );
            }
            offset += span.content.len();
        }
        line
    }

    fn merged(&self) -> bool {
        !self.branches.is_empty()
            && self
                .branches
                .iter()
                .all(|branch| branch.merged_into.is_some())
    }
}

/// Merged tips remain graph landmarks, but only open tips and true shared
/// ancestors stop a Log. A merged branch itself opens its complete history.
pub fn log_args(node: &Commit, rows: &[Commit]) -> Vec<String> {
    let mut args = vec![node.hash.clone()];
    if !node.branch().is_some_and(BranchNode::merged) {
        let index: HashMap<_, _> = rows.iter().map(|row| (row.hash.as_str(), row)).collect();
        let mut todo = node.parents.clone();
        let mut seen = HashSet::new();
        let mut bases = BTreeSet::new();
        while let Some(hash) = todo.pop() {
            if !seen.insert(hash.clone()) {
                continue;
            }
            match index.get(hash.as_str()) {
                Some(parent)
                    if parent
                        .branch()
                        .is_some_and(|branch| branch.merged() && !branch.shared_ancestor) =>
                {
                    todo.extend(parent.parents.iter().cloned());
                }
                _ => {
                    bases.insert(hash);
                }
            }
        }
        args.extend(bases.into_iter().map(|hash| format!("^{hash}")));
    }
    args.push("--".into());
    args
}

struct Node {
    hash: String,
    /// Parents within the walk. Those below its common-ancestor cutoff
    /// carry no forks.
    parents: Vec<usize>,
}

fn compact(
    ancestry: &str,
    mut tips: HashMap<String, Vec<Branch>>,
    primary: Option<&str>,
) -> Result<Vec<Commit>, String> {
    let records: Vec<_> = ancestry
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .filter(|fields| !fields.is_empty())
        .collect();
    let indices: HashMap<_, _> = records
        .iter()
        .enumerate()
        .map(|(index, fields)| (fields[0], index))
        .collect();
    let nodes: Vec<_> = records
        .iter()
        .map(|fields| Node {
            hash: fields[0].into(),
            parents: fields[1..]
                .iter()
                .filter_map(|hash| indices.get(hash).copied())
                .collect(),
        })
        .collect();
    let tip_indices = tips
        .keys()
        .map(|hash| {
            let index = indices.get(hash.as_str()).ok_or("missing branch tip")?;
            Ok((hash.clone(), *index))
        })
        .collect::<Result<HashMap<_, _>, String>>()?;
    if let Some(primary) = primary {
        let primary_index = tips.iter().find_map(|(hash, branches)| {
            branches
                .iter()
                .any(|branch| branch.kind == BranchKind::Local && branch.name == primary)
                .then(|| tip_indices[hash])
        });
        if let Some(primary_index) = primary_index {
            let mut integrated = vec![false; nodes.len()];
            integrated[primary_index] = true;
            for (index, node) in nodes.iter().enumerate() {
                if integrated[index] {
                    for &parent in &node.parents {
                        integrated[parent] = true;
                    }
                }
            }
            for (hash, branches) in &mut tips {
                if integrated[tip_indices[hash]] {
                    for branch in branches
                        .iter_mut()
                        .filter(|branch| branch.kind == BranchKind::Local && branch.name != primary)
                    {
                        branch.merged_into = Some(primary.to_owned());
                    }
                }
            }
        }
    }
    let mut owners = vec![BTreeSet::new(); nodes.len()];
    for (owner, (hash, _)) in tips
        .iter()
        .filter(|(_, branches)| branches.iter().any(|branch| branch.merged_into.is_none()))
        .enumerate()
    {
        owners[tip_indices[hash]].insert(owner);
    }
    let mut largest_child = vec![0; nodes.len()];
    let mut keep = vec![false; nodes.len()];
    let mut shared_ancestor = vec![false; nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        // Children precede parents. A new intersection of tip histories is a
        // fork; ordinary chains and old merges shared by every tip disappear.
        let reachable = std::mem::take(&mut owners[index]);
        shared_ancestor[index] = reachable.len() > 1 && reachable.len() > largest_child[index];
        keep[index] = tips.contains_key(&node.hash) || shared_ancestor[index];
        for &parent in &node.parents {
            if parent <= index {
                return Err("branch ancestry is not in topological order".into());
            }
            largest_child[parent] = largest_child[parent].max(reachable.len());
            owners[parent].extend(reachable.iter().copied());
        }
    }
    // Compress paths to their next retained ancestors. Prune redundant edges
    // when a merge reaches a node both directly and through a nearer fork.
    let mut frontiers: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); nodes.len()];
    let mut ancestors: HashMap<usize, HashSet<usize>> = HashMap::new();
    let mut parents = HashMap::new();
    for index in (0..nodes.len()).rev() {
        let frontier: BTreeSet<_> = nodes[index]
            .parents
            .iter()
            .flat_map(|&parent| frontiers[parent].iter().copied())
            .collect();
        if keep[index] {
            let nearest: Vec<_> = frontier
                .iter()
                .copied()
                .filter(|candidate| {
                    !frontier
                        .iter()
                        .any(|other| other != candidate && ancestors[other].contains(candidate))
                })
                .collect();
            let reachable = nearest
                .iter()
                .flat_map(|parent| {
                    std::iter::once(*parent).chain(ancestors[parent].iter().copied())
                })
                .collect();
            ancestors.insert(index, reachable);
            parents.insert(index, nearest);
            frontiers[index].insert(index);
        } else {
            frontiers[index] = frontier;
        }
    }
    Ok(nodes
        .iter()
        .enumerate()
        .filter(|(index, _)| keep[*index])
        .map(|(index, node)| {
            let branches = tips.remove(&node.hash).unwrap_or_default();
            let subject = if branches.is_empty() {
                "shared ancestor".into()
            } else {
                branches
                    .iter()
                    .map(|branch| branch.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            Commit {
                kind: CommitKind::Revision,
                annotation: Some(Annotation::Branch(BranchNode {
                    branches,
                    shared_ancestor: shared_ancestor[index],
                    updated_at: None,
                    update_width: 0,
                })),
                hash: node.hash.clone(),
                short_hash: node.hash[..node.hash.len().min(7)].into(),
                decorations: String::new(),
                subject,
                author: String::new(),
                author_email: String::new(),
                author_date: String::new(),
                collaborators: Collaborators::default(),
                parents: parents[&index]
                    .iter()
                    .map(|&p| nodes[p].hash.clone())
                    .collect(),
                first_parent_only: false,
                diff_args: Vec::new(),
                graph: vec!["* ".into()],
                graph_node: '*',
            }
        })
        .collect())
}

/// Fetch standard decorations and fallback commit dates for retained nodes,
/// including refs that do not themselves add nodes to the compact graph.
fn load_metadata(rows: &mut [Commit], date: &str) -> Result<(), String> {
    let input: String = rows.iter().map(|row| format!("{}\n", row.hash)).collect();
    let output = git::pipe_through(
        git::read_command()
            .args([
                "log",
                "--no-walk=unsorted",
                "--stdin",
                "--decorate=short",
                "--no-color",
                "--no-patch",
                "--no-show-signature",
                "--no-notes",
                "-z",
                "--format=%H%x00%D%x00%cd",
            ])
            .arg(format!("--date={date}")),
        input.as_bytes(),
    )
    .ok_or("could not read branch refs or date format")?;
    let fields: Vec<_> = output
        .strip_suffix('\0')
        .unwrap_or(&output)
        .split('\0')
        .collect();
    let (records, remainder) = fields.as_chunks::<3>();
    if !remainder.is_empty() {
        return Err("invalid Git ref decoration output".into());
    }
    let refs: HashMap<_, _> = records
        .iter()
        .map(|record| (record[0], (record[1], record[2])))
        .collect();
    let mut updates = branch_updates(rows, date).into_iter();
    for row in rows.iter_mut() {
        let (decorations, commit_date) = refs
            .get(row.hash.as_str())
            .ok_or("missing branch ref decorations")?;
        if commit_date.chars().any(char::is_control) {
            return Err("Git commit dates must fit on one line; use --date=iso-strict".into());
        }
        row.decorations = (*decorations).to_owned();
        if let Some(Annotation::Branch(node)) = &mut row.annotation {
            node.updated_at = updates
                .next()
                .expect("an update for each branch row")?
                .or_else(|| Some((*commit_date).to_owned()));
        }
    }
    let width = rows
        .iter()
        .filter_map(Commit::branch)
        .filter_map(|node| node.updated_at.as_deref())
        .map(|date| Span::raw(date).width())
        .max()
        .unwrap_or(0);
    for node in rows.iter_mut().filter_map(|row| match &mut row.annotation {
        Some(Annotation::Branch(node)) => Some(node),
        _ => None,
    }) {
        node.update_width = width;
    }
    Ok(())
}

fn branch_ref(branch: &Branch) -> String {
    format!(
        "refs/{}/{}",
        match branch.kind {
            BranchKind::Local => "heads",
            BranchKind::Remote => "remotes",
        },
        branch.name
    )
}

/// A missing reflog leaves the update time unknown. A ref that moved after
/// the graph snapshot must not lend its new timestamp to the pinned old tip.
fn ref_update(reference: &str, hash: &str, date: &str) -> Option<String> {
    let output = git::read_command()
        .env("LC_ALL", "C")
        .args([
            "reflog",
            "show",
            "-1",
            "--no-color",
            "--no-patch",
            "--no-show-signature",
            "--no-notes",
            "-z",
            "--format=%H%x00%gD",
        ])
        .arg(format!("--date={date}"))
        .arg(reference)
        .arg("--")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let (tip, selector) = text.strip_suffix('\0').unwrap_or(&text).split_once('\0')?;
    if tip != hash {
        return None;
    }
    selector
        .split_once("@{")?
        .1
        .strip_suffix('}')
        .map(str::to_owned)
}

/// Each reflog needs its own Git process, so read them concurrently.
/// Results follow the order of the branch rows.
fn branch_updates(rows: &[Commit], date: &str) -> Vec<Result<Option<String>, String>> {
    let nodes: Vec<_> = rows
        .iter()
        .filter_map(|row| Some((row.branch()?, row.hash.as_str())))
        .collect();
    let workers = std::thread::available_parallelism().map_or(1, usize::from);
    let chunk = nodes.len().div_ceil(workers).max(1);
    std::thread::scope(|scope| {
        let readers: Vec<_> = nodes
            .chunks(chunk)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|(node, hash)| latest_update(&node.branches, hash, date))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        readers
            .into_iter()
            .flat_map(|reader| reader.join().expect("reflog reader panicked"))
            .collect()
    })
}

fn latest_update(branches: &[Branch], hash: &str, date: &str) -> Result<Option<String>, String> {
    let reference = if let [branch] = branches {
        Some(branch_ref(branch))
    } else {
        // Aliases may have moved at different times. Compare reflog epochs,
        // then ask Git to format the newest event using the requested style.
        branches
            .iter()
            .filter_map(|branch| {
                let reference = branch_ref(branch);
                let raw = ref_update(&reference, hash, "raw")?;
                let timestamp = raw.split_whitespace().next()?.parse::<i64>().ok()?;
                Some((timestamp, reference))
            })
            .max_by_key(|(timestamp, _)| *timestamp)
            .map(|(_, reference)| reference)
    };
    let updated = reference.and_then(|reference| ref_update(&reference, hash, date));
    if updated
        .as_ref()
        .is_some_and(|date| date.chars().any(char::is_control))
    {
        return Err("Git branch update dates must fit on one line; use --date=iso-strict".into());
    }
    Ok(updated)
}

pub fn spans(
    commit: &Commit,
    node: &BranchNode,
    format: &crate::log_format::LogFormat,
) -> Vec<Span<'static>> {
    use crate::log_format::{field_span, pad, Field};
    let mut spans = Vec::new();
    if format.shows(Field::Date) && node.update_width > 0 {
        let date = node.updated_at.as_deref().unwrap_or("");
        spans.push(field_span(Field::Date, pad(date, node.update_width)));
    }
    let refs = if commit.decorations.is_empty() && !node.branches.is_empty() {
        // Keep pinned branch names identifiable if their refs moved during loading.
        &commit.subject
    } else {
        &commit.decorations
    };
    if !refs.is_empty() {
        spans.push(field_span(Field::Refs, format!("({refs})")));
    }
    if node.branches.is_empty() {
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        spans.push(field_span(Field::Hash, commit.short_hash.clone()));
        spans.push(Span::styled(
            " shared ancestor",
            Style::default().fg(Color::Gray),
        ));
    }
    if node.merged() {
        spans.push(Span::styled(
            format!(
                " · merged into {}",
                node.branches[0].merged_into.as_deref().unwrap()
            ),
            Style::default().fg(Color::Gray),
        ));
    } else {
        for branch in &node.branches {
            if let Some(target) = &branch.merged_into {
                spans.push(Span::styled(
                    format!(" · {} merged into {target}", branch.name),
                    Style::default().fg(Color::Gray),
                ));
            }
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{History, Mode},
        git::tests::{git, CurrentDirGuard, TestDirectory},
        input,
    };
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::{backend::TestBackend, Terminal};

    fn graph(text: &str, heads: &[(&str, &[&str])]) -> Vec<Commit> {
        compact(
            text,
            heads
                .iter()
                .map(|(hash, names)| {
                    (
                        (*hash).into(),
                        names
                            .iter()
                            .map(|name| Branch {
                                name: (*name).into(),
                                current: false,
                                merged_into: None,
                                kind: BranchKind::Local,
                            })
                            .collect(),
                    )
                })
                .collect(),
            Some("main"),
        )
        .unwrap()
    }

    fn edges(rows: &[Commit]) -> Vec<(&str, Vec<&str>)> {
        rows.iter()
            .map(|row| {
                (
                    row.hash.as_str(),
                    row.parents.iter().map(String::as_str).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn compact_graph_keeps_tip_ancestors_forks_and_aliases() {
        let stacked = graph(
            "c x\nx b\nb a\na r\nr\n",
            &[
                ("a", &["main"]),
                ("b", &["feature", "alias"]),
                ("c", &["stacked"]),
            ],
        );
        assert_eq!(
            edges(&stacked),
            [("c", vec!["b"]), ("b", vec!["a"]), ("a", vec![])]
        );
        assert_eq!(stacked[1].branch().unwrap().branches.len(), 2);
        let diverged = graph(
            "a x\nb y\nx f\ny f\nf r\nr\n",
            &[("a", &["main"]), ("b", &["feature"])],
        );
        assert_eq!(
            edges(&diverged),
            [("a", vec!["f"]), ("b", vec!["f"]), ("f", vec![])]
        );
        assert!(diverged[2].branch().unwrap().branches.is_empty());
    }

    #[test]
    fn compact_graph_collapses_old_merges_and_preserves_multiple_bases() {
        let old_merge = graph(
            "a m\nb m\nm x y\nx r\ny r\nr\n",
            &[("a", &["main"]), ("b", &["feature"])],
        );
        assert_eq!(
            edges(&old_merge),
            [("a", vec!["m"]), ("b", vec!["m"]), ("m", vec![])]
        );
        let criss_cross = graph(
            "a x y\nb y x\nx r\ny r\nr\n",
            &[("a", &["main"]), ("b", &["feature"])],
        );
        assert_eq!(
            edges(&criss_cross),
            [
                ("a", vec!["x", "y"]),
                ("b", vec!["x", "y"]),
                ("x", vec![]),
                ("y", vec![])
            ]
        );
        let redundant = graph(
            "a x c\nb x\nx c\nc r\nr\n",
            &[("a", &["main"]), ("b", &["feature"]), ("c", &["old"])],
        );
        assert_eq!(
            edges(&redundant),
            [
                ("a", vec!["x"]),
                ("b", vec!["x"]),
                ("x", vec!["c"]),
                ("c", vec![])
            ]
        );
        let unrelated = graph("a r\nb s\nr\ns\n", &[("a", &["main"]), ("b", &["orphan"])]);
        assert_eq!(edges(&unrelated), [("a", vec![]), ("b", vec![])]);
    }

    #[test]
    fn branch_markers_distinguish_current_local_remote_and_shared_nodes() {
        let mut rows = graph(
            "a f\nb f\nc f\nf r\nr\n",
            &[
                ("a", &["main"]),
                ("b", &["origin/main"]),
                ("c", &["feature"]),
            ],
        );
        for row in &mut rows {
            let Some(Annotation::Branch(node)) = &mut row.annotation else {
                panic!("expected branch node");
            };
            match row.hash.as_str() {
                "a" => node.branches[0].current = true,
                "b" => node.branches[0].kind = BranchKind::Remote,
                "c" => node.branches.push(Branch {
                    name: "origin/feature".into(),
                    current: false,
                    merged_into: None,
                    kind: BranchKind::Remote,
                }),
                _ => {}
            }
        }
        let mut app = App::new(rows);
        app.history = History::Branches;
        let mut terminal = Terminal::new(TestBackend::new(90, 12)).unwrap();
        // Moving the cursor must not move the checked-out branch's marker.
        for selected in [0, 2, 3] {
            app.selected = selected;
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            for (hash, marker, color) in [
                ("a", "◆", Color::LightGreen),
                ("b", "○", Color::Cyan),
                ("c", "●", Color::Cyan),
                (
                    "f",
                    "●",
                    if selected == 3 {
                        Color::Gray
                    } else {
                        Color::DarkGray
                    },
                ),
            ] {
                let index = app.commits.iter().position(|row| row.hash == hash).unwrap();
                let y = app
                    .visible_log_rows
                    .iter()
                    .rposition(|row| *row == Some(index))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let cell = (0..90)
                    .map(|x| &buffer[(x, 1 + y as u16)])
                    .find(|cell| cell.symbol() == marker)
                    .unwrap_or_else(|| panic!("missing {marker} for {hash}"));
                assert_eq!(cell.fg, color, "{hash}");
                if hash == "f" {
                    let hash_cell = (0..90)
                        .map(|x| &buffer[(x, 1 + y as u16)])
                        .find(|cell| cell.symbol() == "f")
                        .expect("shared ancestor hash");
                    assert_eq!(hash_cell.fg, Color::Yellow);
                }
            }
        }
    }

    #[test]
    fn merged_labels_preserve_real_forks_and_shared_default_tips() {
        // A merged branch can also sit exactly where an open topic diverged.
        let rows = graph(
            "a m\nm b x\nc b\nb r\nx r\nr\n",
            &[("a", &["main"]), ("b", &["merged"]), ("c", &["topic"])],
        );
        assert_eq!(
            edges(&rows),
            [("a", vec!["b"]), ("c", vec!["b"]), ("b", vec![])]
        );
        assert_eq!(log_args(&rows[0], &rows), ["a", "^b", "--"]);
        assert_eq!(log_args(&rows[2], &rows), ["b", "--"]);
        assert!(rows[2].branch().unwrap().shared_ancestor);
        // An alias merged at the default tip must not turn the entire node
        // into a retired branch and remove a stacked topic's real base.
        let rows = graph(
            "b a\na r\nr\n",
            &[("a", &["alias", "main"]), ("b", &["topic"])],
        );
        assert!(!rows[1].branch().unwrap().merged());
        assert_eq!(log_args(&rows[0], &rows), ["b", "^a", "--"]);
        assert!(rows[1].branch().unwrap().branches[0].merged_into.is_some());
        assert!(rows[0].branch().unwrap().branches[0].merged_into.is_none());
    }

    fn commit(tree: &str, message: &str, parents: &[&str]) -> String {
        let mut args = vec!["commit-tree", tree, "-m", message];
        for parent in parents {
            args.extend(["-p", *parent]);
        }
        git(&args)
    }

    fn interactive() -> Box<App> {
        match load(&[]).unwrap() {
            Launch::Interactive(app) => app,
            Launch::Print(text) => panic!("expected interactive branches, got {text:?}"),
        }
    }

    fn press(app: &mut App, code: KeyCode) {
        input::handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), app);
    }

    #[test]
    fn tab_does_not_open_a_branch_log() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        let tree = git(&["write-tree"]);
        let root = commit(&tree, "root", &[]);
        let main = commit(&tree, "main", &[&root]);
        let feature = commit(&tree, "feature", &[&root]);
        git(&["update-ref", "refs/heads/main", &main]);
        git(&["update-ref", "refs/heads/feature", &feature]);
        let mut app = interactive();
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.history, History::Branches);
        assert_eq!(app.mode, Mode::Log);
    }

    #[test]
    fn branch_logs_stop_at_forks_and_return_to_the_picker_without_checkout() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        let tree = git(&["write-tree"]);
        let root = commit(&tree, "root", &[]);
        let main = commit(&tree, "main", &[&root]);
        let feature = commit(&tree, "feature", &[&root]);
        let stacked = commit(&tree, "stacked", &[&feature]);
        for (name, hash) in [
            ("main", &main),
            ("feature", &feature),
            ("alias", &feature),
            ("stacked", &stacked),
        ] {
            git(&["update-ref", &format!("refs/heads/{name}"), hash]);
        }
        git(&["symbolic-ref", "HEAD", "refs/heads/stacked"]);
        let mut app = interactive();
        assert_eq!(app.history, History::Branches);
        assert_eq!(app.commits.len(), 4);
        assert_eq!(app.commits[app.selected].hash, stacked);
        assert_eq!(app.status, None);
        assert!(!app.log_folds.reading());
        let mut terminal = Terminal::new(TestBackend::new(90, 12)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("Branches"));
        assert!(screen.contains("HEAD -> stacked"));
        assert!(screen.contains("alias") && screen.contains("feature"));
        assert!(screen.contains("shared ancestor"));
        assert!(!screen.contains(" Show "));
        assert!(screen.contains(['╭', '╮', '├', '┤']));
        for hash in [&stacked, &main, &feature, &root] {
            app.selected = app
                .commits
                .iter()
                .position(|row| &row.hash == hash)
                .unwrap();
            let selected = app.selected;
            press(&mut app, KeyCode::Enter);
            assert_eq!(app.history, History::Log);
            assert_eq!(app.mode, Mode::Log);
            assert_eq!(app.status, None);
            assert_eq!(app.commits.len(), 1);
            assert_eq!(&app.commits[0].hash, hash);
            press(&mut app, KeyCode::Enter);
            assert_eq!(app.mode, Mode::Show);
            assert_eq!(app.status, None);
            press(&mut app, KeyCode::Esc);
            assert_eq!(app.mode, Mode::Log);
            press(
                &mut app,
                if hash == &feature {
                    KeyCode::Backspace
                } else {
                    KeyCode::Esc
                },
            );
            assert_eq!(app.history, History::Branches);
            assert_eq!(app.selected, selected);
        }
        // Alias names participate in search, and a single click opens their log.
        app.begin_search(false);
        app.search_input = Some("alias".into());
        app.submit_search();
        assert_eq!(app.commits[app.selected].hash, feature);
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let row = app
            .visible_log_rows
            .iter()
            .position(|&row| row == Some(app.selected))
            .unwrap();
        let y = app.log_row_origin + row as u16;
        input::handle(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 4,
                row: y,
                modifiers: KeyModifiers::NONE,
            }),
            &mut app,
        );
        assert_eq!(app.history, History::Log);
        assert_eq!(app.commits[0].hash, feature);
        assert_eq!(git(&["symbolic-ref", "HEAD"]), "refs/heads/stacked");
    }

    #[test]
    fn merged_branches_keep_full_history_without_truncating_main() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        let tree = git(&["write-tree"]);
        let root = commit(&tree, "root", &[]);
        let main_line = commit(&tree, "main line", &[&root]);
        let review = commit(&tree, "review fixes", &[&root]);
        let merged = commit(&tree, "merge review fixes", &[&main_line, &review]);
        let main = commit(&tree, "after merge", &[&merged]);
        git(&["update-ref", "refs/heads/main", &main]);
        git(&["update-ref", "refs/heads/review-fixes", &review]);
        // A merged side tip alone must not cut the main Log. Adding an open
        // topic keeps its real shared ancestor as a cutoff, past the merged tip.
        for with_topic in [false, true] {
            if with_topic {
                let topic = commit(&tree, "open topic", &[&root]);
                git(&["update-ref", "refs/heads/topic", &topic]);
            }
            let mut app = interactive();
            let old = app
                .commits
                .iter()
                .position(|row| row.hash == review)
                .unwrap();
            assert!(app
                .log_format
                .text(&app.commits[old])
                .contains("(review-fixes) · merged into main"));
            assert!(!app
                .log_format
                .text(&app.commits[app.selected])
                .contains("merged into"));
            press(&mut app, KeyCode::Enter);
            assert_eq!(app.status, None);
            let hashes: HashSet<_> = app.commits.iter().map(|row| row.hash.as_str()).collect();
            let mut expected: HashSet<_> = [&main, &merged, &main_line, &review]
                .into_iter()
                .map(String::as_str)
                .collect();
            if !with_topic {
                expected.insert(&root);
            }
            assert_eq!(hashes, expected);
            press(&mut app, KeyCode::Esc);
            app.selected = old;
            press(&mut app, KeyCode::Enter);
            assert_eq!(app.status, None);
            assert_eq!(
                app.commits
                    .iter()
                    .map(|row| row.hash.as_str())
                    .collect::<HashSet<_>>(),
                [review.as_str(), root.as_str()].into()
            );
            press(&mut app, KeyCode::Esc);
            assert_eq!(app.history, History::Branches);
            assert_eq!(app.selected, old);
        }
    }

    #[test]
    fn fast_forwarded_branch_does_not_become_a_log_cutoff() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        let tree = git(&["write-tree"]);
        let root = commit(&tree, "root", &[]);
        let review = commit(&tree, "review fixes", &[&root]);
        let main = commit(&tree, "after fast forward", &[&review]);
        git(&["update-ref", "refs/heads/main", &main]);
        git(&["update-ref", "refs/heads/review-fixes", &review]);
        let mut app = interactive();
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.status, None);
        assert_eq!(app.commits.len(), 3);
        assert!(app.commits.iter().any(|row| row.hash == root));
    }

    #[test]
    fn branch_tips_peel_annotated_tags() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        let tree = git(&["write-tree"]);
        let root = commit(&tree, "root", &[]);
        let main = commit(&tree, "main", &[&root]);
        let side = commit(&tree, "side", &[&root]);
        git(&["update-ref", "refs/heads/main", &main]);
        git(&["update-ref", "refs/heads/side", &side]);
        git(&["tag", "-a", "-m", "tag", "tagged", &side]);
        let tag = git(&["rev-parse", "refs/tags/tagged"]);
        // Git refuses to point a branch at a tag, but other tools can.
        std::fs::write(".git/refs/heads/tagged", format!("{tag}\n")).unwrap();
        git(&["update-ref", "refs/remotes/origin/main", &tag]);
        let app = interactive();
        let tip = |name: &str| {
            app.commits
                .iter()
                .find(|row| {
                    row.branch()
                        .is_some_and(|node| node.branches.iter().any(|branch| branch.name == name))
                })
                .map(|row| row.hash.as_str())
        };
        assert_eq!(tip("tagged"), Some(side.as_str()));
        assert_eq!(tip("origin/main"), Some(side.as_str()));
    }

    #[test]
    fn merged_status_uses_the_local_remote_default_instead_of_the_checkout() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "trunk"]);
        let tree = git(&["write-tree"]);
        let root = commit(&tree, "root", &[]);
        let trunk = commit(&tree, "trunk", &[&root]);
        let feature = commit(&tree, "feature", &[&root]);
        let stacked = commit(&tree, "stacked", &[&feature]);
        for (name, hash) in [
            ("trunk", &trunk),
            ("main", &root),
            ("feature", &feature),
            ("stacked", &stacked),
        ] {
            git(&["update-ref", &format!("refs/heads/{name}"), hash]);
        }
        git(&["update-ref", "refs/remotes/origin/trunk", &trunk]);
        git(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ]);
        git(&["symbolic-ref", "HEAD", "refs/heads/stacked"]);
        let app = interactive();
        let branch = |name: &str| {
            app.commits
                .iter()
                .filter_map(Commit::branch)
                .flat_map(|node| &node.branches)
                .find(|branch| branch.name == name)
                .unwrap()
        };
        assert_eq!(branch("main").merged_into.as_deref(), Some("trunk"));
        assert_eq!(branch("trunk").merged_into, None);
        assert_eq!(branch("feature").merged_into, None);
        assert_eq!(branch("stacked").merged_into, None);
        assert!(branch("stacked").current);
    }

    #[test]
    fn remote_default_names_the_base_of_local_main_and_feature_logs() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        let tree = git(&["write-tree"]);
        let root = commit(&tree, "root", &[]);
        let base = commit(&tree, "published main", &[&root]);
        let main = commit(&tree, "local main work", &[&base]);
        let feature = commit(&tree, "feature work", &[&base]);
        git(&["update-ref", "refs/heads/main", &main]);
        git(&["update-ref", "refs/remotes/origin/main", &base]);
        // Keep the single-local-branch fast path even with a tracking ref.
        assert!(matches!(load(&[]).unwrap(), Launch::Print(_)));
        git(&["update-ref", "refs/heads/feature", &feature]);
        // Recognize origin/main even in a repository lacking origin/HEAD.
        let mut app = interactive();
        let ancestor = app.commits.iter().position(|row| row.hash == base).unwrap();
        assert!(app
            .log_format
            .text(&app.commits[ancestor])
            .contains("(origin/main)"));
        assert_eq!(app.commits.len(), 3);
        for hash in [&main, &feature] {
            app.selected = app
                .commits
                .iter()
                .position(|row| &row.hash == hash)
                .unwrap();
            press(&mut app, KeyCode::Enter);
            assert_eq!(app.status, None);
            assert_eq!(app.commits.len(), 1);
            assert_eq!(&app.commits[0].hash, hash);
            press(&mut app, KeyCode::Esc);
        }
        app.selected = ancestor;
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.commits
                .iter()
                .map(|row| row.hash.as_str())
                .collect::<HashSet<_>>(),
            [base.as_str(), root.as_str()].into()
        );
        // A configured remote default can have a different name and may
        // advance beyond the actual common ancestor of the local branches.
        let upstream = commit(&tree, "upstream advanced", &[&base]);
        git(&["update-ref", "refs/remotes/origin/trunk", &upstream]);
        git(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ]);
        let app = interactive();
        assert_eq!(app.commits.len(), 4);
        let named = app.commits.iter().find(|row| row.hash == upstream).unwrap();
        assert!(app.log_format.text(named).contains("origin/trunk"));
        assert!(!app.log_format.text(named).contains("merged into"));
        let fork = app.commits.iter().find(|row| row.hash == base).unwrap();
        assert!(fork.branch().unwrap().branches.is_empty());
        assert!(!app
            .commits
            .iter()
            .filter_map(Commit::branch)
            .flat_map(|node| &node.branches)
            .any(|branch| branch.name == "origin/main"));
        // When local and remote tips coincide they share one node, with
        // the remote label retained rather than marked merged into main.
        git(&["update-ref", "refs/heads/main", &upstream]);
        let app = interactive();
        let named = app.commits.iter().find(|row| row.hash == upstream).unwrap();
        assert!(app.log_format.text(named).contains("HEAD -> main"));
        assert!(app.log_format.text(named).contains("origin/trunk"));
    }

    #[test]
    fn branch_nodes_use_git_decorations_for_head_tags_and_other_remotes() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        let tree = git(&["write-tree"]);
        let root = commit(&tree, "root", &[]);
        let main = commit(&tree, "main", &[&root]);
        git(&["update-ref", "refs/heads/main", &main]);
        git(&["update-ref", "refs/heads/feature", &root]);
        git(&["tag", "-a", "published", "-m", "published", &root]);
        git(&["update-ref", "refs/remotes/upstream/main", &root]);
        let app = interactive();
        for row in &app.commits {
            let expected = git(&[
                "log",
                "--no-walk",
                "--decorate=short",
                "--format=%D",
                &row.hash,
            ]);
            assert_eq!(row.decorations, expected);
            assert!(app.log_format.text(row).contains(&format!("({expected})")));
        }
        let root = app.commits.iter().find(|row| row.hash == root).unwrap();
        assert!(root.decorations.contains("tag: published"));
        assert!(root.decorations.contains("upstream/main"));
        assert!(app.commits[app.selected]
            .decorations
            .contains("HEAD -> main"));
    }

    #[test]
    fn branch_update_dates_follow_ref_moves_and_the_newest_alias() {
        use std::process::Command;
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        let dated = |args: &[&str], date: &str| {
            let output = Command::new("git")
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00+00:00")
                .env("GIT_COMMITTER_DATE", date)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        let tree = git(&["write-tree"]);
        let base = dated(
            &["commit-tree", &tree, "-m", "shared base"],
            "1999-01-01T00:00:00+00:00",
        );
        let root = dated(
            &["commit-tree", &tree, "-p", &base, "-m", "root"],
            "2001-01-01T00:00:00+00:00",
        );
        let main = dated(
            &["commit-tree", &tree, "-p", &root, "-m", "main"],
            "2001-01-02T00:00:00+00:00",
        );
        dated(
            &["update-ref", "--create-reflog", "refs/heads/main", &main],
            "2019-01-01T00:00:00+00:00",
        );
        dated(
            &["update-ref", "--create-reflog", "refs/heads/feature", &main],
            "2018-01-01T00:00:00+00:00",
        );
        dated(
            &["update-ref", "--create-reflog", "refs/heads/feature", &root],
            "2020-01-03T00:00:00+00:00",
        );
        dated(
            &["update-ref", "--create-reflog", "refs/heads/alias", &main],
            "2021-01-04T00:00:00+00:00",
        );
        git(&["config", "core.logAllRefUpdates", "false"]);
        let side = dated(
            &["commit-tree", &tree, "-p", &base, "-m", "side"],
            "2001-01-03T00:00:00+00:00",
        );
        git(&["update-ref", "refs/heads/side", &side]);
        let orphan = dated(
            &["commit-tree", &tree, "-m", "no reflog"],
            "2002-01-01T00:00:00+00:00",
        );
        git(&["update-ref", "refs/heads/without-reflog", &orphan]);
        git(&["config", "log.date", "short"]);
        let mut app = interactive();
        let updated = |app: &App, hash: &str| {
            app.commits
                .iter()
                .find(|row| row.hash == hash)
                .unwrap()
                .branch()
                .unwrap()
                .updated_at
                .clone()
        };
        assert_eq!(updated(&app, &main).as_deref(), Some("2021-01-04"));
        assert_eq!(updated(&app, &root).as_deref(), Some("2020-01-03"));
        assert_eq!(updated(&app, &orphan).as_deref(), Some("2002-01-01"));
        assert_eq!(updated(&app, &base).as_deref(), Some("1999-01-01"));
        assert!(app
            .commits
            .iter()
            .find(|row| row.hash == base)
            .unwrap()
            .branch()
            .unwrap()
            .branches
            .is_empty());
        let row = &app.commits[app.selected];
        assert!(app.log_format.text(row).contains("2021-01-04"));
        assert!(app.log_format.text(row).contains("HEAD -> main"));
        press(&mut app, KeyCode::Char('d'));
        assert!(!app
            .log_format
            .text(&app.commits[app.selected])
            .contains("2021-01-04"));
        assert!(app
            .log_format
            .text(&app.commits[app.selected])
            .contains("HEAD -> main"));
        let Launch::Interactive(app) = load(&["--date=iso-strict".into()]).unwrap() else {
            panic!("expected graph")
        };
        assert!(updated(&app, &root)
            .unwrap()
            .starts_with("2020-01-03T00:00:00"));
        assert!(updated(&app, &base)
            .unwrap()
            .starts_with("1999-01-01T00:00:00"));
        assert!(load(&["--date=format:%n".into()]).is_err());
        assert!(load(&["--date=invalid-date-style".into()]).is_err());
    }

    #[test]
    fn trivial_lists_exit_but_detached_and_same_tip_branches_stay_interactive() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        assert!(matches!(load(&[]).unwrap(), Launch::Print(text) if text.is_empty()));
        let tree = git(&["write-tree"]);
        let root = commit(&tree, "root", &[]);
        git(&["update-ref", "refs/heads/main", &root]);
        assert!(
            matches!(load(&[]).unwrap(), Launch::Print(text) if text.trim() == git(&["branch", "--list"]))
        );
        git(&["branch", "alias"]);
        assert_eq!(interactive().commits.len(), 1);
        git(&["branch", "-D", "alias"]);
        git(&["checkout", "-q", "--detach"]);
        let app = interactive();
        assert_eq!(app.commits.len(), 1);
        assert!(!app.commits[0].branch().unwrap().branches[0].current);
        for args in [vec!["-d", "main"], vec!["new-branch"], vec!["--all"]] {
            assert!(load(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err());
        }
    }
}
