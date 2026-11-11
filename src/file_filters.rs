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

    /// A submodule's entry stands for its directory rather than being a file,
    /// so it belongs to no file category; its own files carry theirs.
    pub fn of(file: &FileSection) -> Vec<Self> {
        if file.submodule.is_some() {
            return Vec::new();
        }
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
        if file.submodule.is_some() {
            return Self::Directory(crate::path_groups::join(&file.path_bytes, b""));
        }
        // Hide the most specific category from the keyboard.
        Self::of(file)
            .pop()
            .expect("every file has an extension category")
    }

    /// Rebase a nested repository's category onto its path in this one.
    pub fn under(self, directory: &[u8]) -> Self {
        match self {
            Self::Directory(path) => Self::Directory(crate::path_groups::join(directory, &path)),
            category => category,
        }
    }
}

/// Whether `directory` holds the file at `path`. A submodule's entry stands
/// for the directory of its files, so hiding that directory hides the entry.
pub fn in_directory(file: &FileSection, path: &[u8], directory: &[u8]) -> bool {
    if file.submodule.is_some() {
        crate::path_groups::join(path, b"").starts_with(directory)
    } else {
        path.starts_with(directory)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Filters {
    hidden: BTreeSet<Category>,
}

impl Default for Filters {
    /// Lockfile churn rarely needs reading, so it starts hidden.
    fn default() -> Self {
        Self {
            hidden: BTreeSet::from([Category::Lockfiles]),
        }
    }
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
            .any(|directory| in_directory(file, &file.path_bytes, directory))
            || Category::of(file)
                .iter()
                .any(|category| self.hidden.contains(category))
    }
}
