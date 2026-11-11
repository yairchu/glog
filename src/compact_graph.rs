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
