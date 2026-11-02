//! Reflog events stay in time order, so ancestry can run in either direction.
//! Nodes and side-path arrowheads point from parents into children; ref
//! movements use hollow nodes and true merges use diamonds.
//! Every event occupies exactly one line, even at forks and crossings.
use std::collections::{BTreeSet, HashMap, HashSet};

use crate::git::{Annotation, Commit};

use super::{paint, Drawing, Fold, GraphRows, Layout, Plan, PALETTE};

#[derive(Default)]
struct Occurrences<'a> {
    all: HashMap<&'a str, Vec<usize>>,
    by_ref: HashMap<(&'a str, &'a str), Vec<usize>>,
}

fn reference(commit: &Commit) -> &str {
    match &commit.annotation {
        Some(Annotation::Reflog(entry)) => &entry.reference,
        _ => "",
    }
}

impl<'a> Occurrences<'a> {
    fn new(commits: &'a [Commit], visible: &[bool]) -> Self {
        let mut result = Self::default();
        for (row, commit) in commits.iter().enumerate().filter(|&(row, _)| visible[row]) {
            result.all.entry(&commit.hash).or_default().push(row);
            result
                .by_ref
                .entry((&commit.hash, reference(commit)))
                .or_default()
                .push(row);
        }
        result
    }

    fn nearest(&self, hash: &str, reference: &str, source: usize) -> Option<usize> {
        let rows = self
            .by_ref
            .get(&(hash, reference))
            .or_else(|| self.all.get(hash))?;
        let at = rows.partition_point(|&row| row < source);
        // On equal distances prefer the newer visit, making a recent reset
        // a useful endpoint rather than always connecting to the old visit.
        [at.checked_sub(1), (at < rows.len()).then_some(at)]
            .into_iter()
            .flatten()
            .map(|at| rows[at])
            .filter(|&row| row != source)
            .min_by_key(|&row| (row.abs_diff(source), row))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Edge {
    child: usize,
    parent: usize,
}

impl Edge {
    fn start(self) -> usize {
        self.child.min(self.parent)
    }

    fn end(self) -> usize {
        self.child.max(self.parent)
    }
}

fn edges(commits: &[Commit], visible: &[bool]) -> Vec<Edge> {
    let occurrences = Occurrences::new(commits, visible);
    let by_hash: HashMap<_, _> = commits.iter().map(|c| (c.hash.as_str(), c)).collect();
    let mut result = Vec::new();
    for (child, commit) in commits.iter().enumerate().filter(|&(row, _)| visible[row]) {
        let mut pending: Vec<_> = commit.parents.iter().map(String::as_str).collect();
        let mut visited = HashSet::new();
        let mut parents = BTreeSet::new();
        while let Some(hash) = pending.pop() {
            if !visited.insert(hash) {
                continue;
            }
            if let Some(parent) = occurrences.nearest(hash, reference(commit), child) {
                parents.insert(parent);
            } else if let Some(hidden) = by_hash.get(hash) {
                // Only pass through loaded, folded steps. Missing ancestry
                // stays unconnected; time order alone never implies an edge.
                pending.extend(hidden.parents.iter().map(String::as_str));
            }
        }
        result.extend(parents.into_iter().map(|parent| Edge { child, parent }));
    }
    result.sort_by_key(|edge| (edge.start(), edge.end(), edge.child));
    result
}

const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

#[derive(Clone, Copy)]
enum Arrow {
    Left,
    Right,
}

#[derive(Clone, Default)]
struct Cell {
    paths: u8,
    arrow: Option<Arrow>,
    colour: usize,
}

impl Cell {
    fn symbol(&self, unicode: bool) -> char {
        if let Some(arrow) = self.arrow {
            return match (arrow, unicode) {
                (Arrow::Left, true) => '◀',
                (Arrow::Left, false) => '<',
                (Arrow::Right, true) => '▶',
                (Arrow::Right, false) => '>',
            };
        }
        if !unicode {
            return match self.paths {
                0 => ' ',
                1..=3 => '|',
                4 | 8 | 12 => '-',
                _ => '+',
            };
        }
        match self.paths {
            0 => ' ',
            1..=3 => '│',
            4 | 8 | 12 => '─',
            5 => '╯',
            6 => '╮',
            7 => '┤',
            9 => '╰',
            10 => '╭',
            11 => '├',
            13 => '┴',
            14 => '┬',
            _ => '┼',
        }
    }
}

/// The arrows describe visible ancestry, not the direction of ref movement.
/// A collapsed rebase finish represents the whole operation's new tip.
fn node(commit: &Commit, directions: u8, fold: Fold, unicode: bool) -> char {
    let visit = if let Some(Annotation::Reflog(entry)) = &commit.annotation {
        let action = entry.action.split_once(": ").map(|(action, _)| action);
        matches!(action, Some("checkout" | "reset" | "clone" | "branch"))
            || action.is_some_and(|action| match super::rebase_stage(action) {
                Some("finish") => !matches!(fold, Fold::Folded(_)),
                Some("start" | "abort" | "reset") => true,
                _ => false,
            })
    } else {
        false
    };
    let (unicode_node, ascii_node) = if visit {
        ('○', 'o')
    } else if commit.parents.len() > 1 {
        ('◆', 'M')
    } else {
        match directions {
            UP => ('▲', '^'),
            DOWN => ('▼', 'v'),
            3 => ('↕', '+'),
            _ => ('●', '*'),
        }
    };
    if unicode {
        unicode_node
    } else {
        ascii_node
    }
}

/// Connect a node to an incident lane on the event's own line. A crossing
/// with a continuing lane is drawn as ┼, without introducing a routing row.
fn connect(line: &mut [Cell], node: usize, lane: usize, toward_node: bool) {
    let from = 2 * node.min(lane);
    let to = 2 * node.max(lane);
    for column in from..to {
        line[column].paths |= RIGHT;
        line[column + 1].paths |= LEFT;
        line[column].colour = lane % PALETTE.len();
        line[column + 1].colour = lane % PALETTE.len();
    }
    // Arrowheads sit next to the child, in the gap between lanes, so they
    // cannot obscure a crossing or imply a merge at the parent endpoint.
    if toward_node && node != lane {
        let (column, arrow) = if lane > node {
            (2 * node + 1, Arrow::Left)
        } else {
            (2 * node - 1, Arrow::Right)
        };
        line[column].arrow = Some(arrow);
        line[column].colour = lane % PALETTE.len();
    }
}

pub(super) fn draw(commits: &[Commit], layout: &Layout) -> Drawing {
    let edges = edges(commits, &layout.visible);
    let mut starts = vec![Vec::new(); commits.len()];
    let mut directions = vec![0; commits.len()];
    for edge in edges {
        starts[edge.start()].push(edge);
        directions[edge.child] |= if edge.parent > edge.child { UP } else { DOWN };
    }

    // Sweep events in display order, treating an edge as an interval rather
    // than assuming its parent is older. Its endpoints retain their roles
    // for the node arrows. Ordinary sequences stay in a single column.
    let mut lanes: Vec<Option<Edge>> = Vec::new();
    let mut fixed = HashMap::new();
    for (row, commit) in commits
        .iter()
        .enumerate()
        .filter(|&(row, _)| layout.visible[row])
    {
        let incoming: Vec<_> = lanes
            .iter()
            .enumerate()
            .filter_map(|(lane, edge)| edge.filter(|edge| edge.end() == row).map(|_| lane))
            .collect();
        let column = incoming.first().copied().unwrap_or_else(|| {
            lanes
                .iter()
                .position(Option::is_none)
                .unwrap_or(lanes.len())
        });
        let above = lanes.clone();
        for &lane in &incoming {
            lanes[lane] = None;
        }
        lanes.resize(lanes.len().max(column + 1), None);
        let mut outgoing = Vec::new();
        for &edge in &starts[row] {
            let lane = if lanes[column].is_none() {
                column
            } else {
                lanes
                    .iter()
                    .position(Option::is_none)
                    .unwrap_or(lanes.len())
            };
            lanes.resize(lanes.len().max(lane + 1), None);
            lanes[lane] = Some(edge);
            outgoing.push(lane);
        }
        let mut line = vec![Cell::default(); 2 * lanes.len().max(above.len()) - 1];
        for (lane, edge) in above.iter().enumerate() {
            if edge.is_some() {
                line[2 * lane].paths |= UP;
                line[2 * lane].colour = lane % PALETTE.len();
            }
        }
        for (lane, edge) in lanes.iter().enumerate() {
            if edge.is_some() {
                line[2 * lane].paths |= DOWN;
                line[2 * lane].colour = lane % PALETTE.len();
            }
        }
        for (lane, edge) in incoming
            .iter()
            .map(|&lane| (lane, above[lane].unwrap()))
            .chain(outgoing.iter().map(|&lane| (lane, lanes[lane].unwrap())))
        {
            connect(&mut line, column, lane, edge.child == row);
        }
        // Keep the node, but trim ended lanes to its right. Reusing a lane
        // never shifts an unrelated continuing path across another event.
        let width = line
            .iter()
            .rposition(|cell| cell.paths != 0)
            .unwrap_or(0)
            .max(2 * column)
            + 1;
        let render = |unicode| {
            let mut text = String::new();
            for (at, cell) in line[..width].iter().enumerate() {
                if at == 2 * column {
                    text.push(node(commit, directions[row], layout.folds[row], unicode));
                } else {
                    let symbol = cell.symbol(unicode);
                    if symbol == ' ' {
                        text.push(' ');
                    } else {
                        text.push_str(&paint(symbol, PALETTE[cell.colour]));
                    }
                }
            }
            text.push(' ');
            vec![text]
        };
        fixed.insert(
            row,
            GraphRows {
                ascii: render(false),
                unicode: render(true),
            },
        );
        while lanes.last() == Some(&None) {
            lanes.pop();
        }
    }
    Drawing(std::rc::Rc::new(Plan {
        visible: layout.visible.clone(),
        fixed,
        ..Plan::default()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{git::ReflogEntry, log_folds::LogFolds};

    fn events(rows: &[(&str, &[&str], &str, &str)]) -> Vec<Commit> {
        rows.iter()
            .map(|&(hash, parents, reference, action)| {
                let mut commit = crate::log_format::tests::commit();
                commit.hash = hash.into();
                commit.graph_node = '*';
                commit.graph = vec![String::new()];
                commit.parents = parents.iter().map(|&parent| parent.into()).collect();
                commit.annotation = Some(Annotation::Reflog(ReflogEntry {
                    reference: reference.into(),
                    show_reference: true,
                    rebasing: false,
                    updated_at: "2026-11-02".into(),
                    actor: "Test".into(),
                    action: action.into(),
                }));
                commit
            })
            .collect()
    }

    fn plain(folds: &LogFolds, commits: &[Commit]) -> Vec<String> {
        commits
            .iter()
            .enumerate()
            .filter(|&(row, _)| folds.visible(row))
            .map(|(row, commit)| {
                let graph = folds.display_graph(row, commit);
                assert_eq!(graph.len(), 1);
                assert_eq!(folds.display_height(row, commit), 1);
                crate::ansi::plain(&graph[0])
            })
            .collect()
    }

    #[test]
    fn reset_visits_are_hollow_and_child_arrows_follow_ancestry_in_either_direction() {
        let commits = events(&[
            ("a", &[], "HEAD", "reset: moving to HEAD~1"),
            ("b", &["a"], "HEAD", "commit: b"),
            ("a", &[], "HEAD", "commit (initial): a"),
        ]);
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        assert_eq!(plain(&folds, &commits), ["○ ", "▼ ", "● "]);
        // Without the reset, ancestry points up from the older parent.
        folds.refresh(&commits[1..]).unwrap();
        assert_eq!(plain(&folds, &commits[1..]), ["▲ ", "● "]);
        folds.ascii_graph = true;
        assert_eq!(plain(&folds, &commits[1..]), ["^ ", "* "]);
    }

    #[test]
    fn interleaved_refs_prefer_their_own_visits_without_linking_checkouts() {
        let commits = events(&[
            ("b", &["a"], "HEAD", "commit: b"),
            ("a", &[], "refs/heads/main", "reset: moving to a"),
            ("x", &["missing"], "HEAD", "checkout: moving to topic"),
            ("a", &[], "HEAD", "commit (initial): a"),
        ]);
        assert_eq!(
            edges(&commits, &[true; 4]),
            [Edge {
                child: 0,
                parent: 3
            }]
        );
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        assert_eq!(plain(&folds, &commits), ["▲ ", "│ ○ ", "│ ○ ", "● "]);
        // --all can still supply a parent absent from this ref's own log.
        assert_eq!(
            edges(&commits[..3], &[true; 3]),
            [Edge {
                child: 0,
                parent: 1
            }]
        );
    }

    #[test]
    fn folded_rebases_project_ancestry_and_redraw_when_expanded_or_revealed() {
        let commits = events(&[
            (
                "c",
                &["b"],
                "HEAD",
                "rebase (finish): returning to refs/heads/topic",
            ),
            ("c", &["b"], "HEAD", "rebase (pick): c"),
            ("b", &["a"], "HEAD", "rebase (pick): b"),
            ("a", &[], "HEAD", "rebase (start): checkout main"),
            ("a", &[], "HEAD", "checkout: moving to main"),
        ]);
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        assert_eq!(plain(&folds, &commits), ["▲ ", "○ "]);
        folds.toggle(0, &commits).unwrap();
        assert_eq!(plain(&folds, &commits), ["○ ", "│ ▲ ", "▲─╯ ", "○ ", "○ "]);
        assert_eq!(
            edges(&commits, &[true; 5]),
            [
                Edge {
                    child: 0,
                    parent: 2
                },
                Edge {
                    child: 1,
                    parent: 2
                },
                Edge {
                    child: 2,
                    parent: 3
                },
            ]
        );
        folds.toggle(0, &commits).unwrap();
        folds.reveal(2, &commits);
        assert_eq!(plain(&folds, &commits).len(), 5);
    }

    #[test]
    fn merge_forks_share_event_rows_without_extra_routing_lines() {
        let commits = events(&[
            ("merge", &["left", "right"], "HEAD", "commit (merge): merge"),
            ("left", &["base"], "HEAD", "commit: left"),
            ("right", &["base"], "HEAD", "checkout: moving to right"),
            ("base", &[], "HEAD", "commit (initial): base"),
        ]);
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        let rows = plain(&folds, &commits);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows, ["◆◀╮ ", "▲ │ ", "│ ○ ", "●─╯ "]);
    }

    #[test]
    fn straight_history_uses_one_column_even_when_parents_are_missing() {
        let commits = events(&[
            ("c", &["b"], "HEAD", "commit: c"),
            ("b", &["a"], "HEAD", "commit: b"),
            ("a", &["missing"], "HEAD", "commit: a"),
        ]);
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        assert_eq!(plain(&folds, &commits), ["▲ ", "▲ ", "● "]);
    }

    #[test]
    fn amend_versions_branch_from_the_shared_parent_when_expanded() {
        let commits = events(&[
            ("new", &["a"], "HEAD", "commit (amend): new"),
            ("old", &["a"], "HEAD", "commit: old"),
            ("a", &[], "HEAD", "commit (initial): a"),
        ]);
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        assert_eq!(plain(&folds, &commits), ["▲ ", "● "]);
        folds.toggle(0, &commits).unwrap();
        assert_eq!(plain(&folds, &commits), ["▲ ", "│ ▲ ", "●─╯ "]);
    }

    #[test]
    fn merge_with_parents_on_both_sides_keeps_its_merge_marker() {
        let commits = events(&[
            ("left", &[], "HEAD", "commit (initial): left"),
            ("merge", &["left", "right"], "HEAD", "commit (merge): merge"),
            ("right", &[], "HEAD", "commit (initial): right"),
        ]);
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        assert_eq!(plain(&folds, &commits), ["● ", "◆ ", "● "]);
        folds.ascii_graph = true;
        assert_eq!(plain(&folds, &commits), ["* ", "M ", "* "]);
    }

    #[test]
    fn converging_paths_cross_a_continuing_lane_on_the_parent_row() {
        let commits = events(&[
            ("first", &["a"], "HEAD", "commit: first"),
            ("other", &["base"], "HEAD", "commit: other"),
            ("second", &["a"], "HEAD", "commit: second"),
            ("a", &[], "HEAD", "commit (initial): a"),
            ("base", &[], "HEAD", "commit (initial): base"),
        ]);
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        assert_eq!(
            plain(&folds, &commits),
            ["▲ ", "│ ▲ ", "│ │ ▲ ", "●─┼─╯ ", "  ● "]
        );
    }

    #[test]
    fn checkout_side_path_points_into_the_child_not_the_shared_parent() {
        // The tip has its parent at the checkout above it. The checkout
        // and the old version share the base below.
        let commits = events(&[
            (
                "rebased",
                &["base"],
                "HEAD",
                "checkout: moving from main to rebased",
            ),
            ("tip", &["rebased"], "HEAD", "commit: tip"),
            ("original", &["base"], "HEAD", "commit: original"),
            ("base", &[], "HEAD", "commit (initial): base"),
        ]);
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        assert_eq!(plain(&folds, &commits), ["○◀╮ ", "▼ │ ", "▲ │ ", "●─╯ "]);
        folds.ascii_graph = true;
        assert_eq!(plain(&folds, &commits), ["o<+ ", "v | ", "^ | ", "*-+ "]);
    }

    #[test]
    fn side_paths_can_enter_a_merge_from_the_left() {
        let commits = events(&[
            ("unrelated", &["root"], "HEAD", "commit: unrelated"),
            ("tip", &["merge"], "HEAD", "commit: tip"),
            ("root", &[], "HEAD", "commit (initial): root"),
            ("merge", &["left", "right"], "HEAD", "commit (merge): merge"),
            ("left", &[], "HEAD", "commit (initial): left"),
            ("right", &[], "HEAD", "commit (initial): right"),
        ]);
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        assert_eq!(
            plain(&folds, &commits),
            ["▲ ", "│ ▲ ", "● │ ", "╭▶◆ ", "│ ● ", "● "]
        );
        folds.ascii_graph = true;
        assert_eq!(
            plain(&folds, &commits),
            ["^ ", "| ^ ", "* | ", "+>M ", "| * ", "* "]
        );
    }

    #[test]
    fn side_paths_point_into_a_merge_below_its_parents() {
        let commits = events(&[
            ("left", &[], "HEAD", "commit (initial): left"),
            ("right", &[], "HEAD", "commit (initial): right"),
            ("merge", &["left", "right"], "HEAD", "commit (merge): merge"),
        ]);
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        assert_eq!(plain(&folds, &commits), ["● ", "│ ● ", "◆◀╯ "]);
        folds.ascii_graph = true;
        assert_eq!(plain(&folds, &commits), ["* ", "| * ", "M<+ "]);
    }
}
