//! Golden-screen + TermEvent regression tests over the recorded corpus.
//!
//! Every .ptyrec in tests/corpus/ is replayed through the vt100 parser at
//! its recorded size; the final screen (text + per-cell attrs) and the
//! ordered TermEvent log are compared against tests/golden/<name>.*.txt.
//!
//! Missing goldens are written and the test fails once, asking for review.
//! To re-baseline intentionally: UPDATE_GOLDEN=1 cargo test --test golden,
//! and put a `REBASELINE: <reason>` note in the commit message
//! (docs/roadmap.md, Guardrails).

#[path = "../src/corpus.rs"]
mod corpus;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// Screen dump: one text line per row (trailing blanks trimmed), then for
/// rows with any non-default attrs a run-length attr line. Stable and
/// readable so golden diffs point at the cell that changed.
fn dump_screen(screen: &vt100::Screen) -> String {
    let (rows, cols) = screen.size();
    let mut out = String::new();
    for r in 0..rows {
        let mut text = String::new();
        let mut attrs = String::new();
        let mut run_start = 0u16;
        let mut run_desc = String::new();
        let flush = |from: u16, to: u16, desc: &str, attrs: &mut String| {
            if !desc.is_empty() {
                let _ = write!(attrs, " [{from}-{to} {desc}]");
            }
        };
        for c in 0..cols {
            let (chars, desc) = match screen.cell(r, c) {
                Some(cell) => {
                    let mut d = String::new();
                    match cell.fgcolor() {
                        vt100::Color::Default => {}
                        fg => {
                            let _ = write!(d, "fg={fg:?}");
                        }
                    }
                    match cell.bgcolor() {
                        vt100::Color::Default => {}
                        bg => {
                            let _ = write!(d, "{}bg={bg:?}", if d.is_empty() { "" } else { "," });
                        }
                    }
                    for (on, tag) in [
                        (cell.bold(), "B"),
                        (cell.italic(), "I"),
                        (cell.underline(), "U"),
                        (cell.inverse(), "R"),
                    ] {
                        if on {
                            d.push_str(tag);
                        }
                    }
                    let ch = cell.contents();
                    (if ch.is_empty() { " ".to_string() } else { ch }, d)
                }
                None => (" ".to_string(), String::new()),
            };
            if desc != run_desc {
                flush(run_start, c.wrapping_sub(1), &run_desc, &mut attrs);
                run_start = c;
                run_desc = desc;
            }
            text.push_str(&chars);
        }
        flush(run_start, cols - 1, &run_desc, &mut attrs);
        let _ = writeln!(out, "{:3}|{}", r, text.trim_end());
        if !attrs.is_empty() {
            let _ = writeln!(out, "   |{}", attrs);
        }
    }
    out
}

fn compare(name: &str, kind: &str, actual: &str) {
    let path = golden_dir().join(format!("{name}.{kind}.txt"));
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    if update || !path.exists() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        std::fs::write(&path, actual).unwrap();
        assert!(
            update,
            "golden {} did not exist; wrote it — review and commit, then re-run",
            path.display()
        );
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    if expected != actual {
        let diff_at = expected
            .lines()
            .zip(actual.lines())
            .position(|(e, a)| e != a)
            .map(|i| {
                let e = expected.lines().nth(i).unwrap_or("");
                let a = actual.lines().nth(i).unwrap_or("");
                format!("first differing line {}:\n  golden: {e}\n  actual: {a}", i + 1)
            })
            .unwrap_or_else(|| "line counts differ".into());
        panic!(
            "golden mismatch for {name}.{kind} — {diff_at}\n\
             (UPDATE_GOLDEN=1 re-baselines; commit with a REBASELINE: note)"
        );
    }
}

fn replay(name: &str) {
    let file = corpus_dir().join(format!("{name}.ptyrec"));
    let rec = corpus::read(std::fs::File::open(&file).unwrap()).unwrap();
    let mut parser = vt100::Parser::new(rec.rows, rec.cols, 0);
    parser.enable_events();
    let mut events = String::new();
    let mut screens = String::new();
    // Full-screen apps restore a blank primary screen on exit, so an
    // EOF-only dump would capture nothing: checkpoint at eight evenly
    // spaced points through the recording as well.
    let marks: std::collections::BTreeSet<usize> =
        (1..8).map(|k| k * rec.chunks.len() / 8).collect();
    for (i, chunk) in rec.chunks.iter().enumerate() {
        match chunk.kind {
            corpus::ChunkKind::Output => parser.process(&chunk.payload),
            corpus::ChunkKind::Resize => {
                if let Some((rows, cols)) = chunk.resize() {
                    // Checkpoint the screen as it stood before the resize.
                    let _ = writeln!(screens, "=== before resize @chunk {i} ===");
                    screens.push_str(&dump_screen(parser.screen()));
                    parser.set_size(rows, cols);
                }
            }
        }
        for ev in parser.drain_events() {
            let _ = writeln!(events, "chunk {i:4}: {ev:?}");
        }
        if marks.contains(&i) {
            let _ = writeln!(screens, "=== @chunk {i} ===");
            screens.push_str(&dump_screen(parser.screen()));
        }
    }
    let _ = writeln!(screens, "=== final ===");
    screens.push_str(&dump_screen(parser.screen()));
    compare(name, "screen", &screens);
    compare(name, "events", &events);
}

macro_rules! golden {
    ($($name:ident),* $(,)?) => {
        $(#[test]
        fn $name() {
            replay(stringify!($name));
        })*
    };
}

golden!(vim, less, fzf, htop);

/// The graphics recording goes through the same replay; the vt100 layer
/// only sees what's left after vte drops the APC (GfxSplitter handling is
/// covered separately by the gfx goldens).
#[test]
fn timg_kitty() {
    replay("timg-kitty");
}

/// Every corpus file must be covered by a test above — a recording that
/// isn't replayed is a silent coverage hole.
#[test]
fn corpus_fully_covered() {
    let covered = ["vim", "less", "fzf", "htop", "timg-kitty"];
    for entry in std::fs::read_dir(corpus_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "ptyrec") {
            let stem = path.file_stem().unwrap().to_string_lossy().to_string();
            assert!(
                covered.contains(&stem.as_str()),
                "corpus file {stem}.ptyrec has no golden test — add it to the golden! list"
            );
        }
    }
}
