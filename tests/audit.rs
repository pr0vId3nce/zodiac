//! Replays the corpus with the vt100 debug log captured: any "unhandled"
//! sequence line is a regression against ADR 0001's zero-noise guarantee
//! (docs/roadmap.md, Phase 1 exit criteria).
//!
//! Lives in its own integration-test crate because it installs the global
//! logger — nothing else here may log.

#[path = "../src/corpus.rs"]
mod corpus;
#[path = "../src/engine.rs"]
mod engine;

use engine::TermEngine as _;
use std::sync::Mutex;

struct Capture(Mutex<Vec<String>>);
static CAP: Capture = Capture(Mutex::new(Vec::new()));

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, rec: &log::Record) {
        self.0.lock().unwrap().push(format!("{}", rec.args()));
    }
    fn flush(&self) {}
}

#[test]
fn corpus_produces_no_unhandled_sequences() {
    log::set_logger(&CAP).unwrap();
    log::set_max_level(log::LevelFilter::Debug);
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "ptyrec") {
            continue;
        }
        let rec = corpus::read(std::fs::File::open(&path).unwrap()).unwrap();
        let mut term = engine::Vt100Engine::new(rec.rows, rec.cols, 0);
        for chunk in &rec.chunks {
            match chunk.kind {
                corpus::ChunkKind::Output => term.process(&chunk.payload),
                corpus::ChunkKind::Resize => {
                    if let Some((r, c)) = chunk.resize() {
                        term.resize(r, c);
                    }
                }
            }
            term.drain_events();
        }
        let mut lines = CAP.0.lock().unwrap();
        for l in lines.drain(..) {
            offenders.push(format!(
                "{}: {l}",
                path.file_name().unwrap().to_string_lossy()
            ));
        }
    }
    offenders.sort();
    offenders.dedup();
    assert!(
        offenders.is_empty(),
        "corpus replay logged unhandled sequences (close the gap in \
         vendor/vt100 or record the decision in an ADR):\n{}",
        offenders.join("\n")
    );
}
