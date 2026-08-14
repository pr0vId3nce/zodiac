//! Context + touched files for **terminal** panes.
//!
//! A structured pane reports its usage and tool calls over the stream zodiac
//! already folds. A pty pane running the Claude Code TUI reports nothing —
//! it's just a terminal — but claude writes the same facts to its session
//! transcript (`~/.claude/projects/<slug>/<session>.jsonl`). This module reads
//! that file so the rail can answer the same two questions for both kinds of
//! pane.
//!
//! Two things it is careful about:
//!
//! * **Never on the UI thread.** A long session's transcript runs to several
//!   MB; parsing one inside a frame would drop it. A worker thread does the
//!   work and the UI reads a small snapshot.
//! * **Append-only reads.** Each poll parses only the bytes added since the
//!   last one, so the cost after the first pass is a few lines.
//!
//! The pane→session mapping is a heuristic — claude records no pid and puts no
//! session id in its environment, so all we have is the directory and the
//! clock. See [`find_session`].

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime};

use zodiac::client_core::{FileEdit, TranscriptFold, Usage};

/// What the UI knows about a pty pane that might be running claude.
#[derive(Clone, Debug)]
pub struct PaneReq {
    pub id: u64,
    pub cwd: String,
    /// Milliseconds the pane has been alive, from `PaneState::uptime_ms`.
    pub uptime_ms: u64,
}

/// What the worker found for a pane.
#[derive(Clone, Default, Debug)]
pub struct PaneAgent {
    pub usage: Usage,
    pub files: Vec<FileEdit>,
    /// The transcript this came from, for the panel's hover text.
    pub session: Option<String>,
    /// More than one session started in this directory while the pane was
    /// alive, so which one belongs to it can't be known. Nothing is shown —
    /// see [`find_session`].
    pub ambiguous: usize,
}

/// Which transcript, if any, belongs to a pane.
#[derive(Debug, PartialEq)]
pub enum Pick {
    One(PathBuf),
    /// N sessions began here while the pane was alive: unattributable.
    Ambiguous(usize),
    None,
}

/// Handle held by the app: push the current pane list, read the latest results.
pub struct Watcher {
    tx: Sender<Vec<PaneReq>>,
    out: Arc<Mutex<HashMap<u64, PaneAgent>>>,
}

impl Watcher {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel();
        let out: Arc<Mutex<HashMap<u64, PaneAgent>>> = Arc::default();
        let worker_out = out.clone();
        std::thread::Builder::new()
            .name("termagent".into())
            .spawn(move || worker(rx, worker_out))
            .ok();
        Self { tx, out }
    }

    /// Tell the worker which panes to watch (cheap; sent at most once a poll).
    pub fn update(&self, panes: Vec<PaneReq>) {
        let _ = self.tx.send(panes);
    }

    /// The latest reading for a pane, if the worker has one.
    pub fn get(&self, id: u64) -> Option<PaneAgent> {
        self.out.lock().ok()?.get(&id).cloned()
    }
}

/// One pane's incremental reader.
struct Tail {
    path: PathBuf,
    offset: u64,
    fold: TranscriptFold,
}

