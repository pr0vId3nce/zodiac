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
                None => {
                    let Some(path) = find_session(&p.cwd, started) else {
                        continue;
                    };
                    tails.entry(p.id).or_insert(Tail {
                        path,
                        offset: 0,
                        fold: TranscriptFold::default(),
                    })
                }
            };
            if read_new(tail) {
                let snap = PaneAgent {
                    usage: tail.fold.usage,
                    files: tail.fold.files.clone(),
                    session: tail
                        .path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned()),
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
pub fn find_session(cwd: &str, started: SystemTime) -> Option<PathBuf> {
    let dir = projects_root()?.join(project_slug(cwd));
    let mut best: Option<(SystemTime, bool, PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        let Ok(modified) = md.modified() else {
            continue;
        };
        // Untouched since the pane began: it belongs to some other run.
        if modified < started {
            continue;
        }
        let born_here = md.created().map(|c| c >= started).unwrap_or(false);
        let better = match &best {
            None => true,
            // A session born in this pane beats an older one that merely got
            // written to (e.g. resumed in a different window).
            Some((bt, bb, _)) => (born_here, modified) > (*bb, *bt),
        };
        if better {
            best = Some((modified, born_here, path));
        }
    }
    best.map(|(_, _, p)| p)
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
