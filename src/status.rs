use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Paragraph},
    Frame,
};
use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    hash::{Hash, Hasher},
    io::{self, Read},
    path::PathBuf,
    process::Command,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Group {
    Conflicts,
    Staged,
    Unstaged,
    Untracked,
}
impl Group {
    fn title(self) -> &'static str {
        match self {
            Self::Conflicts => "Conflicts",
            Self::Staged => "Staged",
            Self::Unstaged => "Unstaged",
            Self::Untracked => "Untracked",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    group: Group,
    path: String,
}
#[derive(Clone, Debug)]
struct Entry {
    record: String,
    key: Key,
    label: String,
    original: Option<String>,
    // Keys display lossy UTF-8; Git and the filesystem need the exact bytes.
    raw_path: PathBuf,
    raw_original: Option<PathBuf>,
}
impl Entry {
    fn dirty_submodule(&self) -> bool {
        self.key.group == Group::Unstaged
            && self.record.split(' ').nth(2).is_some_and(|state| {
                state.starts_with('S')
                    && (state.ends_with('U') || state.as_bytes().get(2) == Some(&b'M'))
            })
    }
}
#[derive(Default)]
struct Snapshot {
    branch: String,
    upstream: String,
    ahead_behind: String,
    initial: bool,
    entries: Vec<Entry>,
}

// Status and diff have independent rename settings. Use the same detection
// threshold so each status entry corresponds to its patch and numstat record.
const RENAME_DETECTION: &str = "--find-renames=50%";

fn raw_path(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::OsStr::from_bytes(bytes).into()
    }
    #[cfg(not(unix))]
    {
        String::from_utf8_lossy(bytes).into_owned().into()
    }
}

fn parse(bytes: &[u8]) -> Result<Snapshot, String> {
    let mut snapshot = Snapshot::default();
    let mut records = bytes
        .split(|&byte| byte == 0)
        .filter(|record| !record.is_empty());
    while let Some(raw) = records.next() {
        let record = &*String::from_utf8_lossy(raw);
        if let Some(branch) = record.strip_prefix("# branch.head ") {
            snapshot.branch = branch.to_owned();
            continue;
        }
        if let Some(upstream) = record.strip_prefix("# branch.upstream ") {
            snapshot.upstream = upstream.to_owned();
            continue;
        }
        if let Some(ab) = record.strip_prefix("# branch.ab ") {
            snapshot.ahead_behind = ab
                .split_once(' ')
                .map(|(ahead, behind)| {
                    format!(
                        "ahead {}, behind {}",
                        ahead.trim_start_matches('+'),
                        behind.trim_start_matches('-')
                    )
                })
                .unwrap_or_else(|| ab.to_owned());
            continue;
        }
        if record == "# branch.oid (initial)" {
            snapshot.initial = true;
            continue;
        }
        if record.starts_with('#') {
            continue;
        }
        if let Some(path) = record.strip_prefix("? ") {
            snapshot.entries.push(Entry {
                record: record.to_owned(),
                key: Key {
                    group: Group::Untracked,
                    path: path.to_owned(),
                },
                label: "new".into(),
                original: None,
                raw_path: raw_path(&raw[2..]),
                raw_original: None,
            });
            continue;
        }
        let expected = match record.as_bytes()[0] {
            b'1' => 9,
            b'2' => 10,
            b'u' => 11,
            _ => return Err("Unrecognized Git status record".into()),
        };
        let fields: Vec<_> = record.splitn(expected, ' ').collect();
        if fields.len() != expected || fields[1].len() != 2 {
            return Err("Malformed Git status record".into());
        }
        let path = fields.last().unwrap().to_string();
        let path_bytes = raw.splitn(expected, |&byte| byte == b' ').last().unwrap();
        let raw_path = raw_path(path_bytes);
        let raw_original = if record.starts_with("2 ") {
            Some(records.next().ok_or("Missing rename source")?)
        } else {
            None
        };
        let original = raw_original.map(|raw| String::from_utf8_lossy(raw).into_owned());
        let raw_original = raw_original.map(self::raw_path);
        if record.starts_with("u ") {
            snapshot.entries.push(Entry {
                record: record.to_owned(),
                key: Key {
                    group: Group::Conflicts,
                    path,
                },
                label: match fields[1] {
                    "DD" => "both deleted",
                    "AU" => "added by us",
                    "UD" => "deleted by them",
                    "UA" => "added by them",
                    "DU" => "deleted by us",
                    "AA" => "both added",
                    "UU" => "both modified",
                    _ => "unmerged",
                }
                .to_owned(),
                original,
                raw_path,
                raw_original,
            });
            continue;
        }
        for (index, group) in [Group::Staged, Group::Unstaged].into_iter().enumerate() {
            let code = fields[1].as_bytes()[index];
            if code != b'.' {
                let label = match code {
                    b'M' => "modified",
                    b'A' => "added",
                    b'D' => "deleted",
                    b'R' => "renamed",
                    b'C' => "copied",
                    b'T' => "type changed",
                    _ => "changed",
                };
                snapshot.entries.push(Entry {
                    record: record.to_owned(),
                    key: Key {
                        group,
                        path: path.clone(),
                    },
                    label: label.into(),
                    original: original.clone(),
                    raw_path: raw_path.clone(),
                    raw_original: raw_original.clone(),
                });
            }
        }
    }
    Ok(snapshot)
}

fn load_snapshot(root: &std::path::Path) -> Result<Snapshot, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=all",
            RENAME_DETECTION,
            "-z",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    parse(&output.stdout)
}

pub fn working_tree_summary() -> Result<String, String> {
    Ok(load_snapshot(std::path::Path::new("."))?.summary())
}

impl Snapshot {
    fn summary(&self) -> String {
        let changed = self
            .entries
            .iter()
            .filter(|entry| entry.key.group != Group::Untracked)
            .map(|entry| &entry.key.path)
            .collect::<HashSet<_>>()
            .len();
        let untracked = self
            .entries
            .iter()
            .filter(|entry| entry.key.group == Group::Untracked)
            .count();
        let mut parts = Vec::new();
        for (count, kind) in [(changed, "changed"), (untracked, "untracked")] {
            if count > 0 {
                let suffix = if count == 1 { "" } else { "s" };
                parts.push(format!("{count} {kind} file{suffix}"));
            }
        }
        if parts.is_empty() {
            "Clean".to_owned()
        } else {
            parts.join(" · ")
        }
    }
}

// Count without building or formatting a patch. Keep memory bounded even for
// large generated text files, and use the patch renderer's binary sniff limit.
fn untracked_stats(path: &std::path::Path) -> io::Result<String> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(path)?;
        return untracked_stream_stats(target.as_os_str().as_encoded_bytes());
    }
    if !metadata.is_file() {
        return Ok("contents not loaded".into());
    }
    untracked_stream_stats(std::fs::File::open(path)?)
}

fn untracked_stream_stats(mut reader: impl Read) -> io::Result<String> {
    let mut prefix = Vec::with_capacity(8_000);
    reader.by_ref().take(8_000).read_to_end(&mut prefix)?;
    if prefix.contains(&0) {
        return Ok("binary".into());
    }
    let mut lines = prefix.iter().filter(|&&byte| byte == b'\n').count();
    let mut last = prefix.last().copied();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = match reader.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        lines += buffer[..count]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count();
        last = Some(buffer[count - 1]);
    }
    lines += usize::from(last.is_some_and(|byte| byte != b'\n'));
    Ok(format!("+{lines} −0"))
}

