//! Shared ancestry projection for compact graphs and expandable connections.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

pub struct Node {
    pub hash: String,
    pub parents: Vec<usize>,
}

pub struct Graph {
    pub nodes: Vec<Node>,
    pub indices: HashMap<String, usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Edge {
    pub from: String,
    pub to: String,
}

pub struct Section {
    pub edge: Edge,
    pub members: Vec<String>,
}

impl Graph {
    pub fn parse(text: &str) -> Result<Self, String> {
        let records: Vec<Vec<_>> = text
            .lines()
            .map(|line| line.split_whitespace().collect())
            .filter(|fields: &Vec<_>| !fields.is_empty())
            .collect();
        let indices: HashMap<String, usize> = records
            .iter()
            .enumerate()
            .map(|(index, fields)| (fields[0].to_owned(), index))
            .collect();
        let nodes: Vec<_> = records
            .iter()
            .map(|fields| Node {
                hash: fields[0].into(),
                parents: fields[1..]
                    .iter()
                    .filter_map(|hash| indices.get(*hash).copied())
                    .collect(),
            })
            .collect();
        if nodes
            .iter()
            .enumerate()
            .any(|(index, node)| node.parents.iter().any(|parent| *parent <= index))
        {
            return Err("commit ancestry is not in topological order".into());
        }
        Ok(Self { nodes, indices })
    }

    fn frontiers(&self, shown: &[bool]) -> Vec<BTreeSet<usize>> {
        let mut frontiers = vec![BTreeSet::new(); self.nodes.len()];
        for index in (0..self.nodes.len()).rev() {
            frontiers[index] = if shown[index] {
                BTreeSet::from([index])
            } else {
                self.nodes[index]
                    .parents
                    .iter()
                    .flat_map(|parent| frontiers[*parent].iter().copied())
                    .collect()
            };
        }
        frontiers
    }

    /// Connect shown nodes through omitted history. Branch pickers prune
    /// redundant ancestry links; expanded graphs preserve the actual routes.
    pub fn project(&self, shown: &[bool], prune: bool) -> HashMap<usize, Vec<usize>> {
        let frontiers = self.frontiers(shown);
        let mut parents = HashMap::<usize, Vec<usize>>::new();
        for index in (0..self.nodes.len()).rev().filter(|index| shown[*index]) {
            let frontier: BTreeSet<_> = self.nodes[index]
                .parents
                .iter()
                .flat_map(|parent| frontiers[*parent].iter().copied())
                .collect();
            let nearest = if prune && frontier.len() > 1 {
                frontier
                    .iter()
                    .copied()
                    .filter(|candidate| {
                        !frontier.iter().any(|other| {
                            other != candidate && reaches(&parents, *other, *candidate)
                        })
                    })
                    .collect()
            } else {
                frontier.into_iter().collect()
            };
            parents.insert(index, nearest);
        }
        parents
    }

    /// Also show omitted commits where routes split toward different shown
    /// commits or join from different ones, so no omitted commit belongs to
    /// more than one section.
    pub fn with_junctions(&self, shown: &[bool]) -> Vec<bool> {
        let mut shown = shown.to_vec();
        loop {
            let mut changed = false;
            // Parents follow children, so each pass sees its inputs settled.
            let mut frontiers = vec![BTreeSet::new(); self.nodes.len()];
            for index in (0..self.nodes.len()).rev() {
                let routes: BTreeSet<_> = self.nodes[index]
                    .parents
                    .iter()
                    .map(|parent| &frontiers[*parent])
                    .filter(|frontier| !frontier.is_empty())
                    .collect();
                if !shown[index] && routes.len() > 1 {
                    shown[index] = true;
                    changed = true;
                }
                frontiers[index] = if shown[index] {
                    BTreeSet::from([index])
                } else {
                    routes.into_iter().next().cloned().unwrap_or_default()
                };
            }
            let mut arrivals = vec![BTreeSet::<BTreeSet<usize>>::new(); self.nodes.len()];
            for index in 0..self.nodes.len() {
                let routes = std::mem::take(&mut arrivals[index]);
                if !shown[index] && routes.len() > 1 {
                    shown[index] = true;
                    changed = true;
                }
                let sources = if shown[index] {
                    BTreeSet::from([index])
                } else {
                    routes.into_iter().next().unwrap_or_default()
                };
                if !sources.is_empty() {
                    for parent in &self.nodes[index].parents {
                        arrivals[*parent].insert(sources.clone());
                    }
                }
            }
            if !changed {
                return shown;
            }
        }
    }

