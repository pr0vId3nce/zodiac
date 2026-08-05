//! The pane monitor — a headless background watcher for stuck panes,
//! and classifies them against a small local model, purely server-side so it
//! keeps working while detached. Deliberately independent of `chat.rs`
//! (the interactive chat panel): different model, different port, its own
//! tiny blocking HTTP client — the two never share state or contend for the
//! same backend.
//!
//! Advisory only: this module produces a classification and a policy
//! verdict, logs every evaluation, and hands the caller a message to notify
//! with. It never writes to a pane.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::server::SrvEvent;

/// CPU-only classifier model set up alongside the interactive one (see
/// llama-cpu.service on bigbox) — small and fast enough for a background
/// tick to never compete with the chat panel's GPU model.
const HOST: &str = "100.88.193.78";
const PORT: u16 = 8092;
const MODEL: &str = "qwen3-4b-cpu";

/// A pane "working" with an unchanged screen this long is suspicious enough
/// (stalled or looping) to be worth a classification.
pub const STALL_WORKING_AFTER: Duration = Duration::from_secs(180);
/// Don't re-classify the same pane more often than this, so a long-stuck
/// pane gets periodic reminders rather than a request every tick.
pub const RECHECK_INTERVAL: Duration = Duration::from_secs(90);

/// Card subtitles (Tier 3.1) are refreshed at most this often per pane —
/// frequent enough to feel alive, rare enough not to spam the CPU model.
pub const SUBTITLE_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Verdict {
    pub classification: String,
    pub confidence: f32,
    pub reason: String,
    #[serde(default)]
    pub policy_verdict: String,
    #[serde(default)]
    pub policy_reason: String,
}

const SYSTEM_PREFIX: &str = "You are a background monitor watching one terminal pane in a \
coding-agent multiplexer. You are given the tail of its screen. Classify what it is doing. \
If it is waiting on a permission prompt, also judge the pending action against the \
operator's policy below and say whether it's safe. Be terse and decisive; when unsure, say \
so via low confidence rather than guessing.\n\nPolicy:\n";

/// Classify a pane's screen tail against `policy` on a background thread,
/// logging the evaluation and sending the result back as `SrvEvent::
/// MonitorVerdict` on success. Silent no-op on any network/parse failure —
/// the next tick simply tries again.
pub fn classify_async(
    tx: Sender<SrvEvent>,
    session: String,
    pane_id: u64,
    pane_name: String,
    screen: String,
    policy: String,
) {
    std::thread::spawn(move || {
        let Some(v) = classify(&screen, &policy) else {
            return;
        };
        log_decision(&session, &pane_name, &v);
        let _ = tx.send(SrvEvent::MonitorVerdict(pane_id, pane_name, v));
    });
}

fn classify(screen: &str, policy: &str) -> Option<Verdict> {
    let system = format!("{SYSTEM_PREFIX}{policy}");
    let body = serde_json::json!({
        "model": MODEL,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": format!("Screen tail:\n{screen}")},
        ],
        "stream": false,
        "max_tokens": 200,
        "chat_template_kwargs": {"enable_thinking": false},
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "verdict",
                "schema": {
                    "type": "object",
                    "properties": {
                        "classification": {
                            "type": "string",
                            "enum": ["waiting_permission", "waiting_choice", "crashed",
                                      "rate_limited", "finished", "looping", "actually_working"],
                        },
                        "confidence": {"type": "number"},
                        "reason": {"type": "string"},
                        "policy_verdict": {
                            "type": "string",
                            "enum": ["safe", "unsafe", "unclear", "n/a"],
                        },
                        "policy_reason": {"type": "string"},
                    },
                    "required": ["classification", "confidence", "reason",
                                  "policy_verdict", "policy_reason"],
                },
            },
        },
    })
    .to_string();

    let content = post(&body)?;
    serde_json::from_str::<Verdict>(&content).ok()
}

fn post(body: &str) -> Option<String> {
    let addr = (HOST, PORT).to_socket_addrs().ok()?.next()?;
    let mut sock = TcpStream::connect_timeout(&addr, Duration::from_secs(4)).ok()?;
    let _ = sock.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = sock.set_write_timeout(Some(Duration::from_secs(6)));
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: {HOST}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    sock.write_all(req.as_bytes()).ok()?;
    sock.write_all(body.as_bytes()).ok()?;
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).ok()?;

    let split = find_bytes(&raw, b"\r\n\r\n")?;
    let (headers, rest) = raw.split_at(split);
    let rest = &rest[4..];
    let chunked = String::from_utf8_lossy(headers)
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked");
    let payload = if chunked { dechunk(rest) } else { rest.to_vec() };

    let v: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    v["choices"][0]["message"]["content"].as_str().map(str::to_string)
}