fn worker(rx: Receiver<Vec<PaneReq>>, out: Arc<Mutex<HashMap<u64, PaneAgent>>>) {
    let mut tails: HashMap<u64, Tail> = HashMap::new();
    let mut panes: Vec<PaneReq> = Vec::new();
    loop {
        // Take the newest pane list, discarding any backlog.
        match rx.recv_timeout(Duration::from_millis(1500)) {
            Ok(list) => {
                panes = list;
                while let Ok(newer) = rx.try_recv() {
                    panes = newer;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        tails.retain(|id, _| panes.iter().any(|p| p.id == *id));
        for p in &panes {
            let started = SystemTime::now()
                .checked_sub(Duration::from_millis(p.uptime_ms))
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let tail = match tails.get_mut(&p.id) {
                Some(t) => t,
                None => match find_session(&p.cwd, started) {
                    Pick::One(path) => tails.entry(p.id).or_insert(Tail {
                        path,
                        offset: 0,
                        fold: TranscriptFold::default(),
                    }),
                    // Say why there is nothing rather than showing nothing.
                    Pick::Ambiguous(n) => {
                        if let Ok(mut m) = out.lock() {
                            m.insert(
                                p.id,
                                PaneAgent {
                                    ambiguous: n,
                                    ..Default::default()
                                },
                            );
                        }
                        continue;
                    }
                    Pick::None => continue,
                },
            };
            if read_new(tail) {
                let snap = PaneAgent {
                    usage: tail.fold.usage,
                    files: tail.fold.files.clone(),
                    session: tail
                        .path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned()),
                    ambiguous: 0,
                };
                if let Ok(mut m) = out.lock() {
                    m.insert(p.id, snap);
                }
            }
        }
    }
}

/// Parse whatever has been appended since the last poll. Returns whether
/// anything was read.
fn read_new(tail: &mut Tail) -> bool {
    let Ok(file) = std::fs::File::open(&tail.path) else {
        return false;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len < tail.offset {
        // Truncated or replaced — start over rather than read garbage.
        tail.offset = 0;
        tail.fold = TranscriptFold::default();
    }
    if len == tail.offset {
        return false;
    }
    let mut r = BufReader::new(file);
    if r.seek(SeekFrom::Start(tail.offset)).is_err() {
        return false;
    }
    let mut read = 0u64;
    let mut line = String::new();
    loop {
        line.clear();
        match r.read_line(&mut line) {
            Ok(0) => break,
            Ok(n) => {
                // A partial last line means the writer is mid-append: stop
                // before it and pick it up whole on the next poll.
                if !line.ends_with('\n') {
                    break;
                }
                read += n as u64;
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                    tail.fold.apply(&v);
                }
            }
            Err(_) => break,
        }
    }
    tail.offset += read;
    read > 0
}

/// Claude Code's directory name for a working directory: the path with every
/// non-alphanumeric character replaced by `-`, so `/home/d3s/claude/zodiac`
/// becomes `-home-d3s-claude-zodiac`.
pub fn project_slug(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Pick the transcript a pane is most likely writing.
///
/// Claude records no pid and exports no session id, so the only signals are
/// the directory and the clock: among this directory's transcripts, prefer
/// ones **born after the pane started** (a session this pane began), and take
/// the most recently written. A pane that has not written yet gets `None`,
/// which the panels render as "no turn reported yet" rather than as another
/// session's numbers.
///
/// The residual ambiguity is real: two claude TUIs started in the *same*
/// directory can't be told apart this way, and the newer writer wins. The
/// panel names the session it read so a wrong guess is visible rather than
/// silent.
pub fn find_session(cwd: &str, started: SystemTime) -> Pick {
    let Some(root) = projects_root() else {
        return Pick::None;
    };
    let Ok(entries) = std::fs::read_dir(root.join(project_slug(cwd))) else {
        return Pick::None;
    };
    // Only a session *born* while this pane was alive can be this pane's. An
    // earlier one that merely got written to belongs to whoever else is in the
    // directory, and attributing it produced exactly that: a busy home
    // directory's 42 MB transcript — another project's files, a hundred
    // million cached tokens — shown against an unrelated pane.
    let mut born: Vec<PathBuf> = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        // No creation time (some filesystems don't record one): ownership
        // can't be established, so don't claim it.
        let Ok(created) = md.created() else { continue };
        if created >= started && md.modified().map(|m| m >= started).unwrap_or(false) {
            born.push(path);
        }
    }
    match born.len() {
        0 => Pick::None,
        1 => Pick::One(born.remove(0)),
        // Two sessions started in one directory while this pane ran. Nothing
        // distinguishes them from the outside, and a coin flip presented as
        // fact is worse than saying so.
        n => Pick::Ambiguous(n),
    }
}

/// Where claude keeps its per-directory transcripts. `ZODIAC_PROJECTS_DIR`
/// overrides it so the e2e can exercise this path against a scratch tree
/// instead of writing into the user's real `~/.claude/projects`.
pub fn projects_root() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ZODIAC_PROJECTS_DIR") {
        return Some(PathBuf::from(p));
    }
    Some(dirs_home()?.join(".claude/projects"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).filter(|p| {
        let p: &Path = p.as_ref();
        p.is_absolute()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_matches_claudes_directory_naming() {
        assert_eq!(
            project_slug("/home/d3s/claude/zodiac"),
            "-home-d3s-claude-zodiac"
        );
        // Dots and underscores are separators too, not kept verbatim.
        assert_eq!(project_slug("/tmp/a.b_c"), "-tmp-a-b-c");
    }

    /// A directory holding an older session plus one started after the pane:
    /// only the latter can be this pane's.
    #[test]
    fn only_a_session_born_after_the_pane_is_claimed() {
        let root = std::env::temp_dir().join(format!("zodiac-pick-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cwd = "/some/where";
        let dir = root.join(project_slug(cwd));
        std::fs::create_dir_all(&dir).unwrap();
        // An old, *busy* session: the kind that used to win on mtime and put
        // another project's files and totals against an unrelated pane.
        std::fs::write(dir.join("old.jsonl"), "{}\n").unwrap();
        std::thread::sleep(Duration::from_millis(30));
        let started = SystemTime::now();
        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(dir.join("mine.jsonl"), "{}\n").unwrap();
        // The old one is touched again *after* the pane started — recency
        // alone must not make it the pane's.
        std::fs::write(dir.join("old.jsonl"), "{}\n{}\n").unwrap();
        std::env::set_var("ZODIAC_PROJECTS_DIR", &root);
        let pick = find_session(cwd, started);
        std::env::remove_var("ZODIAC_PROJECTS_DIR");
        assert_eq!(pick, Pick::One(dir.join("mine.jsonl")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn two_sessions_born_here_are_reported_as_ambiguous() {
        let root = std::env::temp_dir().join(format!("zodiac-amb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cwd = "/two/agents";
        let dir = root.join(project_slug(cwd));
        std::fs::create_dir_all(&dir).unwrap();
        let started = SystemTime::now();
        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(dir.join("a.jsonl"), "{}\n").unwrap();
        std::fs::write(dir.join("b.jsonl"), "{}\n").unwrap();
        std::env::set_var("ZODIAC_PROJECTS_DIR", &root);
        let pick = find_session(cwd, started);
        std::env::remove_var("ZODIAC_PROJECTS_DIR");
        assert_eq!(
            pick,
            Pick::Ambiguous(2),
            "a coin flip must not read as fact"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_partial_final_line_is_not_folded_twice() {
        // The writer appends mid-poll: the incomplete line must be left for
        // the next read, and must not be counted now.
        let dir = std::env::temp_dir().join(format!("zodiac-tail-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("s.jsonl");
        let whole = serde_json::json!({"type": "assistant", "message": {"content": [],
            "usage": {"input_tokens": 1, "cache_read_input_tokens": 10,
                      "cache_creation_input_tokens": 0, "output_tokens": 5}}})
        .to_string();
        std::fs::write(&path, format!("{whole}\n{{\"type\":\"assis")).unwrap();
        let mut t = Tail {
            path: path.clone(),
            offset: 0,
            fold: TranscriptFold::default(),
        };
        assert!(read_new(&mut t));
        assert_eq!(t.fold.usage.output, 5);
        // Completing the line and appending another folds both, once each.
        let rest = serde_json::json!({"type": "assistant", "message": {"content": [],
            "usage": {"input_tokens": 2, "cache_read_input_tokens": 20,
                      "cache_creation_input_tokens": 0, "output_tokens": 7}}})
        .to_string();
        std::fs::write(&path, format!("{whole}\n{rest}\n")).unwrap();
        assert!(read_new(&mut t));
        assert_eq!(t.fold.usage.output, 12, "the first line must not re-fold");
        assert_eq!(t.fold.usage.context, 22);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