    /// Every connection's distinct omitted commits, stopping at shown nodes.
    /// A merge may reach the same commit by several paths; sets deduplicate it.
    pub fn sections(&self, shown: &[bool]) -> Vec<Section> {
        let frontiers = self.frontiers(shown);
        let mut sections = Vec::new();
        for (index, node) in self
            .nodes
            .iter()
            .enumerate()
            .filter(|(index, _)| shown[*index])
        {
            let mut members = BTreeMap::<usize, BTreeSet<usize>>::new();
            let mut seen = HashSet::new();
            let mut todo = node.parents.clone();
            while let Some(hidden) = todo.pop() {
                if shown[hidden] || !seen.insert(hidden) {
                    continue;
                }
                for target in &frontiers[hidden] {
                    members.entry(*target).or_default().insert(hidden);
                }
                todo.extend(self.nodes[hidden].parents.iter().copied());
            }
            for (target, members) in members {
                sections.push(Section {
                    edge: Edge {
                        from: self.nodes[index].hash.clone(),
                        to: self.nodes[target].hash.clone(),
                    },
                    members: members
                        .into_iter()
                        .map(|member| self.nodes[member].hash.clone())
                        .collect(),
                });
            }
        }
        sections
    }
}

fn reaches(parents: &HashMap<usize, Vec<usize>>, from: usize, target: usize) -> bool {
    let mut todo = vec![from];
    let mut seen = HashSet::new();
    while let Some(node) = todo.pop() {
        if node == target {
            return true;
        }
        if node < target && seen.insert(node) {
            todo.extend(parents.get(&node).into_iter().flatten().copied());
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disjoint_sections(ancestry: &str, landmarks: &[&str]) -> Vec<(String, String, Vec<String>)> {
        let graph = Graph::parse(ancestry).unwrap();
        let shown: Vec<_> = graph
            .nodes
            .iter()
            .map(|node| landmarks.contains(&node.hash.as_str()))
            .collect();
        let sections = graph.sections(&graph.with_junctions(&shown));
        let mut owners = HashMap::new();
        for section in &sections {
            for member in &section.members {
                assert_eq!(
                    owners.insert(member, &section.edge),
                    None,
                    "{member} is shared"
                );
            }
        }
        let mut sections: Vec<_> = sections
            .into_iter()
            .map(|section| (section.edge.from, section.edge.to, section.members))
            .collect();
        sections.sort();
        sections
    }

    fn section(from: &str, to: &str, members: &[&str]) -> (String, String, Vec<String>) {
        (
            from.into(),
            to.into(),
            members.iter().map(|member| (*member).into()).collect(),
        )
    }

    #[test]
    fn omitted_routes_split_and_join_at_shown_junctions() {
        // An omitted merge leads to two landmarks.
        assert_eq!(
            disjoint_sections(
                "bad c\nc x\nx a1 a2\na1 good1\na2 good2\ngood1\ngood2\n",
                &["bad", "good1", "good2"],
            ),
            [
                section("bad", "x", &["c"]),
                section("x", "good1", &["a1"]),
                section("x", "good2", &["a2"]),
            ]
        );
        // One route passes the split; another reaches the same commit around it.
        assert_eq!(
            disjoint_sections(
                "bad p1 p2\np1 a b\np2 h\na h\nb good2\nh good1\ngood1\ngood2\n",
                &["bad", "good1", "good2"],
            ),
            [
                section("bad", "h", &["p2"]),
                section("p1", "good2", &["b"]),
                section("p1", "h", &["a"]),
            ]
        );
    }
}