fn visible(text: &str) -> String {
    text.chars()
        .flat_map(|ch| {
            if ch.is_control() {
                ch.escape_default().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}
#[derive(Clone, PartialEq, Eq)]
enum RowKey {
    Group(Group),
    File(Key),
    Patch(Key, usize),
    Nested(Key, Box<RowKey>),
}
impl RowKey {
    fn outer_file(&self) -> Option<&Key> {
        match self {
            Self::File(key) | Self::Patch(key, _) | Self::Nested(key, _) => Some(key),
            Self::Group(_) => None,
        }
    }

    fn same_patch(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Patch(left, _), Self::Patch(right, _)) => left == right,
            (Self::Nested(left, a), Self::Nested(right, b)) => left == right && a.same_patch(b),
            _ => false,
        }
    }

    fn is_file(&self) -> bool {
        match self {
            Self::File(_) => true,
            Self::Nested(_, row) => row.is_file(),
            _ => false,
        }
    }
}
#[derive(Clone)]
struct Row {
    preview: Option<crate::images::Row>,
    key: RowKey,
    text: String,
    color: Option<Color>,
}
#[derive(Default)]
pub struct StatusView {
    submodules: HashMap<Key, StatusView>,
    images_enabled: bool,
    root: PathBuf,
    snapshot: Snapshot,
    collapsed: HashSet<Group>,
    patches: HashMap<Key, String>,
    fingerprints: HashMap<Key, u64>,
    #[cfg(test)]
    patch_reads: std::cell::Cell<usize>,
    #[cfg(test)]
    stat_reads: std::cell::Cell<usize>,
    rows: Vec<Row>,
    pub show_stat: bool,
    stats: HashMap<Key, String>,
    expanded_folds: HashSet<Key>,
    collapsed_files: HashSet<Key>,
    stat_bookmark: Option<Row>,
    pub cursor: usize,
    offset: usize,
    pub horizontal: u16,
    pub height: usize,
    pub origin: u16,
    pub error: Option<String>,
}
impl StatusView {
    fn load_submodule(&self, key: &Key) -> Result<Self, String> {
        let root = self.root.join(&key.path);
        if !root.join(".git").exists() {
            return Err(format!("Submodule {} is not initialized locally", key.path));
        }
        let mut child = Self {
            root,
            show_stat: self.show_stat,
            images_enabled: self.images_enabled,
            ..Self::default()
        };
        child.refresh()?;
        Ok(child)
    }
    pub fn summary(&self) -> String {
        self.snapshot.summary()
    }

    pub fn load() -> Result<Self, String> {
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        let mut view = Self {
            root: PathBuf::from(String::from_utf8_lossy(&output.stdout).trim_end_matches('\n')),
            ..Self::default()
        };
        view.refresh()?;
        Ok(view)
    }
    fn patch(&self, entry: &Entry) -> Result<String, String> {
        #[cfg(test)]
        self.patch_reads.set(self.patch_reads.get() + 1);
        if entry.key.group == Group::Untracked {
            return crate::git::show_untracked(&self.root.join(&entry.raw_path));
        }
        let mut command = self.diff_command(entry);
        command.args(["--color=always", "--full-index", "--submodule=short"]);
        self.diff_paths(&mut command, entry);
        let output = command.output().map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
    fn diff_command(&self, entry: &Entry) -> Command {
        let mut command = Command::new("git");
        command
            .arg("--literal-pathspecs")
            .arg("-C")
            .arg(&self.root)
            .args(["diff", "--no-ext-diff", "--no-textconv", RENAME_DETECTION])
            .args(crate::git::DIFF_PREFIX_ARGS);
        if entry.key.group == Group::Staged {
            command.arg("--cached");
        }
        command
    }
    fn diff_paths(&self, command: &mut Command, entry: &Entry) {
        command.arg("--").arg(&entry.raw_path);
        if let Some(original) = &entry.raw_original {
            command.arg(original);
        }
    }
    fn load_stats(&self, snapshot: &Snapshot) -> Result<HashMap<Key, String>, String> {
        let mut stats = HashMap::new();
        for entry in snapshot
            .entries
            .iter()
            .filter(|entry| entry.key.group == Group::Untracked)
        {
            let detail = untracked_stats(&self.root.join(&entry.raw_path))
                .map_err(|error| format!("could not inspect {}: {error}", entry.key.path))?;
            stats.insert(entry.key.clone(), detail);
        }
        for group in [Group::Staged, Group::Unstaged, Group::Conflicts] {
            let entries: Vec<_> = snapshot
                .entries
                .iter()
                .filter(|entry| entry.key.group == group)
                .collect();
            let Some(first) = entries.first() else {
                continue;
            };
            #[cfg(test)]
            self.stat_reads.set(self.stat_reads.get() + 1);
            let mut command = self.diff_command(first);
            // One repository-wide numstat per group avoids spawning Git for
            // every file. Read rename destinations from the NUL-delimited data.
            command.args(["--numstat", "-z", "--"]);
            let output = command.output().map_err(|error| error.to_string())?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
            }
            let mut totals = HashMap::<String, (usize, usize, bool)>::new();
            let mut records = output.stdout.split(|&byte| byte == 0);
            while let Some(record) = records.next() {
                let mut fields = record.splitn(3, |&byte| byte == b'\t');
                let (Some(a), Some(d), Some(mut path)) =
                    (fields.next(), fields.next(), fields.next())
                else {
                    continue;
                };
                if path.is_empty() {
                    records.next();
                    path = records.next().ok_or("Missing numstat rename destination")?;
                }
                let total = totals
                    .entry(String::from_utf8_lossy(path).into_owned())
                    .or_default();
                if a == b"-" || d == b"-" {
                    total.2 = true;
                } else {
                    total.0 += String::from_utf8_lossy(a)
                        .parse::<usize>()
                        .map_err(|error| error.to_string())?;
                    total.1 += String::from_utf8_lossy(d)
                        .parse::<usize>()
                        .map_err(|error| error.to_string())?;
                }
            }
            for entry in entries {
                let (added, deleted, binary) =
                    totals.get(&entry.key.path).copied().unwrap_or_default();
                stats.insert(
                    entry.key.clone(),
                    if binary {
                        "binary".into()
                    } else {
                        format!("+{added} −{deleted}")
                    },
                );
            }
        }
        Ok(stats)
    }
    pub fn toggle_stat(&mut self) {
        let current = self.rows.get(self.cursor).cloned();
        if !self.show_stat {
            self.stat_bookmark = current.clone();
            self.expanded_folds.clear();
            self.collapsed_files.clear();
        }
        let next_stat = !self.show_stat;
        let mut patches = self.patches.clone();
        if !next_stat {
            for entry in &self.snapshot.entries {
                if !Self::fold_by_default(entry) && !patches.contains_key(&entry.key) {
                    match self.patch(entry) {
                        Ok(patch) => {
                            patches.insert(entry.key.clone(), patch);
                        }
                        Err(error) => {
                            self.error = Some(error);
                            return;
                        }
                    }
                }
            }
            if let Some(Row {
                key: RowKey::File(key) | RowKey::Patch(key, _),
                ..
            }) = &current
            {
                if patches.contains_key(key) {
                    self.expanded_folds.insert(key.clone());
                }
            }
        }
        self.patches = patches;
        self.show_stat = next_stat;
        self.error = None;
        self.rebuild();
        if self.show_stat {
            // Nested rows disappear with their outer submodule. Keep the
            // cursor on that summary so toggling back can restore its bookmark.
            if let Some(key) = current.as_ref().and_then(|row| row.key.outer_file()) {
                if let Some(index) = self
                    .rows
                    .iter()
                    .position(|row| row.key == RowKey::File(key.clone()))
                {
                    self.cursor = index;
                }
            }
        } else if let Some(Row {
            key: RowKey::File(key),
            ..
        }) = current
        {
            if let Some(bookmark) = &self.stat_bookmark {
                if bookmark.key.outer_file() == Some(&key) {
                    self.cursor = self
                        .rows
                        .iter()
                        .position(|row| {
                            row.key.same_patch(&bookmark.key)
                                && row.text == bookmark.text
                                && row.preview == bookmark.preview
                        })
                        .or_else(|| self.rows.iter().position(|row| row.key == bookmark.key))
                        .unwrap_or(self.cursor);
                }
            }
        }
    }
    fn fold_by_default(entry: &Entry) -> bool {
        entry.dirty_submodule()
            || entry.key.group == Group::Untracked
            || entry.label == "added"
            || crate::diff::is_lockfile(&entry.key.path)
    }
    // Ask Git for effective attributes in one batch, covering worktree/index
    // fallback, nested .gitattributes, info/attributes and global attributes.
    fn attributes(&self, snapshot: &Snapshot) -> Result<HashMap<String, u64>, String> {
        let mut paths: Vec<_> = snapshot
            .entries
            .iter()
            .filter(|entry| entry.key.group != Group::Untracked)
            .flat_map(|entry| std::iter::once(&entry.raw_path).chain(entry.raw_original.iter()))
            .collect();
        paths.sort_unstable();
        paths.dedup();
        if paths.is_empty() {
            return Ok(HashMap::new());
        }
        let mut input = Vec::new();
        for path in paths {
            input.extend_from_slice(path.as_os_str().as_encoded_bytes());
            input.push(0);
        }
        let output = crate::git::pipe_through(
            Command::new("git").arg("-C").arg(&self.root).args([
                "check-attr",
                "--all",
                "-z",
                "--stdin",
            ]),
            &input,
        )
        .ok_or("could not read Git attributes")?;
        let mut hashes = HashMap::<String, DefaultHasher>::new();
        let mut fields = output.split_terminator('\0');
        while let Some(path) = fields.next() {
            let name = fields.next().ok_or("Missing Git attribute name")?;
            let value = fields.next().ok_or("Missing Git attribute value")?;
            let hash = hashes.entry(path.to_owned()).or_default();
            name.hash(hash);
            value.hash(hash);
        }
        Ok(hashes
            .into_iter()
            .map(|(path, hash)| (path, hash.finish()))
            .collect())
    }

    // Porcelain includes the HEAD/index object IDs; metadata catches worktree
    // edits even when the XY status stays unchanged. Directories (submodules
    // and nested repositories) need fresh diffs because child edits need not
    // change the directory's metadata.
    fn fingerprint(&self, entry: &Entry, attributes: &HashMap<String, u64>) -> Option<u64> {
        let mut hash = DefaultHasher::new();
        entry.record.hash(&mut hash);
        entry.original.hash(&mut hash);
        let paths = std::iter::once((&entry.key.path, &entry.raw_path))
            .chain(entry.original.iter().zip(entry.raw_original.iter()));
        for (name, path) in paths {
            attributes.get(name).hash(&mut hash);
            match std::fs::symlink_metadata(self.root.join(path)) {
                Ok(metadata) => {
                    if metadata.is_dir() {
                        return None;
                    }
                    metadata.len().hash(&mut hash);
                    metadata.modified().ok()?.hash(&mut hash);
                    metadata.permissions().readonly().hash(&mut hash);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        metadata.ctime().hash(&mut hash);
                        metadata.ctime_nsec().hash(&mut hash);
                        metadata.ino().hash(&mut hash);
                        metadata.mode().hash(&mut hash);
                    }
                    if metadata.file_type().is_symlink() {
                        std::fs::read_link(self.root.join(path))
                            .ok()?
                            .hash(&mut hash);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    "missing".hash(&mut hash);
                }
                Err(_) => return None,
            }
        }
        Some(hash.finish())
    }

    pub fn refresh(&mut self) -> Result<(), String> {
        let snapshot = load_snapshot(&self.root)?;
        let attributes = self.attributes(&snapshot)?;
        let mut fingerprints = HashMap::new();
        let mut patches = HashMap::new();
        let mut stats = HashMap::new();
        let mut changed = Snapshot::default();
        for entry in &snapshot.entries {
            let fingerprint = self.fingerprint(entry, &attributes);
            let unchanged =
                fingerprint.is_some() && self.fingerprints.get(&entry.key).copied() == fingerprint;
            if let Some(fingerprint) = fingerprint {
                fingerprints.insert(entry.key.clone(), fingerprint);
            }
            if self.patches.contains_key(&entry.key)
                || (!self.show_stat && !Self::fold_by_default(entry))
            {
                let patch = if let Some(cached) = self.patches.get(&entry.key).filter(|_| unchanged)
                {
                    cached.clone()
                } else {
                    self.patch(entry)?
                };
                patches.insert(entry.key.clone(), patch);
            }
            if let Some(cached) = self.stats.get(&entry.key).filter(|_| unchanged) {
                stats.insert(entry.key.clone(), cached.clone());
            } else {
                changed.entries.push(entry.clone());
            }
        }
        stats.extend(self.load_stats(&changed)?);
        self.submodules.retain(|key, _| {
            snapshot
                .entries
                .iter()
                .any(|entry| &entry.key == key && entry.dirty_submodule())
        });
        for child in self.submodules.values_mut() {
            child.refresh()?;
        }
        for entry in &snapshot.entries {
            if entry.dirty_submodule()
                && patches.contains_key(&entry.key)
                && !self.submodules.contains_key(&entry.key)
            {
                self.submodules
                    .insert(entry.key.clone(), self.load_submodule(&entry.key)?);
            }
        }
        self.stats = stats;
        self.fingerprints = fingerprints;
        self.expanded_folds.retain(|key| patches.contains_key(key));
        self.collapsed_files.retain(|key| patches.contains_key(key));
        self.snapshot = snapshot;
        self.patches = patches;
        self.error = None;
        self.rebuild();
        Ok(())
    }
    fn rebuild(&mut self) {
        let old = self.rows.get(self.cursor).cloned();
        let screen = self.cursor.saturating_sub(self.offset);
        self.rows.clear();
        for group in [
            Group::Conflicts,
            Group::Staged,
            Group::Unstaged,
            Group::Untracked,
        ] {
            let entries: Vec<_> = self
                .snapshot
                .entries
                .iter()
                .filter(|entry| entry.key.group == group)
                .collect();
            if group == Group::Conflicts && entries.is_empty() {
                continue;
            }
            self.rows.push(Row {
                key: RowKey::Group(group),
                preview: None,
                text: format!(
                    "{} {} ({})",
                    if entries.is_empty() {
                        " "
                    } else if self.collapsed.contains(&group) {
                        "▶"
                    } else {
                        "▼"
                    },
                    group.title(),
                    entries.len()
                ),
                color: Some(if entries.is_empty() {
                    Color::Gray
                } else {
                    Color::Reset
                }),
            });
            if self.collapsed.contains(&group) {
                continue;
            }
            for entry in entries {
                let expanded = self.patches.contains_key(&entry.key)
                    && ((!self.show_stat
                        && !Self::fold_by_default(entry)
                        && !self.collapsed_files.contains(&entry.key))
                        || self.expanded_folds.contains(&entry.key));
                let name = match &entry.original {
                    Some(original) => {
                        format!("{} → {}", visible(original), visible(&entry.key.path))
                    }
                    None => visible(&entry.key.path),
                };
                let detail = self
                    .stats
                    .get(&entry.key)
                    .map(|stats| format!(" | {stats}"))
                    .unwrap_or_default();
                self.rows.push(Row {
                    key: RowKey::File(entry.key.clone()),
                    text: format!(
                        "  {} {}  {}{}",
                        if expanded { "▼" } else { "▶" },
                        if entry.dirty_submodule() {
                            "dirty submodule"
                        } else {
                            &entry.label
                        },
                        name,
                        detail
                    ),
                    color: Some(Color::Reset),
                    preview: None,
                });
                if let Some(patch) = self.patches.get(&entry.key).filter(|_| expanded) {
                    for (index, line) in patch.lines().enumerate() {
                        self.rows.push(Row {
                            key: RowKey::Patch(entry.key.clone(), index),
                            text: line.to_owned(),
                            color: None,
                            preview: None,
                        });
                        if self.images_enabled
                            && crate::images::is_binary(&crate::ansi::plain(line))
                        {
                            for (label, source) in crate::images::sources(
                                patch,
                                &entry.key.path,
                                &self.root,
                                matches!(entry.key.group, Group::Unstaged | Group::Untracked),
                            ) {
                                self.rows.push(Row {
                                    key: RowKey::Patch(entry.key.clone(), index),
                                    text: format!("{label} image"),
                                    color: None,
                                    preview: None,
                                });
                                for y in 0..crate::images::HEIGHT {
                                    self.rows.push(Row {
                                        key: RowKey::Patch(entry.key.clone(), index),
                                        text: String::new(),
                                        color: None,
                                        preview: Some(crate::images::Row {
                                            source: source.clone(),
                                            row: y,
                                        }),
                                    });
                                }
                            }
                        }
                    }
                    if let Some(child) = self.submodules.get(&entry.key) {
                        for row in &child.rows {
                            self.rows.push(Row {
                                key: RowKey::Nested(entry.key.clone(), Box::new(row.key.clone())),
                                text: format!("    {}", row.text),
                                color: row.color,
                                preview: row.preview.clone(),
                            });
                        }
                    }
                }
            }
        }
        let position = old.as_ref().and_then(|old| {
            if let Some(index) = self
                .rows
                .iter()
                .enumerate()
                .filter(|(_, row)| {
                    row.key.same_patch(&old.key)
                        && row.text == old.text
                        && row.preview == old.preview
                })
                .min_by_key(|(index, _)| index.abs_diff(self.cursor))
                .map(|(index, _)| index)
            {
                return Some(index);
            }
            if let RowKey::Patch(key, _) = &old.key {
                self.rows
                    .iter()
                    .position(|row| row.key == old.key)
                    .or_else(|| {
                        self.rows
                            .iter()
                            .position(|row| row.key == RowKey::File(key.clone()))
                    })
            } else {
                self.rows.iter().position(|row| row.key == old.key)
            }
        });
        self.cursor = position.unwrap_or(self.cursor.min(self.rows.len().saturating_sub(1)));
        self.offset = self.cursor.saturating_sub(screen);
    }
    pub fn move_by(&mut self, delta: isize) {
        self.cursor = self
            .cursor
            .saturating_add_signed(delta)
            .min(self.rows.len().saturating_sub(1));
    }
    pub fn bottom(&mut self) {
        self.cursor = self.rows.len().saturating_sub(1);
    }
    pub fn toggle(&mut self) {
        let Some(row) = self.rows.get(self.cursor) else {
            return;
        };
        match row.key.clone() {
            RowKey::Nested(key, nested) => {
                let child = self.submodules.get_mut(&key).unwrap();
                if let Some(index) = child.rows.iter().position(|row| row.key == *nested) {
                    child.cursor = index;
                    child.toggle();
                    let target = child
                        .rows
                        .get(child.cursor)
                        .map(|row| RowKey::Nested(key, Box::new(row.key.clone())));
                    self.error = child.error.clone();
                    self.rebuild();
                    if let Some(index) =
                        target.and_then(|target| self.rows.iter().position(|row| row.key == target))
                    {
                        self.cursor = index;
                    }
                }
                return;
            }
            RowKey::Group(group) => {
                if !self
                    .snapshot
                    .entries
                    .iter()
                    .any(|entry| entry.key.group == group)
                {
                    return;
                }
                if !self.collapsed.remove(&group) {
                    self.collapsed.insert(group);
                }
            }
            RowKey::File(key) | RowKey::Patch(key, _) => {
                let Some(entry) = self.snapshot.entries.iter().find(|entry| entry.key == key)
                else {
                    return;
                };
                let expand = if !self.show_stat && !Self::fold_by_default(entry) {
                    self.expanded_folds.remove(&key);
                    if self.collapsed_files.remove(&key) {
                        !self.patches.contains_key(&key)
                    } else {
                        self.collapsed_files.insert(key.clone());
                        false
                    }
                } else if self.expanded_folds.remove(&key) {
                    false
                } else {
                    self.expanded_folds.insert(key.clone());
                    !self.patches.contains_key(&key)
                };
                if expand {
                    if let Some(entry) = self.snapshot.entries.iter().find(|entry| entry.key == key)
                    {
                        if entry.dirty_submodule() && !self.submodules.contains_key(&key) {
                            match self.load_submodule(&key) {
                                Ok(child) => {
                                    self.submodules.insert(key.clone(), child);
                                }
                                Err(error) => {
                                    self.expanded_folds.remove(&key);
                                    self.error = Some(error);
                                    return;
                                }
                            }
                        }
                        match self.patch(entry) {
                            Ok(patch) => {
                                self.patches.insert(key.clone(), patch);
                            }
                            Err(error) => {
                                self.expanded_folds.remove(&key);
                                self.error = Some(error);
                                return;
                            }
                        }
                    }
                }
                self.cursor = self
                    .rows
                    .iter()
                    .position(|row| row.key == RowKey::File(key.clone()))
                    .unwrap_or(self.cursor);
            }
        }
        self.error = None;
        self.rebuild();
    }
    pub fn click(&mut self, y: u16) {
        if let Some(relative) = y.checked_sub(self.origin) {
            let index = self.offset + usize::from(relative);
            if index < self.rows.len() && usize::from(relative) < self.height {
                self.cursor = index;
                self.toggle();
            }
        }
    }
    pub fn enable_images(&mut self, enabled: bool) {
        if self.images_enabled != enabled {
            self.images_enabled = enabled;
            for child in self.submodules.values_mut() {
                child.enable_images(enabled);
            }
            self.rebuild();
        }
    }

    #[cfg(test)]
    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        self.draw_with_images(frame, area, &mut crate::images::Images::default());
    }

    pub fn draw_with_images(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        images: &mut crate::images::Images,
    ) {
        let branch = if self.snapshot.branch == "(detached)" {
            "Detached HEAD"
        } else {
            &self.snapshot.branch
        };
        let mut heading = format!(
            "{branch}{}",
            if self.snapshot.initial {
                " · No commits yet"
            } else {
                ""
            }
        );
        if !self.snapshot.upstream.is_empty() {
            heading.push_str(&format!(
                " → {} {}",
                self.snapshot.upstream, self.snapshot.ahead_behind
            ));
        }
        if self.snapshot.entries.is_empty() {
            heading.push_str(" · Working tree clean");
        }
        frame.render_widget(
            Paragraph::new(visible(&heading)).style(Style::default().add_modifier(Modifier::BOLD)),
            Rect::new(area.x, area.y, area.width, area.height.min(1)),
        );
        self.origin = area.y.saturating_add(1);
        self.height = usize::from(area.height.saturating_sub(1));
        if self.cursor < self.offset {
            self.offset = self.cursor;
        }
        if self.cursor >= self.offset + self.height.max(1) {
            self.offset = self.cursor + 1 - self.height.max(1);
        }
        self.offset = self.offset.min(self.rows.len().saturating_sub(self.height));
        for (screen, row) in self
            .rows
            .iter()
            .skip(self.offset)
            .take(self.height)
            .enumerate()
        {
            let mut line = crate::ansi::normalized_line(&row.text);
            if let Some(color) = row.color {
                line = if row.key.is_file() {
                    crate::ui::summary_line(&row.text)
                } else {
                    Line::raw(row.text.clone())
                }
                .style(Style::default().fg(color));
            }
            if self.offset + screen == self.cursor {
                line.style = line.style.bg(Color::DarkGray);
            }
            let row_area = Rect::new(area.x, self.origin + screen as u16, area.width, 1);
            if self.offset + screen == self.cursor {
                frame.render_widget(
                    Block::default().style(Style::default().bg(Color::DarkGray)),
                    row_area,
                );
            }
            frame.render_widget(
                Paragraph::new(line).scroll((0, self.horizontal)),
                Rect::new(area.x, self.origin + screen as u16, area.width, 1),
            );
            if let Some(preview) = &row.preview {
                images.render(frame, preview, row_area, self.horizontal);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn dirty_submodules_expand_live_status_and_keep_nested_folds() {
        let root =
            std::env::temp_dir().join(format!("glog-dirty-submodule-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let git = |at: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(at)
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&root, &["init", "-q"]);
        let module = root.join("module space");
        git(&root, &["init", "-q", "module space"]);
        std::fs::write(module.join("file.txt"), "base\n").unwrap();
        git(&module, &["add", "."]);
        git(&module, &["commit", "-qm", "base"]);
        std::fs::write(
            root.join(".gitmodules"),
            "[submodule \"module space\"]\npath = module space\nurl = ./unused\n",
        )
        .unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "base"]);
        git(&root, &["submodule", "absorbgitdirs"]);
        std::fs::write(module.join("file.txt"), "staged\n").unwrap();
        git(&module, &["add", "."]);
        std::fs::write(module.join("file.txt"), "worktree\n").unwrap();
        std::fs::write(module.join("new.txt"), "untracked contents\n").unwrap();

        let mut view = StatusView {
            root: root.clone(),
            ..StatusView::default()
        };
        view.refresh().unwrap();
        assert!(
            view.submodules.is_empty(),
            "nested contents must load on demand"
        );
        let key = Key {
            group: Group::Unstaged,
            path: "module space".into(),
        };
        view.cursor = view
            .rows
            .iter()
            .position(|row| row.key == RowKey::File(key.clone()))
            .unwrap();
        assert!(crate::ansi::plain(&view.rows[view.cursor].text).contains("dirty submodule"));
        view.toggle();
        assert!(view.error.is_none(), "{:?}", view.error);
        assert!(view
            .rows
            .iter()
            .any(|row| crate::ansi::plain(&row.text).contains("+staged")));
        assert!(view
            .rows
            .iter()
            .any(|row| crate::ansi::plain(&row.text).contains("+worktree")));
        assert!(view
            .rows
            .iter()
            .any(|row| crate::ansi::plain(&row.text).contains("Untracked (1)")));
        view.cursor = view
            .rows
            .iter()
            .position(|row| row.key.is_file() && crate::ansi::plain(&row.text).contains("new.txt"))
            .unwrap();
        view.toggle();
        assert!(view
            .rows
            .iter()
            .any(|row| crate::ansi::plain(&row.text).contains("+untracked contents")));

        view.cursor = view
            .rows
            .iter()
            .position(|row| crate::ansi::plain(&row.text).contains("+worktree"))
            .unwrap();
        let reading_line = view.rows[view.cursor].text.clone();
        view.toggle_stat();
        view.refresh().unwrap();
        view.toggle_stat();
        assert_eq!(view.rows[view.cursor].text, reading_line);
        std::fs::write(module.join("file.txt"), "inserted\nworktree\n").unwrap();
        view.refresh().unwrap();
        assert!(crate::ansi::plain(&view.rows[view.cursor].text).contains("+worktree"));
        assert!(view
            .rows
            .iter()
            .any(|row| crate::ansi::plain(&row.text).contains("+inserted")));
        view.toggle();
        assert!(view.rows[view.cursor].key.is_file());
        assert!(crate::ansi::plain(&view.rows[view.cursor].text).contains("file.txt"));
        view.refresh().unwrap();
        assert!(!view
            .rows
            .iter()
            .any(|row| crate::ansi::plain(&row.text).contains("+worktree")));
        assert!(view
            .rows
            .iter()
            .any(|row| crate::ansi::plain(&row.text).contains("+untracked contents")));

        // Untracked-only dirtiness still has a nested view, even without a tracked patch.
        git(&module, &["reset", "--hard", "-q", "HEAD"]);
        view.refresh().unwrap();
        assert!(view
            .rows
            .iter()
            .any(|row| crate::ansi::plain(&row.text).contains("dirty submodule")));
        assert!(view
            .rows
            .iter()
            .any(|row| crate::ansi::plain(&row.text).contains("+untracked contents")));
        std::fs::remove_file(module.join("new.txt")).unwrap();
        view.refresh().unwrap();
        assert!(view.submodules.is_empty());
        assert_eq!(view.summary(), "Clean");

        // A new dirty state is expandable again after the old entry disappears.
        std::fs::write(module.join("new.txt"), "new contents\n").unwrap();
        view.refresh().unwrap();
        view.cursor = view
            .rows
            .iter()
            .position(|row| row.key == RowKey::File(key.clone()))
            .unwrap();
        view.toggle();
        assert!(view
            .rows
            .iter()
            .any(|row| row.key.is_file() && crate::ansi::plain(&row.text).contains("new.txt")));

        // Keep a pointer change present while the submodule becomes clean and dirty.
        std::fs::write(module.join("file.txt"), "committed\n").unwrap();
        git(&module, &["commit", "-qam", "next"]);
        std::fs::remove_file(module.join("new.txt")).unwrap();
        view.refresh().unwrap();
        assert!(view.submodules.is_empty());
        assert_ne!(view.summary(), "Clean");
        std::fs::write(module.join("new.txt"), "dirty again\n").unwrap();
        view.refresh().unwrap();
        assert!(view.submodules.contains_key(&key));

        // Staging the pointer must not mix the submodule's live changes into Staged.
        git(&root, &["add", "module space"]);
        let mut summary = StatusView {
            root,
            show_stat: true,
            ..StatusView::default()
        };
        summary.refresh().unwrap();
        let staged = Key {
            group: Group::Staged,
            path: key.path.clone(),
        };
        summary.cursor = summary
            .rows
            .iter()
            .position(|row| row.key == RowKey::File(staged.clone()))
            .unwrap();
        summary.toggle();
        assert!(!summary.submodules.contains_key(&staged));
        summary.cursor = summary
            .rows
            .iter()
            .position(|row| row.key == RowKey::File(key.clone()))
            .unwrap();
        summary.toggle();
        summary.cursor = summary
            .rows
            .iter()
            .position(|row| row.key.is_file() && row.text.contains("new.txt"))
            .unwrap();
        summary.toggle();
        assert!(summary
            .rows
            .iter()
            .any(|row| crate::ansi::plain(&row.text).contains("+dirty again")));
    }

    #[test]
    fn untracked_statistics_stream_text_and_stop_at_binary_prefix() {
        for (contents, expected) in [
            ("", "+0 −0"),
            ("++counter;\nnormal\n", "+2 −0"),
            ("one\ntwo", "+2 −0"),
            ("\n", "+1 −0"),
        ] {
            assert_eq!(
                untracked_stream_stats(contents.as_bytes()).unwrap(),
                expected
            );
        }
        let text = "line\n".repeat(40_000) + "tail";
        assert_eq!(
            untracked_stream_stats(text.as_bytes()).unwrap(),
            "+40001 −0"
        );
        struct BinaryPrefix;
        impl Read for BinaryPrefix {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                assert_eq!(buffer.len(), 8_000, "must not read past the binary prefix");
                buffer.fill(0);
                Ok(buffer.len())
            }
        }
        assert_eq!(untracked_stream_stats(BinaryPrefix).unwrap(), "binary");
    }

    #[test]
    fn refresh_invalidates_patches_and_stats_when_attributes_change() {
        let root =
            std::env::temp_dir().join(format!("glog-status-attributes-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::create_dir(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/file.txt"), "before\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        std::fs::write(root.join("nested/file.txt"), "staged\n").unwrap();
        git(&["add", "."]);
        std::fs::write(root.join("nested/file.txt"), "worktree\n").unwrap();
        let mut view = StatusView {
            root: root.clone(),
            ..StatusView::default()
        };
        view.refresh().unwrap();
        // Effective attributes may come from the root, a parent directory, or
        // Git's private attributes file. The changed file itself never changes.
        for attributes in [
            ".gitattributes",
            "nested/.gitattributes",
            ".git/info/attributes",
        ] {
            std::fs::write(root.join(attributes), "*.txt binary\n").unwrap();
            view.refresh().unwrap();
            for group in [Group::Staged, Group::Unstaged] {
                let key = Key {
                    group,
                    path: "nested/file.txt".into(),
                };
                assert!(
                    view.patches[&key].contains("Binary files"),
                    "{attributes}: stale patch"
                );
                assert_eq!(view.stats[&key], "binary", "{attributes}: stale statistics");
            }
            std::fs::remove_file(root.join(attributes)).unwrap();
            view.refresh().unwrap();
            for group in [Group::Staged, Group::Unstaged] {
                let key = Key {
                    group,
                    path: "nested/file.txt".into(),
                };
                assert!(!view.patches[&key].contains("Binary files"));
                assert_eq!(view.stats[&key], "+1 −1");
            }
        }
    }

    #[test]
    fn refresh_reuses_unchanged_files_and_batches_statistics() {
        let root = std::env::temp_dir().join(format!("glog-status-cache-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        for name in ["a", "b", "c"] {
            std::fs::write(root.join(name), "before\n").unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        for name in ["a", "b", "c"] {
            std::fs::write(root.join(name), "after1\n").unwrap();
        }
        std::fs::write(root.join("new"), "untracked\n").unwrap();
        let mut view = StatusView {
            root: root.clone(),
            ..StatusView::default()
        };
        view.refresh().unwrap();
        assert_eq!(view.patch_reads.get(), 3);
        assert!(view.patches.keys().all(|key| key.group != Group::Untracked));
        assert_eq!(
            view.stat_reads.get(),
            1,
            "numstat must be batched across files"
        );
        view.refresh().unwrap();
        assert_eq!(
            view.patch_reads.get(),
            3,
            "unchanged refresh must reuse patches and untracked stats"
        );
        assert_eq!(
            view.stat_reads.get(),
            1,
            "unchanged refresh must reuse numstat"
        );
        // Same-length edits leave porcelain's XY and object IDs unchanged.
        std::fs::write(root.join("a"), "after2\n").unwrap();
        view.refresh().unwrap();
        assert_eq!(
            view.patch_reads.get(),
            4,
            "only the edited file needs a new patch"
        );
        assert_eq!(view.stat_reads.get(), 2);
        let key = Key {
            group: Group::Unstaged,
            path: "a".into(),
        };
        assert!(view.patches[&key].contains("after2"));
        git(&["add", "a"]);
        view.refresh().unwrap();
        let staged = Key {
            group: Group::Staged,
            path: "a".into(),
        };
        assert!(view.patches[&staged].contains("after2"));
        assert!(!view.patches.contains_key(&key));
        std::fs::write(root.join("a"), "after3\n").unwrap();
        git(&["add", "a"]);
        view.refresh().unwrap();
        assert!(view.patches[&staged].contains("after3"));
        std::fs::write(root.join("new"), "one\ntwo\n").unwrap();
        view.refresh().unwrap();
        assert_eq!(
            view.stats[&Key {
                group: Group::Untracked,
                path: "new".into()
            }],
            "+2 −0"
        );
    }

    #[test]
    fn image_rows_follow_status_folds_and_preserve_the_scrolled_row() {
        let key = Key {
            group: Group::Staged,
            path: "image.png".into(),
        };
        let mut view = StatusView::default();
        view.snapshot.entries.push(Entry {
            record: String::new(),
            key: key.clone(),
            label: "M".into(),
            original: None,
            raw_path: "image.png".into(),
            raw_original: None,
        });
        view.patches.insert(key.clone(), "diff --git a/image.png b/image.png\nindex abcd1234..abcd5678 100644\nBinary files a/image.png and b/image.png differ\n".into());
        view.enable_images(true);
        assert_eq!(
            view.rows.iter().filter(|row| row.preview.is_some()).count(),
            24
        );
        view.cursor = view
            .rows
            .iter()
            .position(|row| row.preview.as_ref().is_some_and(|p| p.row == 7))
            .unwrap();
        view.offset = view.cursor - 2;
        let old = view.rows[view.cursor].preview.clone();
        view.rebuild();
        assert_eq!(view.rows[view.cursor].preview, old);
        assert_eq!(view.cursor - view.offset, 2);
        view.toggle();
        assert!(!view.rows.iter().any(|row| row.preview.is_some()));
        view.toggle();
        assert_eq!(
            view.rows.iter().filter(|row| row.preview.is_some()).count(),
            24
        );
        view.toggle_stat();
        assert!(!view.rows.iter().any(|row| row.preview.is_some()));
        view.toggle();
        assert_eq!(
            view.rows.iter().filter(|row| row.preview.is_some()).count(),
            24
        );
    }

    #[test]
    fn stats_hotkey_counts_changes_and_restores_reading_line_across_refresh() {
        use crate::app::{App, Mode};
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        use std::fs;

        let root = std::env::temp_dir().join(format!(
            "glog-status-stats-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        fs::write(root.join("file.txt"), "old\n").unwrap();
        fs::write(root.join("old name.txt"), "rename me\n").unwrap();
        fs::write(root.join("binary"), b"old\0").unwrap();
        fs::write(root.join("Cargo.lock"), "old\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        fs::write(root.join("file.txt"), "staged\n").unwrap();
        git(&["add", "file.txt"]);
        fs::write(root.join("file.txt"), "reading\nextra\n").unwrap();
        fs::write(root.join("binary"), b"new\0").unwrap();
        fs::write(root.join("Cargo.lock"), "new\n").unwrap();
        git(&["mv", "old name.txt", "new name.txt"]);
        fs::write(root.join("untracked"), "new\n").unwrap();
        let mut view = StatusView {
            root: root.clone(),
            ..StatusView::default()
        };
        view.refresh().unwrap();
        assert!(!view.show_stat);
        assert!(view
            .rows
            .iter()
            .any(|row| row.text.ends_with("file.txt | +2 −1")));
        assert!(!view
            .patches
            .keys()
            .any(|key| key.path == "Cargo.lock" || key.group == Group::Untracked));
        let key = Key {
            group: Group::Unstaged,
            path: "file.txt".into(),
        };
        view.cursor = view
            .rows
            .iter()
            .position(|row| row.key == RowKey::File(key.clone()))
            .unwrap();
        let mut app = App::new(Vec::new());
        app.mode = Mode::Status;
        app.status_view = Some(view);
        let enter = |app: &mut App| {
            crate::input::handle(
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                app,
            )
        };
        enter(&mut app);
        let view = app.status_view.as_mut().unwrap();
        assert!(view.rows[view.cursor].key == RowKey::File(key.clone()));
        assert!(view.rows[view.cursor].text.starts_with("  ▶"));
        assert!(!view
            .rows
            .iter()
            .any(|row| matches!(&row.key, RowKey::Patch(current, _) if current == &key)));
        view.refresh().unwrap();
        assert!(view.rows[view.cursor].text.starts_with("  ▶"));
        enter(&mut app);
        let mut view = app.status_view.take().unwrap();
        assert!(view.rows[view.cursor].text.starts_with("  ▼"));
        view.cursor = view
            .rows
            .iter()
            .position(|row| crate::ansi::plain(&row.text) == "+reading")
            .unwrap();
        let mut app = App::new(Vec::new());
        app.mode = Mode::Status;
        app.status_view = Some(view);
        let press_s = |app: &mut App| {
            crate::input::handle(
                Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)),
                app,
            )
        };
        press_s(&mut app);
        assert!(app.show_stat);
        let view = app.status_view.as_mut().unwrap();
        assert!(view.show_stat);
        assert_eq!(view.error, None);
        assert!(view.rows[view.cursor].key == RowKey::File(key.clone()));
        assert!(view.rows[view.cursor].text.ends_with(" | +2 −1"));
        assert!(view.rows.iter().any(|row| row.key
            == RowKey::File(Key {
                group: Group::Staged,
                path: "file.txt".into()
            })
            && row.text.ends_with("file.txt | +1 −1")));
        assert!(view
            .rows
            .iter()
            .any(|row| row.text.contains("old name.txt → new name.txt | +0 −0")));
        assert!(view
            .rows
            .iter()
            .any(|row| row.text.ends_with("binary | binary")));
        assert!(view
            .rows
            .iter()
            .any(|row| row.text.ends_with("untracked | +1 −0")));
        assert!(!view
            .rows
            .iter()
            .any(|row| matches!(row.key, RowKey::Patch(_, _))));
        assert!(!view.patches.keys().any(|key| key.group == Group::Untracked));
        view.toggle();
        assert!(view
            .rows
            .iter()
            .any(|row| matches!(row.key, RowKey::Patch(_, _))));
        fs::write(root.join("file.txt"), "inserted\nreading\nextra\n").unwrap();
        view.refresh().unwrap();
        assert!(view.rows[view.cursor].text.ends_with(" | +3 −1"));
        assert!(view.expanded_folds.contains(&key));
        view.toggle();
        press_s(&mut app);
        let view = app.status_view.as_mut().unwrap();
        assert!(!view.show_stat);
        assert_eq!(crate::ansi::plain(&view.rows[view.cursor].text), "+reading");
        press_s(&mut app);
        let view = app.status_view.as_mut().unwrap();
        view.cursor = view
            .rows
            .iter()
            .position(|row| row.text.contains("untracked | +1 −0"))
            .unwrap();
        view.toggle();
        assert!(view.rows[view.cursor].text.ends_with("untracked | +1 −0"));
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
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
        assert!(screen.contains("file.txt | +3 −1"));
        assert!(screen.contains("s summary"));
        for mode in [false, true] {
            press_s(&mut app);
            assert_eq!(app.show_stat, mode);
            app.status_view.as_mut().unwrap().cursor = 0;
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let mut checked = false;
            for y in 0..30 {
                let line: String = (0..120).map(|x| buffer[(x, y)].symbol()).collect();
                if let Some(index) = line
                    .chars()
                    .collect::<Vec<_>>()
                    .windows(5)
                    .position(|chars| chars == ['+', '3', ' ', '−', '1'])
                {
                    assert_eq!(buffer[(index as u16, y)].fg, Color::Green);
                    assert_eq!(buffer[(index as u16 + 3, y)].fg, Color::Red);
                    checked = true;
                }
                if line.contains("Staged (") || line.contains("Unstaged (") {
                    assert!(
                        (0..120).all(|x| !matches!(buffer[(x, y)].fg, Color::Green | Color::Red))
                    );
                }
            }
            assert!(checked, "statistics stay visible in both modes");
        }
        app.show_stat = false;
        app.open_status();
        assert!(!app.status_view.as_ref().unwrap().show_stat);
        app.show_stat = true;
        app.open_status();
        assert!(app.status_view.as_ref().unwrap().show_stat);
    }

    #[test]
    fn status_records_separate_index_worktree_untracked_and_conflicts() {
        let data = b"# branch.head feature\0# branch.upstream origin/main\0# branch.ab +2 -1\x001 MM N... 100644 100644 100644 a b both.txt\0? new file.txt\0u UU N... 100644 100644 100644 100644 a b c conflict.txt\0";
        let parsed = parse(data).unwrap();
        assert_eq!(parsed.branch, "feature");
        assert_eq!(parsed.upstream, "origin/main");
        assert_eq!(parsed.ahead_behind, "ahead 2, behind 1");
        assert_eq!(
            parsed
                .entries
                .iter()
                .map(|entry| entry.key.group)
                .collect::<Vec<_>>(),
            [
                Group::Staged,
                Group::Unstaged,
                Group::Untracked,
                Group::Conflicts
            ]
        );
        assert_eq!(parsed.entries[0].key.path, parsed.entries[1].key.path);
    }

    #[test]
    fn rename_statistics_and_patches_agree_despite_git_configuration() {
        let root = std::env::temp_dir().join(format!("glog-status-renames-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output);
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("old pure.txt"), "unchanged\n".repeat(10)).unwrap();
        let original = (0..10)
            .map(|i| format!("line {i}\n"))
            .collect::<Vec<_>>()
            .concat();
        std::fs::write(root.join("old edited.txt"), &original).unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        git(&["mv", "old pure.txt", "new pure.txt"]);
        git(&["mv", "old edited.txt", "new edited.txt"]);
        std::fs::write(
            root.join("new edited.txt"),
            original.replace("line 0", "replacement"),
        )
        .unwrap();
        git(&["add", "."]);

        for (status_renames, diff_renames) in [
            ("true", "false"),
            ("false", "true"),
            ("false", "false"),
            ("true", "true"),
            ("copies", "copies"),
        ] {
            git(&["config", "status.renames", status_renames]);
            git(&["config", "diff.renames", diff_renames]);
            let mut view = StatusView {
                root: root.clone(),
                ..StatusView::default()
            };
            view.refresh().unwrap();
            assert_eq!(
                view.snapshot.entries.len(),
                2,
                "status={status_renames}, diff={diff_renames}"
            );
            for (path, expected) in [("new pure.txt", 0), ("new edited.txt", 1)] {
                let entry = view
                    .snapshot
                    .entries
                    .iter()
                    .find(|entry| entry.key.path == path)
                    .unwrap();
                assert_eq!(entry.label, "renamed");
                assert_eq!(
                    view.stats[&entry.key],
                    format!("+{expected} −{expected}"),
                    "status={status_renames}, diff={diff_renames}, {path}"
                );
                let files = crate::diff::file_sections(&view.patches[&entry.key]);
                assert_eq!(files.len(), 1);
                assert_eq!(
                    (files[0].additions, files[0].deletions),
                    (expected, expected)
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_filenames_do_not_break_the_status_view() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let root =
            std::env::temp_dir().join(format!("glog-status-non-utf8-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let git = |args: &[&OsStr]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output);
            String::from_utf8(output.stdout).unwrap()
        };
        git(&["init".as_ref(), "-q".as_ref()]);
        std::fs::write(root.join("blob"), "contents\n").unwrap();
        let blob = git(&["hash-object".as_ref(), "-w".as_ref(), "blob".as_ref()]);
        std::fs::remove_file(root.join("blob")).unwrap();
        // Some filesystems reject such names, so record the file only in the index.
        let info = format!("100644,{},", blob.trim());
        let mut cacheinfo = info.into_bytes();
        cacheinfo.extend_from_slice(b"caf\xe9.txt");
        git(&[
            "update-index".as_ref(),
            "--add".as_ref(),
            "--cacheinfo".as_ref(),
            OsStr::from_bytes(&cacheinfo),
        ]);

        let mut view = StatusView {
            root: root.clone(),
            ..StatusView::default()
        };
        view.refresh().unwrap();
        let key = Key {
            group: Group::Unstaged,
            path: "caf\u{fffd}.txt".into(),
        };
        assert_eq!(view.stats[&key], "+0 −1");
        view.cursor = view
            .rows
            .iter()
            .position(|row| row.key == RowKey::File(key.clone()))
            .unwrap();
        view.toggle();
        assert!(crate::ansi::plain(&view.patches[&key]).contains("-contents"));
    }

    #[test]
    fn rename_records_keep_both_paths_without_splitting_whitespace() {
        let parsed = parse(
            b"2 R. N... 100644 100644 100644 a b R100 new\nname.txt\0old name.txt\0? other.txt\0",
        )
        .unwrap();
        assert_eq!(parsed.entries[0].key.path, "new\nname.txt");
        assert_eq!(parsed.entries[0].original.as_deref(), Some("old name.txt"));
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(visible(&parsed.entries[0].key.path), "new\\nname.txt");
        assert!(parse(b"2 R. N... 100644 100644 100644 a b R100 new.txt\0").is_err());
    }

    #[test]
    fn status_preserves_collapsed_groups_and_patch_line_after_insertions() {
        let snapshot = parse(b"1 .M N... 100644 100644 100644 a b file.txt\0").unwrap();
        let key = snapshot.entries[0].key.clone();
        let mut view = StatusView {
            snapshot,
            ..StatusView::default()
        };
        view.collapsed.insert(Group::Staged);
        view.patches
            .insert(key.clone(), "one\nreading\nafter".into());
        view.rebuild();
        view.cursor = view
            .rows
            .iter()
            .position(|row| row.text == "reading")
            .unwrap();
        view.offset = view.cursor - 1;
        view.patches
            .insert(key, "inserted\none\nreading\nafter".into());
        view.rebuild();
        assert_eq!(view.rows[view.cursor].text, "reading");
        assert_eq!(view.cursor - view.offset, 1);
        assert!(view.collapsed.contains(&Group::Staged));
        view.toggle();
        assert!(view.rows[view.cursor].text.starts_with("  ▶"));
        view.toggle_stat();
        assert!(matches!(view.rows[view.cursor].key, RowKey::File(_)));
        assert!(!view
            .rows
            .iter()
            .any(|row| matches!(row.key, RowKey::Patch(_, _))));
    }

    #[test]
    fn working_tree_detail_keyboard_mouse_and_conflict_sections() {
        use crate::{
            app::{App, Mode},
            input,
        };
        use crossterm::event::{
            Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
        };
        let mut view = StatusView { snapshot: parse(b"# branch.head feature\0u UU N... 100644 100644 100644 100644 a b c conflict.txt\0? new.txt\0").unwrap(), ..StatusView::default() };
        view.rebuild();
        let mut app = App::new(vec![crate::git::working_tree_commit(&view.summary())]);
        app.status_view = Some(view);
        app.mode = Mode::Status;
        let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("Log │ Status"));
        assert!(screen.contains("▼ Conflicts (1)"));
        assert!(screen.contains("both modified  conflict.txt"));
        input::handle(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &mut app,
        );
        assert!(app
            .status_view
            .as_ref()
            .unwrap()
            .collapsed
            .contains(&Group::Conflicts));
        input::handle(
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            &mut app,
        );
        assert_eq!(app.mode, Mode::Log);
        input::handle(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: app.show_tab_start + 2,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
            &mut app,
        );
        assert_eq!(app.mode, Mode::Status);
        assert!(app
            .status_view
            .as_ref()
            .unwrap()
            .collapsed
            .contains(&Group::Conflicts));
        for expected in [Mode::Log, Mode::Status, Mode::Log] {
            input::handle(
                Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
                &mut app,
            );
            assert_eq!(app.mode, expected);
        }
    }

    #[test]
    fn clean_and_unborn_status_render_without_commits() {
        for (record, expected) in [
            ("# branch.head main\0", "main · Working tree clean"),
            (
                "# branch.head main\0# branch.oid (initial)\0",
                "No commits yet",
            ),
            ("# branch.head (detached)\0", "Detached HEAD"),
        ] {
            let mut view = StatusView {
                snapshot: parse(record.as_bytes()).unwrap(),
                ..StatusView::default()
            };
            view.rebuild();
            let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
            terminal
                .draw(|frame| view.draw(frame, frame.area()))
                .unwrap();
            let heading: String = (0..80)
                .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
                .collect();
            assert!(heading.contains(expected), "{heading}");
            assert_eq!(view.rows.len(), 3);
            for index in 0..3 {
                assert!(view.rows[index].text.starts_with("  "));
                view.cursor = index;
                view.toggle();
                assert!(view.collapsed.is_empty());
            }
        }
    }
}
