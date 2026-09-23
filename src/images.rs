//! Inline Kitty graphics. Unicode placeholders participate in Ratatui's normal
//! cell diffing, so clipping, overlays and scrolling do not need cursor escapes.
use std::{
    collections::HashMap,
    fs,
    io::{self, Cursor, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    time::SystemTime,
};

use base64::{engine::general_purpose::STANDARD, Engine};
use ratatui::{layout::Rect, style::Color, widgets::Paragraph, Frame};

pub const HEIGHT: u16 = 12;
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const CACHE_SIZE: usize = 32;
// First 64 entries of Kitty's rowcolumn-diacritics.txt.
const MARKS: [u32; 64] = [
    0x305, 0x30d, 0x30e, 0x310, 0x312, 0x33d, 0x33e, 0x33f, 0x346, 0x34a, 0x34b, 0x34c, 0x350,
    0x351, 0x352, 0x357, 0x35b, 0x363, 0x364, 0x365, 0x366, 0x367, 0x368, 0x369, 0x36a, 0x36b,
    0x36c, 0x36d, 0x36e, 0x36f, 0x483, 0x484, 0x485, 0x486, 0x487, 0x592, 0x593, 0x594, 0x595,
    0x597, 0x598, 0x599, 0x59c, 0x59d, 0x59e, 0x59f, 0x5a0, 0x5a1, 0x5a8, 0x5a9, 0x5ab, 0x5ac,
    0x5af, 0x5c4, 0x610, 0x611, 0x612, 0x613, 0x614, 0x615, 0x616, 0x617, 0x657, 0x658,
];

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum Source {
    Blob(PathBuf, String),
    File(PathBuf, Option<SystemTime>, u64),
}

impl Source {
    fn file(path: PathBuf) -> Self {
        let metadata = fs::metadata(&path).ok();
        Self::File(
            path,
            metadata.as_ref().and_then(|m| m.modified().ok()),
            metadata.map_or(0, |m| m.len()),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub source: Source,
    pub row: u16,
}

/// Read object IDs from patch metadata, never from the current checkout when
/// displaying history or the index. An unstaged diff can name a new blob that
/// is not stored in Git, so its After image always comes from the working file.
pub fn sources(
    patch: &str,
    path: impl AsRef<Path>,
    root: &Path,
    worktree: bool,
) -> Vec<(String, Source)> {
    let path = path.as_ref();
    let extension = path
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "ico" | "webp"
    ) {
        return Vec::new();
    }
    let lines: Vec<_> = patch.lines().map(crate::ansi::plain).collect();
    if !lines.iter().any(|line| is_binary(line)) {
        return Vec::new();
    }
    let added = lines.iter().any(|line| line.starts_with("new file mode "));
    let deleted = lines
        .iter()
        .any(|line| line.starts_with("deleted file mode "));
    // Combined merge diffs list one old ID per parent: "index a,b..c".
    let ids = lines.iter().find_map(|line| {
        let ids = line.strip_prefix("index ")?.split_whitespace().next()?;
        let (old, new) = ids.split_once("..")?;
        let old: Vec<_> = old.split(',').collect();
        let valid =
            |id: &str| (4..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit());
        (old.iter().all(|id| valid(id)) && valid(new)).then_some((old, new))
    });
    let nonzero = |id: &str| id.bytes().any(|b| b != b'0');
    let mut result = Vec::new();
    if let Some((old, new)) = ids {
        if !added {
            let combined = old.len() > 1;
            for (parent, id) in old.into_iter().enumerate() {
                if nonzero(id) {
                    let label = if combined {
                        format!("Parent {}", parent + 1)
                    } else {
                        "Before".to_owned()
                    };
                    result.push((label, Source::Blob(root.to_owned(), id.to_owned())));
                }
            }
        }
        if !deleted {
            if worktree {
                result.push(("After".to_owned(), Source::file(root.join(path))));
            } else if nonzero(new) {
                result.push((
                    "After".to_owned(),
                    Source::Blob(root.to_owned(), new.to_owned()),
                ));
            }
        }
    } else if added && worktree {
        result.push(("After".to_owned(), Source::file(root.join(path))));
    }
    result
}

pub fn is_binary(line: &str) -> bool {
    line.starts_with("Binary files ") && line.ends_with(" differ")
}

struct Decoded {
    width: u32,
    height: u32,
    data: String,
}

struct Entry {
    image: Option<Decoded>,
    pending: bool,
    id: u32,
    columns: u16,
    used: u64,
}

type Response = (Source, Option<Decoded>);

#[derive(Default)]
pub struct Images {
    pub enabled: bool,
    pub root: PathBuf,
    tmux: bool,
    sender: Option<SyncSender<Source>>,
    receiver: Option<Receiver<Response>>,
    entries: HashMap<Source, Entry>,
    next_id: u32,
    clock: u64,
    output: Vec<u8>,
}

impl Images {
    pub fn from_env() -> Self {
        let setting = std::env::var("GLOG_IMAGES").unwrap_or_default();
        let tmux = std::env::var_os("TMUX").is_some();
        let supported = std::env::var("TERM").is_ok_and(|s| s == "xterm-kitty")
            || std::env::var("TERM_PROGRAM").is_ok_and(|s| s == "ghostty");
        let enabled = setting == "kitty" || (setting.is_empty() && supported && !tmux);
        if !enabled {
            return Self::default();
        }
        let root = crate::git::repository_root().unwrap_or_default();
        Self {
            enabled,
            root,
            tmux,
            // Instances sharing a terminal, such as tmux panes, share its image
            // IDs. Sequential IDs from a random start rarely overlap, whereas
            // PID-based ranges collide between neighbouring PIDs.
            next_id: random_u32() % 0xffffff + 1,
            ..Self::default()
        }
    }

    fn start_worker(&mut self) {
        if self.sender.is_some() {
            return;
        }
        let (sender, requests) = mpsc::sync_channel::<Source>(4);
        let (responses, receiver) = mpsc::sync_channel(4);
        std::thread::spawn(move || {
            while let Ok(source) = requests.recv() {
                let decoded = decode(&source);
                if responses.send((source, decoded)).is_err() {
                    break;
                }
            }
        });
        self.sender = Some(sender);
        self.receiver = Some(receiver);
    }

    pub fn poll(&mut self) {
        if let Some(receiver) = &self.receiver {
            while let Ok((source, image)) = receiver.try_recv() {
                if let Some(entry) = self.entries.get_mut(&source) {
                    entry.image = image;
                    entry.pending = false;
                }
            }
        }
    }

    fn command(&mut self, command: &str) {
        if self.tmux {
            self.output.extend_from_slice(b"\x1bPtmux;");
            self.output
                .extend_from_slice(command.replace('\x1b', "\x1b\x1b").as_bytes());
            self.output.extend_from_slice(b"\x1b\\");
        } else {
            self.output.extend_from_slice(command.as_bytes());
        }
    }

    pub fn render(&mut self, frame: &mut Frame, row: &Row, area: Rect, horizontal: u16) {
        if !self.enabled || area.is_empty() {
            return;
        }
        self.start_worker();
        self.clock += 1;
        if !self.entries.contains_key(&row.source) {
            if self
                .sender
                .as_ref()
                .unwrap()
                .try_send(row.source.clone())
                .is_err()
            {
                if row.row == 0 {
                    frame.render_widget(Paragraph::new("Loading image…"), area);
                }
                return;
            }
            if self.entries.len() >= CACHE_SIZE {
                if let Some(source) = self
                    .entries
                    .iter()
                    .min_by_key(|(_, e)| e.used)
                    .map(|(s, _)| s.clone())
                {
                    let entry = self.entries.remove(&source).unwrap();
                    if entry.columns != 0 {
                        self.command(&format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", entry.id));
                    }
                }
            }
            self.next_id = self.next_id % 0xffffff + 1;
            self.entries.insert(
                row.source.clone(),
                Entry {
                    image: None,
                    pending: true,
                    id: self.next_id,
                    columns: 0,
                    used: self.clock,
                },
            );
        }
        let entry = self.entries.get_mut(&row.source).unwrap();
        entry.used = self.clock;
        let Some(image) = &entry.image else {
            if row.row == 0 {
                frame.render_widget(
                    Paragraph::new(if entry.pending {
                        "Loading image…"
                    } else {
                        "Image preview unavailable"
                    }),
                    area,
                );
            }
            return;
        };
        let columns = area.width.min(MARKS.len() as u16);
        let id = entry.id;
        let mut commands = Vec::new();
        if entry.columns == 0 {
            let chunks: Vec<_> = image.data.as_bytes().chunks(4096).collect();
            for (index, chunk) in chunks.iter().enumerate() {
                let more = usize::from(index + 1 < chunks.len());
                let payload = std::str::from_utf8(chunk).unwrap();
                if index == 0 {
                    commands.push(format!(
                        "\x1b_Ga=t,f=32,s={},v={},i={id},q=2,m={more};{payload}\x1b\\",
                        image.width, image.height
                    ));
                } else {
                    commands.push(format!("\x1b_Gm={more},q=2;{payload}\x1b\\"));
                }
            }
        }
        if entry.columns != columns {
            commands.push(format!(
                "\x1b_Ga=p,U=1,i={id},p=1,c={columns},r={HEIGHT},q=2\x1b\\"
            ));
            entry.columns = columns;
        }
        for command in commands {
            self.command(&command);
        }
        for x in horizontal.min(columns)..columns {
            let symbol = format!(
                "\u{10eeee}{}{}",
                char::from_u32(MARKS[row.row as usize]).unwrap(),
                char::from_u32(MARKS[x as usize]).unwrap()
            );
            frame.buffer_mut()[(area.x + x - horizontal, area.y)]
                .set_symbol(&symbol)
                .set_fg(Color::Rgb((id >> 16) as u8, (id >> 8) as u8, id as u8));
        }
    }

    pub fn flush(&mut self, writer: &mut impl Write) -> io::Result<()> {
        writer.write_all(&self.output)?;
        self.output.clear();
        writer.flush()
    }

    pub fn clear(&mut self, writer: &mut impl Write) -> io::Result<()> {
        let ids: Vec<_> = self
            .entries
            .values()
            .filter(|e| e.columns != 0)
            .map(|e| e.id)
            .collect();
        for id in ids {
            self.command(&format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\"));
        }
        self.entries.clear();
        self.flush(writer)
    }
}

fn random_u32() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish() as u32
}

fn read_source(source: &Source) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    match source {
        Source::File(path, _, _) => {
            let metadata = fs::symlink_metadata(path).ok()?;
            if !metadata.is_file() || metadata.len() > MAX_BYTES {
                return None;
            }
            fs::File::open(path)
                .ok()?
                .take(MAX_BYTES + 1)
                .read_to_end(&mut bytes)
                .ok()?;
        }
        Source::Blob(root, oid) => {
            let mut child = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["cat-file", "blob", oid])
                // Previews show local objects only, never fetching from remotes.
                .env("GIT_NO_LAZY_FETCH", "1")
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            let result = child
                .stdout
                .take()?
                .take(MAX_BYTES + 1)
                .read_to_end(&mut bytes);
            if result.is_err() || bytes.len() as u64 > MAX_BYTES {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            if !child.wait().ok()?.success() {
                return None;
            }
        }
    }
    (bytes.len() as u64 <= MAX_BYTES).then_some(bytes)
}

fn decode(source: &Source) -> Option<Decoded> {
    let bytes = read_source(source)?;
    let mut reader = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .ok()?;
    let dimensions = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    if u64::from(dimensions.0) * u64::from(dimensions.1) > 16_000_000 {
        return None;
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .ok()?
        .thumbnail(dimensions.0.min(1024), dimensions.1.min(512))
        .to_rgba8();
    Some(Decoded {
        width: image.width(),
        height: image.height(),
        data: STANDARD.encode(image.as_raw()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{App, Mode},
        ui,
    };
    use ratatui::{backend::TestBackend, Terminal};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Repo(PathBuf);
    impl Repo {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "glog-images-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            let repo = Self(path);
            repo.git(&["init", "-q"]);
            repo.git(&["config", "user.name", "Test"]);
            repo.git(&["config", "commit.gpgsign", "false"]);
            repo.git(&["config", "user.email", "test@example.com"]);
            repo
        }
        fn git(&self, args: &[&str]) -> String {
            let output = Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        }
        fn image(&self, path: &str, value: u8) -> Vec<u8> {
            image::RgbaImage::from_pixel(4, 3, image::Rgba([value, 10, 20, 255]))
                .save(self.0.join(path))
                .unwrap();
            fs::read(self.0.join(path)).unwrap()
        }
    }
    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_partial_clone_blobs_are_not_fetched() {
        let origin = Repo::new();
        origin.image("picture.png", 1);
        origin.git(&["add", "."]);
        origin.git(&["commit", "-qm", "picture"]);
        origin.git(&["config", "uploadpack.allowFilter", "true"]);
        let oid = origin.git(&["rev-parse", "HEAD:picture.png"]);
        let oid = oid.trim();
        let clone = Repo(origin.0.with_extension("clone"));
        let output = Command::new("git")
            .args([
                "clone",
                "-q",
                "--no-local",
                "--no-checkout",
                "--filter=blob:none",
            ])
            .arg(&origin.0)
            .arg(&clone.0)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let present = || {
            Command::new("git")
                .arg("-C")
                .arg(&clone.0)
                .args(["cat-file", "-e", oid])
                .env("GIT_NO_LAZY_FETCH", "1")
                .status()
                .unwrap()
                .success()
        };
        assert!(!present());
        assert!(read_source(&Source::Blob(clone.0.clone(), oid.to_owned())).is_none());
        assert!(!present(), "previews must not fetch missing blobs");
    }

    #[test]
    fn merge_commits_preview_each_parent_and_the_result() {
        let repo = Repo::new();
        let path = "picture.png";
        repo.image(path, 1);
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "base"]);
        repo.git(&["checkout", "-qb", "side"]);
        let side = repo.image(path, 2);
        repo.git(&["commit", "-qam", "side"]);
        repo.git(&["checkout", "-q", "-"]);
        let main = repo.image(path, 3);
        repo.git(&["commit", "-qam", "main"]);
        let output = Command::new("git")
            .arg("-C")
            .arg(&repo.0)
            .args(["merge", "-q", "side"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "the images must conflict");
        let merged = repo.image(path, 4);
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "merge"]);
        let patch = repo.git(&["show", "--full-index", "--format=", "HEAD"]);
        assert!(patch.starts_with("diff --cc"), "{patch}");
        let previews = sources(&patch, path, &repo.0, false);
        let labels: Vec<_> = previews
            .iter()
            .map(|(label, _)| label.to_string())
            .collect();
        assert_eq!(labels, ["Parent 1", "Parent 2", "After"]);
        for ((_, source), expected) in previews.iter().zip([main, side, merged]) {
            assert_eq!(read_source(source).unwrap(), expected);
        }
    }
    #[test]
    fn reads_history_index_and_worktree_images_from_the_correct_sources() {
        let repo = Repo::new();
        let path = "picture space é.png";
        let before = repo.image(path, 1);
        repo.git(&["add", "--", path]);
        repo.git(&["commit", "-qm", "before"]);
        let staged = repo.image(path, 2);
        repo.git(&["add", "--", path]);
        let working = repo.image(path, 3);
        let patch = repo.git(&["diff", "--cached", "--full-index"]);
        let section = crate::diff::file_sections(&patch);
        assert_eq!(section[0].path, path);
        let pair = sources(&patch, path, &repo.0, false);
        assert_eq!(pair.len(), 2);
        assert_eq!(read_source(&pair[0].1).unwrap(), before);
        assert_eq!(read_source(&pair[1].1).unwrap(), staged);
        let unstaged = repo.git(&["diff", "--full-index"]);
        let pair = sources(&unstaged, path, &repo.0, true);
        assert_eq!(read_source(&pair[0].1).unwrap(), staged);
        assert_eq!(read_source(&pair[1].1).unwrap(), working);
        // Color-only delta must retain the metadata on which previews depend.
        if let Ok(mut child) = Command::new("delta")
            .args(["--paging=never", "--color-only"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
        {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(patch.as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            let colored = String::from_utf8(output.stdout).unwrap();
            assert_eq!(
                sources(&colored, path, &repo.0, false),
                sources(&patch, path, &repo.0, false)
            );
        }
        repo.git(&["commit", "-qm", "staged"]);
        let historical = repo.git(&["show", "--full-index", "HEAD"]);
        let pair = sources(&historical, path, &repo.0, false);
        assert_eq!(read_source(&pair[1].1).unwrap(), staged);
        fs::remove_file(repo.0.join(path)).unwrap();
        let deleted = sources(&repo.git(&["diff", "--full-index"]), path, &repo.0, true);
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].0, "Before");
        assert_eq!(read_source(&deleted[0].1).unwrap(), staged);
    }

    #[test]
    fn added_untracked_and_non_image_files() {
        let repo = Repo::new();
        let data = repo.image("new.png", 4);
        let patch = "diff --git a/new.png b/new.png\nnew file mode 100644\nBinary files /dev/null and b/new.png differ\n";
        let previews = sources(patch, "new.png", &repo.0, true);
        assert_eq!(previews.len(), 1);
        assert_eq!(previews[0].0, "After");
        assert_eq!(read_source(&previews[0].1).unwrap(), data);
        assert!(sources(patch, "data.bin", &repo.0, true).is_empty());
        assert!(sources(patch, "new.png", &repo.0, false).is_empty());
        repo.git(&["add", "new.png"]);
        let staged = sources(
            &repo.git(&["diff", "--cached", "--full-index"]),
            "new.png",
            &repo.0,
            false,
        );
        assert_eq!(staged.len(), 1);
        assert_eq!(read_source(&staged[0].1).unwrap(), data);
        let decoded = decode(&staged[0].1).unwrap();
        assert_eq!((decoded.width, decoded.height), (4, 3));
        fs::write(repo.0.join("broken.png"), b"not a PNG").unwrap();
        assert!(decode(&Source::file(repo.0.join("broken.png"))).is_none());
        let huge = fs::File::create(repo.0.join("huge.png")).unwrap();
        huge.set_len(MAX_BYTES + 1).unwrap();
        assert!(read_source(&Source::file(repo.0.join("huge.png"))).is_none());
    }

    fn preview_app() -> App {
        let mut app = App::new(Vec::new());
        app.mode = Mode::Show;
        app.images.enabled = true;
        app.show_text = "diff --git a/p.png b/p.png\nindex abcd1234..abcd5678 100644\nBinary files a/p.png and b/p.png differ\nafter the image\n".to_owned();
        app.ensure_show_rows();
        for (_, source) in sources(&app.show_text, "p.png", Path::new(""), false) {
            let id = 100 + app.images.entries.len() as u32;
            app.images.entries.insert(
                source,
                Entry {
                    image: Some(Decoded {
                        width: 1,
                        height: 1,
                        data: STANDARD.encode([255, 0, 0, 255]),
                    }),
                    pending: false,
                    id,
                    columns: 0,
                    used: 0,
                },
            );
        }
        app
    }

    #[test]
    fn images_clip_scroll_resize_fold_and_leave_help_clear() {
        let mut app = preview_app();
        let mut terminal = Terminal::new(TestBackend::new(30, 9)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let mut output = Vec::new();
        app.images.flush(&mut output).unwrap();
        assert!(String::from_utf8(output).unwrap().contains("a=t,f=32"));
        app.scroll_show(8);
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let index = app
            .show_row_starts
            .partition_point(|&start| start <= app.show_offset)
            - 1;
        let top = app.show_rows[index].preview.as_ref().unwrap();
        let expected = format!(
            "\u{10eeee}{}{}",
            char::from_u32(MARKS[top.row as usize]).unwrap(),
            char::from_u32(MARKS[0]).unwrap()
        );
        assert_eq!(terminal.backend().buffer()[(0, 1)].symbol(), expected);
        assert!(!terminal.backend().buffer()[(0, 0)]
            .symbol()
            .starts_with('\u{10eeee}'));
        let mut output = Vec::new();
        app.images.flush(&mut output).unwrap();
        assert!(!String::from_utf8(output).unwrap().contains("a=t,f=32"));
        terminal.backend_mut().resize(20, 9);
        terminal.autoresize().unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let mut output = Vec::new();
        app.images.flush(&mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("c=20,r=12"));
        assert!(!output.contains("a=t,f=32"));
        app.show_help = true;
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert!(!terminal.backend().buffer()[(3, 3)]
            .symbol()
            .starts_with('\u{10eeee}'));
        app.show_help = false;
        app.show_cursor = app
            .show_rows
            .iter()
            .position(|row| row.preview.as_ref().is_some_and(|p| p.row == 7))
            .unwrap();
        let bookmark = app.show_rows[app.show_cursor].preview.clone();
        app.toggle_show_stat();
        app.toggle_show_stat();
        assert_eq!(app.show_rows[app.show_cursor].preview, bookmark);
        app.toggle_show_stat();
        assert!(!app.show_rows.iter().any(|row| row.preview.is_some()));
        app.toggle_show_file();
        assert_eq!(
            app.show_rows
                .iter()
                .filter(|row| row.preview.is_some())
                .count(),
            2 * usize::from(HEIGHT)
        );
        let mut output = Vec::new();
        app.images.clear(&mut output).unwrap();
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("a=d,d=I,i=100,q=2"));
    }

    #[test]
    fn unsupported_terminals_keep_original_diff_rows() {
        let mut app = App::new(Vec::new());
        app.show_text = preview_app().show_text;
        app.ensure_show_rows();
        assert_eq!(app.show_rows.len(), app.show_text.lines().count());
        assert!(app.show_rows.iter().all(|row| row.preview.is_none()));
    }
}