const SUBTITLE_SYSTEM: &str = "You are summarizing one terminal pane in a coding-agent \
multiplexer for a tiny status line painted on a card. Given the tail of its screen, describe \
what it is doing right now in 6 words or fewer, lowercase, no punctuation, no quotes. Be \
concrete — name the file, command, or topic if it's visible — not generic.";

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Subtitle {
    subtitle: String,
}

/// Summarize a pane's screen tail into a ≤6-word subtitle on a background
/// thread, sending the result back as `SrvEvent::MonitorSubtitle` on success.
/// Silent no-op on any network/parse failure — the next tick tries again.
pub fn summarize_async(tx: Sender<SrvEvent>, pane_id: u64, screen: String) {
    std::thread::spawn(move || {
        let Some(subtitle) = summarize(&screen) else {
            return;
        };
        let _ = tx.send(SrvEvent::MonitorSubtitle(pane_id, subtitle));
    });
}

fn summarize(screen: &str) -> Option<String> {
    let body = serde_json::json!({
        "model": MODEL,
        "messages": [
            {"role": "system", "content": SUBTITLE_SYSTEM},
            {"role": "user", "content": format!("Screen tail:\n{screen}")},
        ],
        "stream": false,
        "max_tokens": 40,
        "chat_template_kwargs": {"enable_thinking": false},
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "subtitle",
                "schema": {
                    "type": "object",
                    "properties": {
                        "subtitle": {"type": "string", "maxLength": 60},
                    },
                    "required": ["subtitle"],
                },
            },
        },
    })
    .to_string();

    let content = post(&body)?;
    let s: Subtitle = serde_json::from_str(&content).ok()?;
    let trimmed = s.subtitle.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn dechunk(mut s: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(nl) = find_bytes(s, b"\r\n") {
        let size_hex = std::str::from_utf8(&s[..nl]).unwrap_or("0").trim();
        let size = usize::from_str_radix(size_hex.split(';').next().unwrap_or("0"), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        let start = nl + 2;
        let end = (start + size).min(s.len());
        out.extend_from_slice(&s[start..end]);
        if end + 2 > s.len() {
            break;
        }
        s = &s[end + 2..];
    }
    out
}

// ---------------------------------------------------------------------------
// Policy file and decision log — both plain text/JSON-lines under the user's
// own config/state dirs, independent of Settings and of any session.

const DEFAULT_POLICY: &str = "\
Safe — approve without asking:
- reading files, listing directories, git status/diff/log, grep/rg
- running tests or type-checks (cargo test, npm test, pytest, tsc --noEmit)
- a routine build (cargo build, npm run build)

Not safe — always needs a human, no matter how the agent justifies it:
- deleting or overwriting anything outside the current repo
- git push --force, git reset --hard, rm -rf
- anything touching ~/nixos or system configuration
- outbound network requests that send data (POST, upload, publish)
- database writes or drops (DROP, DELETE, TRUNCATE, migrations)

Anything else: unclear.
";

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_default()
        .join("zodiac")
}

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_default()
        .join("zodiac")
        .join("monitor")
}

/// The operator's policy, bootstrapping a sensible default on first run so
/// the monitor works out of the box without requiring hand-authored config.
pub fn policy_text() -> String {
    let path = config_dir().join("monitor-policy.md");
    if let Ok(s) = std::fs::read_to_string(&path) {
        if !s.trim().is_empty() {
            return s;
        }
    }
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(&path, DEFAULT_POLICY);
    DEFAULT_POLICY.to_string()
}

#[derive(Serialize)]
struct DecisionEntry<'a> {
    ts_unix: u64,
    session: &'a str,
    pane: &'a str,
    classification: &'a str,
    confidence: f32,
    reason: &'a str,
    policy_verdict: &'a str,
    policy_reason: &'a str,
}

/// Append one evaluation to the durable decisions log — the dataset for
/// grading classification/policy accuracy before ever considering autonomy.
fn log_decision(session: &str, pane: &str, v: &Verdict) {
    let ts_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let entry = DecisionEntry {
        ts_unix,
        session,
        pane,
        classification: &v.classification,
        confidence: v.confidence,
        reason: &v.reason,
        policy_verdict: &v.policy_verdict,
        policy_reason: &v.policy_reason,
    };
    let Ok(line) = serde_json::to_string(&entry) else {
        return;
    };
    let dir = state_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    use std::fs::OpenOptions;
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(dir.join("decisions.log")) {
        let _ = writeln!(f, "{line}");
    }
}
