use std::{
    collections::{BTreeMap, HashSet},
    ops::Range,
};

use crate::{
    diff::{self, FileSection},
    file_filters::Filters,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Directory(Vec<u8>),
    File,
}

#[derive(Clone, Debug)]
pub struct Path {
    pub components: Vec<(String, Target)>,
    pub directory: Option<Vec<u8>>,
    pub offset: usize,
}

impl Path {
    pub fn ranges(&self) -> Vec<(Range<usize>, &Target)> {
        let mut offset = self.offset;
        self.components
            .iter()
            .map(|(label, target)| {
                let start = offset;
                offset += label.len();
                (start..offset, target)
            })
            .collect()
    }

    pub fn focused(&self, cursor: &crate::path_groups::Cursor) -> Option<&Target> {
        Some(&self.components[cursor.index(self.components.len())?].1)
    }
}

impl Target {
    pub fn under(&self, directory: &[u8]) -> Self {
        match self {
            Self::Directory(path) => Self::Directory(crate::path_groups::join(directory, path)),
            Self::File => Self::File,
        }
    }
}

pub struct Group {
    pub path: Vec<u8>,
    pub members: Vec<usize>,
}

#[derive(Default)]
pub struct Groups {
    groups: BTreeMap<Vec<u8>, Group>,
}

pub fn key(path: &[u8], layer: Option<diff::StashLayer>) -> Vec<u8> {
    let mut key = path.to_vec();
    if let Some(layer) = layer {
        key.push(0);
        key.extend_from_slice(layer.label().as_bytes());
    }
    key
}

impl Groups {
    pub fn new(files: &[FileSection], filters: &Filters) -> Self {
        let mut groups: BTreeMap<_, Group> = BTreeMap::new();
        for (index, file) in files
            .iter()
            .enumerate()
            .filter(|(_, file)| !filters.hides(file))
        {
            for prefix in crate::path_groups::directories(&file.path_bytes) {
                groups
                    .entry(key(prefix, file.stash_layer))
                    .or_insert_with(|| Group {
                        path: prefix.to_vec(),
                        members: Vec::new(),
                    })
                    .members
                    .push(index);
            }
        }
        Self { groups }
    }

    pub fn collapsed(
        &self,
        file: &FileSection,
        collapsed: &HashSet<Vec<u8>>,
    ) -> Option<(&Vec<u8>, &Group)> {
        crate::path_groups::directories(&file.path_bytes).find_map(|prefix| {
            collapsed
                .contains(&key(prefix, file.stash_layer))
                .then(|| self.groups.get_key_value(&key(prefix, file.stash_layer)))
                .flatten()
        })
    }

    pub fn layout(&self, file: &FileSection, directory: Option<&[u8]>) -> Path {
        if directory.is_none() && file.summary_path() != file.path {
            return Path {
                components: vec![(file.summary_path().to_owned(), Target::File)],
                directory: None,
                offset: "▶ ".len(),
            };
        }
        let mut parts = Vec::new();
        let mut previous = 0;
        for prefix in crate::path_groups::directories(&file.path_bytes) {
            if !self.groups.contains_key(&key(prefix, file.stash_layer)) {
                continue;
            }
            let label = diff::display_path(prefix);
            parts.push((
                label[previous..].to_owned(),
                Target::Directory(prefix.to_vec()),
            ));
            previous = label.len();
            if directory == Some(prefix) {
                break;
            }
        }
        if directory.is_none() {
            parts.push((file.path[previous..].to_owned(), Target::File));
        }
        Path {
            // Every ancestor is a distinct filter target, even when it has
            // the same visible files as its child or the file itself.
            components: parts,
            directory: directory.map(<[u8]>::to_vec),
            offset: "▶ ".len(),
        }
    }
}
