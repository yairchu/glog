use std::collections::BTreeSet;

use crate::{diff::FileSection, git};

/// Categories can overlap: Go tests are both `.go` and `*_test.go`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    Directory(Vec<u8>),
    Extension(Vec<u8>),
    GoTests,
    Lockfiles,
}

impl Category {
    pub fn label(&self) -> String {
        match self {
            Self::Directory(path) => format!("{}**", crate::diff::display_path(path)),
            Self::Extension(ext) if ext.is_empty() => "(no extension)".into(),
            Self::Extension(ext) => format!(".{}", crate::diff::display_path(ext)),
            Self::GoTests => "*_test.go".into(),
            Self::Lockfiles => "lockfiles".into(),
        }
    }

    pub fn of(file: &FileSection) -> Vec<Self> {
        let path = git::raw_path(&file.path_bytes);
        let extension = path
            .extension()
            .map(|ext| git::path_bytes(std::path::Path::new(ext)).into_owned())
            .unwrap_or_default();
        let mut categories = vec![Self::Extension(extension)];
        if file.path_bytes.ends_with(b"_test.go") {
            categories.push(Self::GoTests);
        }
        if file.lockfile {
            categories.push(Self::Lockfiles);
        }
        categories
    }

    pub fn selected(file: &FileSection) -> Self {
        // Hide the most specific category from the keyboard.
        Self::of(file)
            .pop()
            .expect("every file has an extension category")
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Filters {
    hidden: BTreeSet<Category>,
}

impl Filters {
    pub fn toggle(&mut self, category: Category) {
        if !self.hidden.remove(&category) {
            self.hidden.insert(category);
        }
    }

    /// Scope directory filters to a nested repository; extension filters apply
    /// everywhere, while unrelated root directories must not hide child files.
    pub fn within(&self, directory: &[u8]) -> Self {
        let prefix = crate::path_groups::join(directory, b"");
        Self {
            hidden: self
                .hidden
                .iter()
                .filter_map(|category| match category {
                    Category::Directory(path) => path
                        .strip_prefix(prefix.as_slice())
                        .map(|path| Category::Directory(path.to_vec())),
                    category => Some(category.clone()),
                })
                .collect(),
        }
    }

    pub fn clear(&mut self) {
        self.hidden.clear();
    }

    pub fn directories(&self) -> impl Iterator<Item = &Vec<u8>> {
        self.hidden.iter().filter_map(|category| match category {
            Category::Directory(path) => Some(path),
            _ => None,
        })
    }

    pub fn hides_category(&self, category: &Category) -> bool {
        self.hidden.contains(category)
    }

    pub fn hides(&self, file: &FileSection) -> bool {
        if self.hidden.is_empty() {
            return false;
        }
        self.directories()
            .any(|path| file.path_bytes.starts_with(path))
            || Category::of(file)
                .iter()
                .any(|category| self.hidden.contains(category))
    }
}
