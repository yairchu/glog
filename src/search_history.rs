use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

const LIMIT: usize = 100;

#[derive(Default)]
pub struct SearchHistory {
    pub queries: Vec<String>,
    path: Option<PathBuf>,
}

impl SearchHistory {
    pub fn load() -> Self {
        Self::from_path(history_path())
    }

    fn from_path(path: Option<PathBuf>) -> Self {
        let queries = path
            .as_ref()
            .and_then(|path| {
                let mut file = File::open(path).ok()?;
                file.lock_shared().ok()?;
                read(&mut file).ok()
            })
            .unwrap_or_default();
        Self { queries, path }
    }

    pub fn record(&mut self, query: &str) {
        if query.is_empty() || self.queries.last().is_some_and(|last| last == query) {
            return;
        }
        push(&mut self.queries, query);
        // Missing or unwritable state storage must not interrupt searching.
        let _ = self.save(query);
    }

    fn save(&self, query: &str) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        // Merge under the same lock used to write, preserving other sessions' searches.
        file.lock()?;
        let mut queries = read(&mut file)?;
        push(&mut queries, query);
        let text = queries.join("\n") + "\n";
        file.seek(SeekFrom::Start(0))?;
        file.set_len(0)?;
        file.write_all(text.as_bytes())
    }
}

fn push(queries: &mut Vec<String>, query: &str) {
    if queries.last().is_none_or(|last| last != query) {
        queries.push(query.to_owned());
    }
    if queries.len() > LIMIT {
        queries.drain(..queries.len() - LIMIT);
    }
}

/// Read the saved queries, tolerating invalid UTF-8 so that one bad byte
/// cannot stop all later searches from being saved.
fn read(file: &mut File) -> io::Result<Vec<String>> {
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let mut queries = Vec::new();
    for query in String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|query| !query.is_empty())
    {
        push(&mut queries, query);
    }
    Ok(queries)
}

fn history_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("GLOG_HISTFILE") {
        return (!path.is_empty() && path != "-" && path != "/dev/null")
            .then(|| PathBuf::from(path));
    }
    let state = env::var_os("XDG_STATE_HOME").filter(|path| !path.is_empty());
    #[cfg(windows)]
    let state = state.or_else(|| env::var_os("LOCALAPPDATA"));
    #[cfg(not(windows))]
    let state = state.or_else(|| {
        env::var_os("HOME")
            .filter(|path| !path.is_empty())
            .map(|home| PathBuf::from(home).join(".local/state").into_os_string())
    });
    state.map(|state| PathBuf::from(state).join("glog/search_history"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::TestDirectory;

    #[test]
    fn searches_survive_new_sessions_without_overwriting_other_sessions() {
        let directory = TestDirectory::new();
        let path = directory.path().join("state/search_history");
        let mut first = SearchHistory::from_path(Some(path.clone()));
        let mut second = SearchHistory::from_path(Some(path.clone()));
        first.record("older");
        second.record("日本語");
        first.record("newer");
        first.record("newer");
        first.record("");
        assert_eq!(
            SearchHistory::from_path(Some(path)).queries,
            ["older", "日本語", "newer"]
        );
    }

    #[test]
    fn persisted_history_stays_bounded() {
        let directory = TestDirectory::new();
        let path = directory.path().join("search_history");
        let mut history = SearchHistory::from_path(Some(path.clone()));
        for query in 0..LIMIT + 5 {
            history.record(&query.to_string());
        }
        let reloaded = SearchHistory::from_path(Some(path));
        assert_eq!(reloaded.queries, history.queries);
        assert_eq!(reloaded.queries.len(), LIMIT);
        assert_eq!(reloaded.queries.first().unwrap(), "5");
    }

    #[test]
    fn concurrent_writers_preserve_each_others_queries() {
        let directory = TestDirectory::new();
        let path = directory.path().join("search_history");
        std::thread::scope(|scope| {
            for session in 0..4 {
                let path = &path;
                scope.spawn(move || {
                    let mut history = SearchHistory::from_path(Some(path.clone()));
                    for query in 0..10 {
                        history.record(&format!("{session}-{query}"));
                    }
                });
            }
        });
        let history = SearchHistory::from_path(Some(path));
        assert_eq!(history.queries.len(), 40);
        for session in 0..4 {
            for query in 0..10 {
                assert!(history.queries.contains(&format!("{session}-{query}")));
            }
        }
    }

    #[test]
    fn invalid_utf8_keeps_history_loading_and_saving() {
        let directory = TestDirectory::new();
        let path = directory.path().join("search_history");
        fs::write(&path, b"older\n\xff\n").unwrap();
        let mut history = SearchHistory::from_path(Some(path.clone()));
        assert_eq!(history.queries, ["older", "\u{fffd}"]);
        history.record("newer");
        assert_eq!(
            SearchHistory::from_path(Some(path)).queries,
            ["older", "\u{fffd}", "newer"]
        );
    }

    #[test]
    fn unavailable_storage_keeps_in_memory_history_working() {
        let directory = TestDirectory::new();
        let mut history = SearchHistory::from_path(Some(directory.path().to_owned()));
        history.record("query");
        assert_eq!(history.queries, ["query"]);
    }
}
