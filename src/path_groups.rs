//! Compact inline paths shared by Grep and file summaries.

/// Ancestors covering the same visible contents offer the same fold. Merge
/// their labels and retain the deepest target, without adding directory rows.
pub fn compact<T>(parts: impl IntoIterator<Item = (String, T, usize)>) -> Vec<(String, T)> {
    let mut groups: Vec<(String, T, usize)> = Vec::new();
    for (label, target, count) in parts {
        if let Some((previous, old_target, old_count)) = groups.last_mut() {
            if *old_count == count {
                previous.push_str(&label);
                *old_target = target;
                continue;
            }
        }
        groups.push((label, target, count));
    }
    groups
        .into_iter()
        .map(|(label, target, _)| (label, target))
        .collect()
}

#[derive(Clone, Default)]
pub struct Cursor {
    from_end: usize,
}

impl Cursor {
    pub fn index(&self, count: usize) -> Option<usize> {
        count
            .checked_sub(1)
            .map(|last| last - self.from_end.min(last))
    }

    pub fn select(&mut self, index: usize, count: usize) {
        self.from_end = count.saturating_sub(index.saturating_add(1));
    }

    pub fn step(&mut self, right: bool, count: usize) {
        self.from_end = self.from_end.min(count.saturating_sub(1));
        if right {
            self.from_end = self.from_end.saturating_sub(1);
        } else {
            self.from_end = (self.from_end + 1).min(count.saturating_sub(1));
        }
    }
}

/// Raw repository-relative directory boundaries, retaining their trailing slash.
pub fn directories(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    path.iter()
        .enumerate()
        .filter_map(|(i, byte)| (*byte == b'/').then_some(&path[..=i]))
}

pub fn join(directory: &[u8], path: &[u8]) -> Vec<u8> {
    let mut full = directory.to_vec();
    full.push(b'/');
    full.extend_from_slice(path);
    full
}
