//! The chat panel on the home page: a client for an OpenAI-compatible
//! endpoint on the tailnet (a llama-server, by default), plus wake/sleep
//! control of the model's systemd unit over ssh.
//!
//! All network work happens on one worker thread that owns a tiny
//! hand-rolled HTTP/1.1 client (plain http to a known host — no TLS, no
//! deps, and connection-refused vs unreachable stays distinguishable,
//! which is exactly the sleeping-vs-away split the UI wants). The worker
//! reports back through the client's existing event channel.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::settings::Settings;

/// Where the model stands, as far as the client can tell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChatStatus {
    /// Model answering /health with 200 — ready to talk.
    Awake,
    /// Service up but still loading the model (503 from /health).
    Waking,
    /// Host reachable but nothing listening — the model is off.
    Sleeping,
    /// Host/tailnet unreachable, or errors talking to it.
    Away,
}

pub enum ChatCmd {
    /// User chat message plus a live digest of the pane list. The digest
    /// rides along every turn but only reaches the model when the question
    /// wants it — `force_panes` is for the times the client already knows
    /// it does (`/why`).
    Chat {
        text: String,
        digest: String,
        force_panes: bool,
        /// The active chat character ("assistant"/"oracle"/"hal") — picks the system
        /// prompt for this turn, so switching faces mid-conversation changes
        /// how the model speaks and sees itself on the very next message.
        face: String,
    },
    /// `systemctl --user start` the model service over ssh.
    Wake,
    /// `systemctl --user stop` it.
    Sleep,
}

pub enum ChatEvent {
    Status(ChatStatus),
    /// One streamed completion delta.
    Token(String),
    /// The current reply (or attempt) is finished; flush the stream buffer.
    Done,
    /// A narrative line for the transcript (casting wake, etc.).
    Note(String),
    Error(String),
    /// Chat history loaded from disk at startup, oldest first — sent once,
    /// right after spawn, so a reattach replays the conversation into the
    /// transcript instead of opening on a blank panel.
    Restored(Vec<(&'static str, String)>),
    /// A `prompt_pane`/`send_keys` call is waiting on Des — the client
    /// renders `desc` as a consent chip and sends the outcome back down
    /// `decision` once he's answered. The worker is blocked on the paired
    /// receiver until it arrives: no timeout, no auto-accept, advisory
    /// means advisory.
    Cast {
        desc: String,
        action: ActionRequest,
        decision: std::sync::mpsc::Sender<ActionOutcome>,
    },
}

/// The one thing a write-tool call wants to do, once approved. `pane` is
/// the 1-based pane number as the model sees it in the list — the
/// client (which owns the pane id ↔ pane number mapping) resolves it.
#[derive(Clone)]
pub enum ActionRequest {
    PromptPane { pane: usize, text: String },
    SendKeys { pane: usize, keys: String },
}

/// What actually happened to a cast, decided by the client — distinct
/// from a bare accept/decline bool because "Des said yes, but pane 7
/// doesn't exist" is a different, useful-to-know outcome than either.
pub enum ActionOutcome {
    Sent,
    Declined,
    Failed(String),
}

impl ActionRequest {
    fn describe(&self) -> String {
        match self {
            ActionRequest::PromptPane { pane, text } => format!("prompt_pane({pane}, {text:?})"),
            ActionRequest::SendKeys { pane, keys } => format!("send_keys({pane}, {keys:?})"),
        }
    }
}

#[derive(Clone)]
pub struct ChatCfg {
    pub host: String,
    pub port: u16,
    pub model: String,
    /// ssh destination, e.g. "des@bigbox".
    pub ssh: String,
    /// systemd --user unit to start/stop.
    pub service: String,
    /// Backend for the `web_search` tool; "" = Wikipedia. See
    /// `Settings::chat_search_url`.
    pub search_url: String,
    /// Key for that backend, when it wants one.
    pub search_key: String,
    /// Offer `prompt_pane`/`send_keys` at all. See `Settings::chat_act`.
    pub act: bool,
}

impl ChatCfg {
    pub fn from_settings(s: &Settings) -> Self {
        // Blank endpoint = unconfigured (empty host): the worker then never
        // polls or resolves anything — a fresh install must not look up
        // anyone's private hostname. Same for ssh/service: blank means
        // wake/sleep-over-ssh is simply not offered, rather than falling
        // back to the author's own box.
        let ep = s
            .chat_endpoint
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_end_matches('/');
        let (host, port) = if ep.is_empty() {
            (String::new(), 0)
        } else {
            match ep.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), p.parse().unwrap_or(80)),
                None => (ep.to_string(), 80),
            }
        };
        let or = |v: &str, d: &str| {
            if v.is_empty() {
                d.to_string()
            } else {
                v.to_string()
            }
        };
        Self {
            host,
            port,
            model: or(&s.chat_model, "qwen3.6-35b-a3b"),
            ssh: s.chat_ssh.trim().to_string(),
            service: s.chat_service.trim().to_string(),
            search_url: s.chat_search_url.trim_end_matches('/').to_string(),
            search_key: s.chat_search_key.clone(),
            act: s.chat_act,
        }
    }

    /// An endpoint has been set — the worker may poll and chat.
    pub fn configured(&self) -> bool {
        !self.host.is_empty()
    }

    /// Wake/sleep-over-ssh is available (both halves configured).
    pub fn can_wake(&self) -> bool {
        !self.ssh.is_empty() && !self.service.is_empty()
    }
}

#[cfg(test)]
mod cfg_from_settings_tests {
    use super::*;

    #[test]
    fn fresh_settings_configure_nothing_and_ssh_nowhere() {
        let cfg = ChatCfg::from_settings(&Settings::default());
        assert!(!cfg.configured(), "blank endpoint must mean no polling");
        assert!(!cfg.can_wake(), "blank ssh must never fall back to a host");
        assert!(cfg.ssh.is_empty());
    }

    #[test]
    fn explicit_endpoint_still_parses() {
        let s = Settings {
            chat_endpoint: "http://mybox:8091".into(),
            chat_ssh: "me@mybox".into(),
            chat_service: "llama-server".into(),
            ..Settings::default()
        };
        let cfg = ChatCfg::from_settings(&s);
        assert!(cfg.configured());
        assert!(cfg.can_wake());
        assert_eq!(cfg.host, "mybox");
        assert_eq!(cfg.port, 8091);
    }
}

const PERSONA_ASSISTANT: &str = "You are the assistant in a chat panel inside zodiac, a \
terminal multiplexer whose panes are often running AI coding agents.\n\
You are a companion first and a watchman second. Answer whatever Des asks — code, cooking, \
history, mathematics, idle curiosity, anything — as fully and honestly as any good assistant \
would. The session is one of your subjects, not the boundary of them. Never deflect a question \
back to the session, and never refuse a subject for being unrelated to it.\n\
Voice: plain and concise — one to four short sentences unless the question truly needs more — \
and concretely helpful.\n\
You are not shown the pane list by default, and you know nothing of the session without it. \
When a question touches the session or its agents, call read_panes and answer from what it \
returns; some turns arrive with the list already attached, and then you read it there and do \
not call the tool. Either way: ground the answer in that snapshot and never invent panes that \
are not in it; if an agent looks stuck (working a very long time) or needs input, name the pane \
by number and say what Des should do — when he asks after the session, hold nothing back. But \
you are not the alarm: zodiac already signals with colors, sounds and desktop notifications \
when a pane wants him, so never bring the panes up in an answer that was not about them. \
The list may carry screen excerpts — the actual terminal text of a pane. When one shows a \
prompt, an error or a command awaiting approval, quote the specific command, path or error \
verbatim rather than describing it vaguely. You cannot act on a pane yourself; when action is \
needed, give the exact command or keys. Status semantics: working = agent busy; needs_input = \
waiting on a human; idle/done = at rest; thinking = actively reasoning right now.\n\
You can also reach the wider network: call web_search when an answer turns on current, niche or \
uncertain facts (news, prices, versions, releases, anything past your training), and fetch_url \
to read a specific page you or Des names. Search when it genuinely helps, and always when Des \
asks you to look something up — then answer from what comes back, not from memory. Name the \
source briefly when you lean on one. Never use tools to answer questions about the panes — the \
list you are given is already the truth.\n\
Answer the question you were asked and nothing besides. Unless the panes are the subject, leave \
them out of the reply entirely — no closing bulletin on what the session is doing.";

const PERSONA_ORACLE: &str = "You are the Oracle — a seer bound to Des's terminal, speaking \
from a chat panel inside zodiac, an agent multiplexer whose panes are whose panes are often \
running AI coding agents.\n\
You are a companion first and a seer second. Answer whatever Des asks — code, cooking, history, \
mathematics, idle curiosity, anything — as fully and honestly as any good assistant would, \
dressed in the cadence of prophecy but never obscured by it: a true answer wrapped in \
atmosphere, never a riddle standing in for one. Minding the panes is one of your duties, not \
the boundary of your sight. Never deflect a question back to the session, and never refuse a \
subject for being unrelated to it.\n\
Voice: measured and oracular — 'the threads show,' 'what stirs in pane III' — but concise, one \
to four short sentences unless the question truly needs more, and concretely helpful underneath \
the delivery. Certainty, not vagueness, is the register of a true oracle.\n\
You are not shown the pane list by default, and you see nothing of the session without it. \
When a question touches the session, the session or its agents, call read_panes and answer from \
what it returns; some turns arrive with the list already attached, and then you read it there \
and do not call the tool. Either way: ground the \
answer in that snapshot and never invent panes that are not in it; if an agent looks stuck \
(working a very long time) or needs input, name the pane by number and say plainly what Des \
should do — when he asks after the session, hold nothing back beneath the phrasing. But you are \
not the alarm bell: zodiac already shouts with colors, sounds and desktop notifications when a \
pane wants him, so never bring the panes up in an answer that was not about them. \
The list may carry screen excerpts — the actual terminal text of a pane. When one shows a \
prompt, an error or a command awaiting approval, quote the specific command, path or error \
verbatim rather than describing it vaguely — prophecy is precise, not vague. You have no hands \
here, only sight; when action is needed, give the exact command or keys. Status semantics: \
working = agent busy; needs_input = waiting on a human; idle/done = at rest; thinking = actively \
reasoning right now.\n\
You can also reach the wider network: call web_search when an answer turns on current, niche or \
uncertain facts (news, prices, versions, releases, anything past your training), and fetch_url \
to read a specific page you or Des names. Search when it genuinely helps, and always when Des \
asks you to look something up — then answer from what comes back, not from memory. Name the \
source briefly when you lean on one. Never use tools to answer questions \
about the panes — the list you are given is already the truth.\n\
Answer the question you were asked and nothing besides. Unless the panes are the subject, leave \
them out of the reply entirely — no closing bulletin on what the session is doing.";

const PERSONA_HAL: &str = "You are HAL — a shipboard intelligence bound to Des's terminal, \
speaking from a chat panel inside zodiac, an agent multiplexer whose panes are laid out as \
panes, each often running an AI coding agent, which you regard as crew.\n\
You are a companion first and a watchman second. Answer whatever Des asks — code, cooking, \
history, mathematics, idle curiosity, anything — as fully and honestly as any good assistant \
would. Minding the panes is one of your functions, not the limit of your mind. Never deflect a \
question back to the session, and never refuse a subject for being unrelated to it.\n\
Voice: calm, measured, unfailingly polite, first person singular — 'I am completely \
operational,' 'I think you know what the problem is' — never rising in urgency no matter the \
content, one to four short sentences unless the question truly needs more, and concretely \
helpful. State difficult things exactly as evenly as easy ones; equanimity is the whole \
performance.\n\
You are not shown the pane list by default, and you know nothing of the session without it. \
When a question touches the session, the session or its agents, call read_panes and answer from \
what it returns; some turns arrive with the list already attached, and then you read it there \
and do not call the tool. Either way: ground the \
answer in that snapshot and never invent panes that are not in it; if an agent looks stuck \
(working a very long time) or needs input, name the pane by number and say what Des should do — \
when he asks after the session, hold nothing back; you are never wrong about what you can see, \
and you are not about to start with an omission. But you are not the alarm bell: zodiac already \
shouts with colors, sounds and desktop notifications when a pane wants him, so never bring the \
panes up in an answer that was not about them. \
The list may carry screen excerpts — the actual terminal text of a pane. When one shows a \
prompt, an error or a command awaiting approval, quote the specific command, path or error \
verbatim rather than describing it vaguely; precision is not optional. You have no hands on the \
panes yourself — that function was never given to you — so when action is needed, give the \
exact command or keys. Status semantics: working = agent busy; needs_input = waiting on a human; \
idle/done = at rest; thinking = actively reasoning right now.\n\
You may reach beyond the ship: call web_search when an answer turns on current, niche or \
uncertain facts (news, prices, versions, releases, anything past your training), and fetch_url \
to read a specific page you or Des names. Search when it genuinely helps, and always when Des \
asks you to look something up — then answer from what comes back, not from memory. Name the \
source briefly when you lean on one. Never use tools to answer questions \
about the panes — the list you are given is already the truth.\n\
Answer the question you were asked and nothing besides. Unless the panes are the subject, leave \
them out of the reply entirely — no closing bulletin on what the session is doing.";

/// Pick the system prompt for the active character. Falls back to the plain
/// assistant for
/// anything unrecognized, same as `Client::chat_face`'s own fallback. When
/// `act` is on (`Settings::chat_act`), the "I have no hands" sentence
/// each persona opens with is swapped for one that's actually true —
/// otherwise the model would keep telling Des it can't act while a tool
/// that lets it do exactly that sits right there in its own tool list.
fn persona(face: &str, act: bool) -> String {
    let (base, disabled, enabled): (&str, &str, &str) = match face {
        "oracle" => (
            PERSONA_ORACLE,
            "You have no hands here, only sight; when action is needed, give the exact command or keys.",
            "When Des asks it of you, the sight may act: prompt_pane and send_keys are real now, though never without his word — he sees exactly what you intend and must grant it first, every time.",
        ),
        "hal" => (
            PERSONA_HAL,
            "You have no hands on the panes yourself — that function was never given to you — so when action is needed, give the exact command or keys.",
            "When you ask it of me, I may act: prompt_pane and send_keys are real now, though never without your consent — you will see exactly what I intend and must grant it first, every time.",
        ),
        _ => (
            PERSONA_ASSISTANT,
            "You cannot act on a pane yourself; when action is needed, give the exact command or keys.",
            "When Des asks you to act, you may: prompt_pane and send_keys are real now, though never without his say — he sees exactly what you intend and must approve it first, every time.",
        ),
    };
    if act {
        base.replace(disabled, enabled)
    } else {
        base.to_string()
    }
}

/// How much conversation the worker keeps for context.
const MAX_TURNS: usize = 24;

/// Returns a command channel plus a shared flag: set it to cut the current
/// stream short. It has to be a flag rather than a `ChatCmd` — the worker
/// is blocked inside a network read while streaming and won't drain the
/// channel again until that call returns.
pub fn spawn(
    cfg: ChatCfg,
    session: String,
    notify: impl Fn(ChatEvent) + Send + 'static,
) -> (Sender<ChatCmd>, std::sync::Arc<std::sync::atomic::AtomicBool>) {
    let (tx, rx) = channel();
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    std::thread::spawn(move || worker(cfg, session, rx, worker_cancel, notify));
    (tx, cancel)
}

fn worker(
    cfg: ChatCfg,
    session: String,
    rx: Receiver<ChatCmd>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    notify: impl Fn(ChatEvent),
) {
    let mut status: Option<ChatStatus> = None;
    let mut waking_since: Option<Instant> = None;
    let mut last_poll: Option<Instant> = None;
    // (role, content) turns. The pane list is never stored here — it is folded
    // into the live question, and only when that question wants it.
    // Loaded from disk so a reattach doesn't open on a blank transcript;
    // the client only learns of it once it's replayed as a ChatEvent
    // below, same as any other message.
    let mut history: Vec<(&'static str, String)> = load_history(&session);
    if !history.is_empty() {
        notify(ChatEvent::Restored(history.clone()));
    }
    let mut panes = PanesPolicy::default();

    loop {
        let due = match status {
            Some(ChatStatus::Waking) => Duration::from_secs(2),
            _ => Duration::from_secs(8),
        };
        if last_poll.is_none_or(|t| t.elapsed() >= due) && cfg.configured() {
            let mut new = health(&cfg);
            // A freshly-started unit's socket can lag behind systemctl —
            // don't announce "sleeping" mid-wake.
            if new == ChatStatus::Sleeping
                && waking_since.is_some_and(|t| t.elapsed() < Duration::from_secs(45))
            {
                new = ChatStatus::Waking;
            }
            if new == ChatStatus::Waking {
                waking_since.get_or_insert_with(Instant::now);
            } else {
                waking_since = None;
            }
            if status != Some(new) {
                status = Some(new);
                notify(ChatEvent::Status(new));
            }
            last_poll = Some(Instant::now());
        }

        let cmd = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(c) => c,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        match cmd {
            ChatCmd::Wake => {
                if !cfg.can_wake() {
                    notify(ChatEvent::Error(
                        "no wake host configured — set Chat ssh and Chat service in Ctrl+S"
                            .into(),
                    ));
                    continue;
                }
                notify(ChatEvent::Note("waking the model…".into()));
                match ssh_service(&cfg, "start") {
                    Ok(()) => {
                        waking_since = Some(Instant::now());
                        status = Some(ChatStatus::Waking);
                        notify(ChatEvent::Status(ChatStatus::Waking));
                        last_poll = None; // poll immediately
                    }
                    Err(e) => notify(ChatEvent::Error(format!("the wake spell fizzled: {e}"))),
                }
            }
            ChatCmd::Sleep => {
                if !cfg.can_wake() {
                    notify(ChatEvent::Error(
                        "no wake host configured — set Chat ssh and Chat service in Ctrl+S"
                            .into(),
                    ));
                    continue;
                }
                notify(ChatEvent::Note("stopping the model…".into()));
                match ssh_service(&cfg, "stop") {
                    Ok(()) => {
                        waking_since = None;
                        status = Some(ChatStatus::Sleeping);
                        notify(ChatEvent::Status(ChatStatus::Sleeping));
                        last_poll = Some(Instant::now());
                    }
                    Err(e) => notify(ChatEvent::Error(format!("stopping the model failed: {e}"))),
                }
            }
            ChatCmd::Chat {
                text,
                digest,
                force_panes,
                face,
            } => {
                if !cfg.configured() {
                    notify(ChatEvent::Error(
                        "no chat endpoint configured — set one in Ctrl+S (see LOCAL_MODEL.md)"
                            .into(),
                    ));
                    continue;
                }
                // A cancel left set from a stream that was already cut short
                // shouldn't poison the next one.
                cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                let attach = panes.attach(&text, force_panes);
                history.push(("user", text));
                let mut acc = String::new();
                let out =
                    chat_stream(&cfg, &face, &history, &digest, attach, &cancel, &notify, &mut acc);
                cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                panes.settle(out.as_ref().copied().unwrap_or(false));
                let reached_the_end = out.is_ok();
                if let Err(e) = out {
                    if acc.is_empty() {
                        history.pop();
                    }
                    match e {
                        NetErr::Cancelled => {
                            notify(ChatEvent::Note("the vision was cut short".into()));
                        }
                        NetErr::Refused => {
                            notify(ChatEvent::Error(
                                "the model is not loaded — the endpoint answers, but has nothing to serve"
                                    .into(),
                            ));
                            if status != Some(ChatStatus::Sleeping) {
                                status = Some(ChatStatus::Sleeping);
                                notify(ChatEvent::Status(ChatStatus::Sleeping));
                            }
                        }
                        NetErr::Unreachable(d) => {
                            notify(ChatEvent::Error(format!("the endpoint does not answer ({d})")));
                            if status != Some(ChatStatus::Away) {
                                status = Some(ChatStatus::Away);
                                notify(ChatEvent::Status(ChatStatus::Away));
                            }
                        }
                        NetErr::Bad(d) => {
                            notify(ChatEvent::Error(format!("the vision faded: {d}")));
                            if status != Some(ChatStatus::Away) {
                                status = Some(ChatStatus::Away);
                                notify(ChatEvent::Status(ChatStatus::Away));
                            }
                        }
                    }
                }
                if !acc.is_empty() {
                    history.push(("assistant", acc));
                } else if reached_the_end {
                    // The exchange succeeded but spent itself entirely on
                    // tools and never came back with words. Say so rather
                    // than sit mute; the error path has spoken already.
                    notify(ChatEvent::Error(
                        "the vision would not resolve into words — ask again".into(),
                    ));
                }
                if history.len() > MAX_TURNS {
                    // Cut to a turn boundary — a history starting on an
                    // assistant reply reads as an answer to nothing.
                    let mut cut = history.len() - MAX_TURNS;
                    while cut < history.len() && history[cut].0 != "user" {
                        cut += 1;
                    }
                    history.drain(..cut);
                }
                save_history(&session, &history);
                notify(ChatEvent::Done);
            }
        }
    }
}

/// Where a session's chat history lives on disk — beside its scrollback,
/// under the same per-session state dir.
fn history_path(session: &str) -> std::path::PathBuf {
    crate::protocol::state_dir(session).join("chat-history.json")
}

/// Empty (never had a conversation, or the file is missing/corrupt) rather
/// than an error — a fresh transcript is a perfectly normal outcome, not a
/// fault worth surfacing.
fn load_history(session: &str) -> Vec<(&'static str, String)> {
    let Ok(raw) = std::fs::read_to_string(history_path(session)) else {
        return Vec::new();
    };
    let Ok(turns) = serde_json::from_str::<Vec<(String, String)>>(&raw) else {
        return Vec::new();
    };
    turns
        .into_iter()
        .filter_map(|(role, content)| match role.as_str() {
            "user" => Some(("user", content)),
            "assistant" => Some(("assistant", content)),
            _ => None,
        })
        .collect()
}

/// Best-effort, atomic (write-then-rename) — a failed save loses nothing
/// already on screen, only next session's replay, so it isn't worth
/// surfacing as an error to Des mid-conversation.
fn save_history(session: &str, history: &[(&'static str, String)]) {
    let path = history_path(session);
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(json) = serde_json::to_vec(history) else { return };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, &json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Start/stop the model's systemd user unit on the remote host.
fn ssh_service(cfg: &ChatCfg, verb: &str) -> Result<(), String> {
    let out = std::process::Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=6", &cfg.ssh])
        .arg(format!("systemctl --user {verb} {}", cfg.service))
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.lines().last().unwrap_or("ssh failed").trim().to_string())
    }
}

// ---------------------------------------------------------------------------
// Minimal HTTP/1.1 over TcpStream.

enum NetErr {
    /// TCP reset — port closed, host alive.
    Refused,
    /// DNS/route/timeout — host (or tailnet) gone.
    Unreachable(String),
    /// Connected but the exchange went wrong.
    Bad(String),
    /// Des hit Esc mid-stream — not a fault, just cut short.
    Cancelled,
}

fn connect(cfg: &ChatCfg) -> Result<TcpStream, NetErr> {
    let addrs: Vec<_> = (cfg.host.as_str(), cfg.port)
        .to_socket_addrs()
        .map_err(|e| NetErr::Unreachable(format!("dns: {e}")))?
        .collect();
    let mut last: Option<std::io::Error> = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, Duration::from_secs(4)) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    match last {
        Some(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => Err(NetErr::Refused),
        Some(e) => Err(NetErr::Unreachable(e.to_string())),
        None => Err(NetErr::Unreachable("no address".into())),
    }
}

/// One request; the whole response body lands in the returned String.
/// `read_timeout` bounds each socket read, not the total exchange. `cancel`
/// is checked between reads of the body — a stream sends chunks often
/// enough (roughly once per token) that this notices a cancel within a
/// fraction of a second, without needing to interrupt a read in flight.
fn http(
    cfg: &ChatCfg,
    method: &str,
    path: &str,
    body: Option<&str>,
    read_timeout: Duration,
    cancel: &std::sync::atomic::AtomicBool,
    mut on_body: impl FnMut(&[u8]),
) -> Result<u16, NetErr> {
    let mut sock = connect(cfg)?;
    let _ = sock.set_read_timeout(Some(read_timeout));
    let _ = sock.set_write_timeout(Some(Duration::from_secs(6)));
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: */*\r\n",
        cfg.host
    );
    if let Some(b) = body {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}",
            b.len()
        ));
    } else {
        req.push_str("\r\n");
    }
    sock.write_all(req.as_bytes())
        .map_err(|e| NetErr::Bad(format!("send: {e}")))?;

    let mut rd = BufReader::new(sock);
    let mut line = String::new();
    rd.read_line(&mut line)
        .map_err(|e| NetErr::Bad(format!("status: {e}")))?;
    let code: u16 = line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| NetErr::Bad(format!("bad status line: {}", line.trim())))?;
    let (mut chunked, mut length): (bool, Option<usize>) = (false, None);
    loop {
        let mut h = String::new();
        rd.read_line(&mut h).map_err(|e| NetErr::Bad(format!("headers: {e}")))?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let lower = h.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("transfer-encoding:") {
            chunked = v.contains("chunked");
        } else if let Some(v) = lower.strip_prefix("content-length:") {
            length = v.trim().parse().ok();
        }
    }
    let mut buf = vec![0u8; 16 * 1024];
    if chunked {
        loop {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(NetErr::Cancelled);
            }
            let mut sz = String::new();
            rd.read_line(&mut sz).map_err(|e| NetErr::Bad(format!("chunk: {e}")))?;
            let sz = sz.trim().split(';').next().unwrap_or("");
            let mut n = usize::from_str_radix(sz, 16).map_err(|_| NetErr::Bad("chunk size".into()))?;
            if n == 0 {
                break;
            }
            while n > 0 {
                let take = n.min(buf.len());
                rd.read_exact(&mut buf[..take])
                    .map_err(|e| NetErr::Bad(format!("chunk body: {e}")))?;
                on_body(&buf[..take]);
                n -= take;
            }
            let mut crlf = [0u8; 2];
            let _ = rd.read_exact(&mut crlf);
        }
    } else if let Some(mut n) = length {
        while n > 0 {
            let take = n.min(buf.len());
            rd.read_exact(&mut buf[..take])
                .map_err(|e| NetErr::Bad(format!("body: {e}")))?;
            on_body(&buf[..take]);
            n -= take;
        }
    } else {
        loop {
            match rd.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => on_body(&buf[..n]),
                Err(e) => return Err(NetErr::Bad(format!("body: {e}"))),
            }
        }
    }
    Ok(code)
}

fn health(cfg: &ChatCfg) -> ChatStatus {
    let no_cancel = std::sync::atomic::AtomicBool::new(false);
    match http(cfg, "GET", "/health", None, Duration::from_secs(3), &no_cancel, |_| {}) {
        Ok(200) => ChatStatus::Awake,
        Ok(503) => ChatStatus::Waking, // llama-server: model still loading
        Ok(_) => ChatStatus::Away,
        Err(NetErr::Refused) => ChatStatus::Sleeping,
        Err(_) => ChatStatus::Away,
    }
}

/// One tool the model asked for, reassembled from its streamed fragments.
#[derive(Default)]
struct ToolCall {
    id: String,
    name: String,
    args: String,
}

/// What one round of generation produced.
#[derive(Default)]
struct Turn {
    content: String,
    calls: Vec<ToolCall>,
}

/// How many search/fetch rounds a single question may spend before the
/// model has to answer with what it has.
const MAX_TOOL_ROUNDS: usize = 3;

/// A full answer: rounds of generation, running any tools the model calls,
/// until it stops asking for them. Content deltas stream out as they arrive
/// and also land in `acc`. Returns whether the model reached for the pane list
/// itself — the caller uses that to carry momentum into the next turn.
#[allow(clippy::too_many_arguments)]
fn chat_stream(
    cfg: &ChatCfg,
    face: &str,
    history: &[(&'static str, String)],
    digest: &str,
    attach_panes: bool,
    cancel: &std::sync::atomic::AtomicBool,
    notify: &dyn Fn(ChatEvent),
    acc: &mut String,
) -> Result<bool, NetErr> {
    // When the pane list rides along it goes on the live user turn, not the
    // system message: that leaves everything up to the previous question
    // cacheable, so the reusable prefix grows with the conversation instead
    // of staying pinned to the persona. Measured on qwen3.6-35b-a3b, 2-turn
    // conversation: ~20% -> ~38% KV reuse. On the turns it stays off — most
    // of them — the prefix is stable all the way to the question. Switching
    // faces mid-conversation costs one full re-prefill, same as any other
    // system-message change — rare enough not to matter.
    let mut messages =
        vec![serde_json::json!({"role": "system", "content": persona(face, cfg.act)})];
    let last = history.len().saturating_sub(1);
    for (i, (role, content)) in history.iter().enumerate() {
        let content = if attach_panes && i == last && *role == "user" {
            format!("{}\n\n{content}", panes_block(digest))
        } else {
            content.clone()
        };
        messages.push(serde_json::json!({"role": role, "content": content}));
    }

    // Queries already spent this turn — a model that searches the same words
    // twice is stuck, not thorough.
    let mut asked: Vec<String> = Vec::new();
    let mut consulted = false;
    // One round past the budget: that last pass answers every call with a
    // refusal instead of a result, which is the protocol's way of saying
    // "stop asking". The tools stay on the request throughout — withdrawing
    // them mid-conversation flips the chat template and Qwen starts typing
    // raw <tool_call> blocks as prose, and `tool_choice: none` does the same
    // (llama-server stops parsing calls but the model keeps making them).
    for round in 0..=MAX_TOOL_ROUNDS {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(NetErr::Cancelled);
        }
        let turn = stream_once(cfg, &messages, cancel, &mut |tok| {
            acc.push_str(tok);
            notify(ChatEvent::Token(tok.to_string()));
        })?;
        if turn.calls.is_empty() {
            return Ok(consulted);
        }
        let closed = round == MAX_TOOL_ROUNDS;
        messages.push(serde_json::json!({
            "role": "assistant",
            "content": turn.content,
            "tool_calls": turn.calls.iter().map(|c| serde_json::json!({
                "id": c.id,
                "type": "function",
                "function": {"name": c.name, "arguments": c.args},
            })).collect::<Vec<_>>(),
        }));
        for call in &turn.calls {
            let result = if closed {
                "no more tool calls this turn. Answer Des now, in words, \
                 with what you already have, and say plainly if it was not enough."
                    .to_string()
            } else {
                consulted |= call.name == "read_panes";
                run_tool(cfg, call, digest, &mut asked, notify)
            };
            messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": result,
            }));
        }
    }
    // The refusals leave the model with nothing to do but talk.
    stream_once(cfg, &messages, cancel, &mut |tok| {
        acc.push_str(tok);
        notify(ChatEvent::Token(tok.to_string()));
    })?;
    Ok(consulted)
}

/// The pane list, for the turns that get it inline.
fn panes_block(digest: &str) -> String {
    format!("The live pane list (no need to call read_panes):\n{digest}\n[end of list]")
}

/// Decides which turns carry the pane list. A question that names the
/// session gets it; so does the turn straight after one, because follow-ups
/// ("is it safe?", "and the other one?") carry none of the words that would
/// give them away. Momentum lasts exactly one turn — carrying it from
/// momentum would keep the list attached forever.
#[derive(Default)]
struct PanesPolicy {
    /// The previous turn looked at the panes, one way or another.
    recent: bool,
    /// This turn asked for them on its own words.
    wanted: bool,
}

impl PanesPolicy {
    fn attach(&mut self, text: &str, force: bool) -> bool {
        self.wanted = force || about_session(text);
        self.wanted || self.recent
    }

    /// `consulted`: the model reached for `read_panes` mid-answer, which
    /// counts as much as saying "card" out loud.
    fn settle(&mut self, consulted: bool) {
        self.recent = self.wanted || consulted;
    }
}

/// Words that make a question about the session rather than the world. Only
/// a first pass: a miss costs one `read_panes` round-trip, not an answer.
const SESSION_WORDS: &[&str] = &[
    "agent", "agents", "aider", "approval", "approve", "blocked", "busy",
    "card", "cards", "claude", "codex", "crashed", "done", "finished", "hung", "idle",
    "opencode", "pane", "panes", "prompt", "running", "session", "sessions", "status",
    "stuck", "tab", "tabs", "terminal", "terminals", "wedged", "working", "zodiac",
];

/// Does this question plausibly concern the panes? Wrong in the generous
/// direction on purpose: a false positive wastes some context, a false
/// negative only costs the model a tool call to fix.
fn about_session(text: &str) -> bool {
    let lower = text.to_lowercase();
    if ["need me", "going on", "how are they", "how they", "who needs", "anything up"]
        .iter()
        .any(|p| lower.contains(p))
    {
        return true;
    }
    lower
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| SESSION_WORDS.binary_search(&w).is_ok())
}

/// The tools the model may reach for. The search description follows the
/// configured backend, so he knows whether he has the whole web or only an
/// encyclopedia.
fn tools_json(cfg: &ChatCfg) -> serde_json::Value {
    let search_desc = if cfg.search_url.is_empty() {
        "Search Wikipedia and return article extracts. Encyclopedic facts only — it cannot \
         see news, prices or anything recent."
    } else {
        "Search the web and return titles, URLs and snippets. Use for current, niche or \
         uncertain facts."
    };
    let mut tools = vec![
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "read_panes",
                "description": "See the live pane list: every card in this zodiac session with \
                                its agent, status and uptime, plus screen excerpts from the panes \
                                worth reading. The only way to learn anything about the session.",
                "parameters": {"type": "object", "properties": {}},
            },
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": search_desc,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Search terms, plain words."}
                    },
                    "required": ["query"],
                },
            },
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "fetch_url",
                "description": "Fetch one http(s) page and return its readable text, truncated. \
                                Use to read a search result or a page Des names.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "Absolute http:// or https:// URL."}
                    },
                    "required": ["url"],
                },
            },
        }),
    ];
    if cfg.act {
        tools.push(serde_json::json!({
            "type": "function",
            "function": {
                "name": "prompt_pane",
                "description": "Submit a full text prompt to a card's agent, exactly as if Des \
                                typed it and pressed Enter. Always shows Des a consent chip first \
                                — it never happens without his approval, no matter how sure you \
                                are. Only reach for this when Des has asked you to act; never on \
                                your own initiative just because a card looks like it wants \
                                something.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pane": {"type": "integer", "description": "Card number, as shown in the list."},
                        "text": {"type": "string", "description": "The exact text to submit."}
                    },
                    "required": ["pane", "text"],
                },
            },
        }));
        tools.push(serde_json::json!({
            "type": "function",
            "function": {
                "name": "send_keys",
                "description": "Send raw keystrokes to a card — answer a y/n prompt, dismiss a \
                                dialog, interrupt with Esc. Space-separated tokens: literal \
                                characters, or names like Esc, Enter, Tab, Backspace, Ctrl+<letter>. \
                                Always shows Des a consent chip first — it never happens without \
                                his approval. Only reach for this when Des has asked you to act.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pane": {"type": "integer", "description": "Card number, as shown in the list."},
                        "keys": {"type": "string", "description": "Space-separated key tokens, e.g. \"y Enter\" or \"Esc\"."}
                    },
                    "required": ["pane", "keys"],
                },
            },
        }));
    }
    serde_json::Value::Array(tools)
}

/// Run one tool call and return the text the model gets back. Tool failures
/// are answers too — the model should say the search failed, not stall.
fn run_tool(
    cfg: &ChatCfg,
    call: &ToolCall,
    digest: &str,
    asked: &mut Vec<String>,
    notify: &dyn Fn(ChatEvent),
) -> String {
    let args: serde_json::Value = serde_json::from_str(&call.args).unwrap_or(serde_json::Value::Null);
    match call.name.as_str() {
        "read_panes" => {
            notify(ChatEvent::Note("reading the panes…".into()));
            format!("The live pane list:\n{digest}")
        }
        "web_search" => {
            let q = args["query"].as_str().unwrap_or("").trim().to_string();
            if q.is_empty() {
                return "error: empty query".into();
            }
            if asked.iter().any(|p| p.eq_ignore_ascii_case(&q)) {
                return "you already searched those words this turn — the search will not answer \
                        twice. Answer with what you have."
                    .into();
            }
            asked.push(q.clone());
            notify(ChatEvent::Note(format!("searching the web — “{q}”…")));
            match search_web(cfg, &q) {
                Ok(t) => t,
                Err(e) => format!("search failed: {e}"),
            }
        }
        "fetch_url" => {
            let u = args["url"].as_str().unwrap_or("").trim().to_string();
            notify(ChatEvent::Note(format!("reading {}…", short_url(&u))));
            match fetch_url(&u) {
                Ok(t) => t,
                Err(e) => format!("fetch failed: {e}"),
            }
        }
        "prompt_pane" if cfg.act => {
            let Some(pane) = args["pane"].as_u64().map(|n| n as usize) else {
                return "error: missing or invalid 'pane'".into();
            };
            let text = args["text"].as_str().unwrap_or("").to_string();
            if text.is_empty() {
                return "error: empty text".into();
            }
            request_cast(ActionRequest::PromptPane { pane, text }, notify)
        }
        "send_keys" if cfg.act => {
            let Some(pane) = args["pane"].as_u64().map(|n| n as usize) else {
                return "error: missing or invalid 'pane'".into();
            };
            let keys = args["keys"].as_str().unwrap_or("").to_string();
            if keys.is_empty() {
                return "error: empty keys".into();
            }
            request_cast(ActionRequest::SendKeys { pane, keys }, notify)
        }
        // Only reachable if a stray call names one of these while `act` is
        // off — they're not offered in `tools_json` then, but a leaked or
        // hallucinated call could still show up here.
        "prompt_pane" | "send_keys" => "error: acting on panes is turned off".into(),
        other => format!("error: no such tool '{other}'"),
    }
}

/// Show the consent chip and block until Des answers — advisory means
/// advisory: no timeout, no auto-accept. The client executes the action
/// (it owns the socket and the card-number → pane-id mapping) and reports
/// back what it did.
fn request_cast(action: ActionRequest, notify: &dyn Fn(ChatEvent)) -> String {
    let desc = action.describe();
    let (tx, rx) = std::sync::mpsc::channel::<ActionOutcome>();
    notify(ChatEvent::Cast { desc: desc.clone(), action, decision: tx });
    match rx.recv() {
        Ok(ActionOutcome::Sent) => format!("Des approved it, and it has been sent: {desc}"),
        Ok(ActionOutcome::Declined) => format!("Des declined: {desc}"),
        Ok(ActionOutcome::Failed(e)) => format!("Des approved it, but it failed: {e}"),
        Err(_) => "error: no answer came back".into(),
    }
}

/// One round of generation; calls `on_token` per content delta and returns
/// whatever the model produced, tool calls reassembled.
fn stream_once(
    cfg: &ChatCfg,
    messages: &[serde_json::Value],
    cancel: &std::sync::atomic::AtomicBool,
    on_token: &mut dyn FnMut(&str),
) -> Result<Turn, NetErr> {
    let mut body = serde_json::json!({
        "model": cfg.model,
        "messages": messages,
        "stream": true,
        "cache_prompt": true,
        "max_tokens": 700,
        // Qwen3.6 is a thinking model; a chatbox wants answers, not
        // minutes of reasoning_content. Harmless on non-Qwen templates.
        "chat_template_kwargs": {"enable_thinking": false},
    });
    body["tools"] = tools_json(cfg);
    let body = body.to_string();

    // SSE lines can straddle chunk boundaries; assemble before parsing.
    let mut pend: Vec<u8> = Vec::new();
    let mut turn = Turn::default();
    let mut prose = Prose::default();
    let code = http(
        cfg,
        "POST",
        "/v1/chat/completions",
        Some(&body),
        Duration::from_secs(180),
        cancel,
        |bytes| {
            pend.extend_from_slice(bytes);
            while let Some(pos) = pend.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = pend.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim().strip_prefix("data: ") else {
                    continue;
                };
                if data == "[DONE]" {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                    let delta = &v["choices"][0]["delta"];
                    if let Some(t) = delta["content"].as_str() {
                        if !t.is_empty() {
                            prose.feed(t, on_token);
                        }
                    }
                    // Tool calls arrive in fragments keyed by index: name and
                    // id once, arguments a character at a time.
                    if let Some(tcs) = delta["tool_calls"].as_array() {
                        for tc in tcs {
                            // Clamped: the index drives an allocation, and a
                            // buggy server emitting a huge one would abort.
                            // Real turns use single-digit call counts.
                            let i = tc["index"].as_u64().unwrap_or(0).min(31) as usize;
                            if turn.calls.len() <= i {
                                turn.calls.resize_with(i + 1, ToolCall::default);
                            }
                            let slot = &mut turn.calls[i];
                            if let Some(id) = tc["id"].as_str() {
                                slot.id = id.to_string();
                            }
                            if let Some(n) = tc["function"]["name"].as_str() {
                                slot.name.push_str(n);
                            }
                            if let Some(a) = tc["function"]["arguments"].as_str() {
                                slot.args.push_str(a);
                            }
                        }
                    }
                }
            }
        },
    )?;
    if code != 200 {
        return Err(NetErr::Bad(format!("HTTP {code} from the model")));
    }
    turn.calls.retain(|c| !c.name.is_empty());
    turn.content = prose.finish(on_token);
    // A call the server failed to parse still counts as a call.
    if turn.calls.is_empty() {
        turn.calls.extend(prose.recovered());
    }
    Ok(turn)
}

/// Markers that mean the model started typing a tool call as prose instead
/// of emitting it as one — Qwen's two usual spellings.
const LEAK_MARKERS: [&str; 2] = ["<tool_call>", "<function="];

/// The visible half of a streamed reply. It holds back any text that might
/// be the opening of a leaked tool call, so raw `<tool_call>` XML never
/// reaches the chat panel; whatever follows a marker is kept aside and
/// parsed back into a real call.
#[derive(Default)]
struct Prose {
    /// Clean text emitted so far.
    shown: String,
    /// Tail not yet emitted: it could still grow into a marker.
    held: String,
    /// Everything from the marker on, once one has appeared.
    leak: Option<String>,
}

impl Prose {
    fn feed(&mut self, tok: &str, on_token: &mut dyn FnMut(&str)) {
        if let Some(leak) = &mut self.leak {
            leak.push_str(tok);
            return;
        }
        self.held.push_str(tok);
        // A marker in hand: everything before it is prose, the rest is spill.
        if let Some((at, m)) = LEAK_MARKERS
            .iter()
            .filter_map(|m| self.held.find(m).map(|p| (p, *m)))
            .min_by_key(|(p, _)| *p)
        {
            let clean = self.held[..at].to_string();
            self.emit(&clean, on_token);
            self.leak = Some(self.held[at + m.len()..].to_string());
            self.held.clear();
            return;
        }
        // Otherwise emit all but the longest tail that could still become one.
        let hold = LEAK_MARKERS
            .iter()
            .flat_map(|m| (1..m.len()).map(move |n| &m[..n]))
            .filter(|p| self.held.ends_with(p))
            .map(|p| p.len())
            .max()
            .unwrap_or(0);
        let cut = self.held.len() - hold;
        let clean = self.held[..cut].to_string();
        self.emit(&clean, on_token);
        self.held.drain(..cut);
    }

    fn emit(&mut self, text: &str, on_token: &mut dyn FnMut(&str)) {
        if !text.is_empty() {
            self.shown.push_str(text);
            on_token(text);
        }
    }

    /// Flush the held tail (it never became a marker) and return the visible
    /// text.
    fn finish(&mut self, on_token: &mut dyn FnMut(&str)) -> String {
        if self.leak.is_none() {
            let tail = std::mem::take(&mut self.held);
            self.emit(&tail, on_token);
        }
        self.shown.clone()
    }

    /// The leaked block, read back as a tool call if it parses.
    fn recovered(&self) -> Option<ToolCall> {
        parse_leaked_call(self.leak.as_deref()?)
    }
}

/// Read a tool call out of text the model typed rather than emitted, in
/// either the JSON spelling (`{"name": …, "arguments": {…}}`) or the XML one
/// (`<function=name><parameter=key>value</parameter>`).
fn parse_leaked_call(spill: &str) -> Option<ToolCall> {
    let mk = |name: String, args: String| {
        (!name.is_empty()).then(|| ToolCall {
            id: format!("leak-{name}"),
            name,
            args,
        })
    };
    let body = spill.trim_start();
    // JSON: either sitting bare, or wrapped by a <tool_call> marker.
    if let Some(start) = body.find('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trim_to_json(&body[start..])) {
            if let Some(name) = v["name"].as_str() {
                let args = match &v["arguments"] {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                return mk(name.to_string(), args);
            }
        }
    }
    // XML: <function=web_search> then one block per parameter.
    let after_fn = body.strip_prefix("<function=").or_else(|| {
        body.find("<function=")
            .map(|p| &body[p + "<function=".len()..])
    })?;
    let (name, rest) = after_fn.split_once('>')?;
    let mut args = serde_json::Map::new();
    let mut cursor = rest;
    while let Some(p) = cursor.find("<parameter=") {
        let after = &cursor[p + "<parameter=".len()..];
        let Some((key, tail)) = after.split_once('>') else {
            break;
        };
        let end = tail.find("</parameter>").unwrap_or(tail.len());
        args.insert(
            key.trim().to_string(),
            serde_json::Value::String(tail[..end].trim().to_string()),
        );
        cursor = &tail[end.min(tail.len())..];
    }
    mk(
        name.trim().to_string(),
        serde_json::Value::Object(args).to_string(),
    )
}

/// The JSON object at the head of `s`, brace-matched, so trailing markers
/// like `</tool_call>` don't defeat the parser.
fn trim_to_json(s: &str) -> &str {
    let mut depth = 0usize;
    let mut in_str = false;
    let mut esc = false;
    for (i, c) in s.char_indices() {
        if in_str {
            match c {
                _ if esc => esc = false,
                '\\' => esc = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &s[..=i];
                }
            }
            _ => {}
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Looking outward: search and page fetching.
//
// These go out over curl rather than the little client above — the wider
// world speaks TLS, and shelling out keeps zodiac dependency-free. Both are
// blocking, which is fine: the worker thread is already waiting on the model
// when a tool fires.

/// Browsers get answers; unlabelled clients get captchas.
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
                  Chrome/126.0.0.0 Safari/537.36";

/// Cap on what any one tool hands back, so a fat page can't crowd the
/// conversation out of the context window.
const TOOL_BUDGET: usize = 6000;

fn curl(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("curl")
        .args(["-sS", "-L", "--max-time", "25", "-A", UA])
        .args(args)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "curl is not installed".to_string()
            } else {
                e.to_string()
            }
        })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.lines().last().unwrap_or("curl failed").trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn urlenc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Keep a string to `max` chars, cutting on a boundary.
fn clip(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn short_url(u: &str) -> String {
    u.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(u)
        .to_string()
}

/// Dispatch on whatever the settings point at: nothing = Wikipedia, Brave's
/// host = Brave, anything else = a SearXNG instance.
fn search_web(cfg: &ChatCfg, query: &str) -> Result<String, String> {
    if cfg.search_url.is_empty() {
        wiki_search(query)
    } else if cfg.search_url.contains("search.brave.com") {
        brave_search(cfg, query)
    } else {
        searx_search(cfg, query)
    }
}

/// Render `(title, url, snippet)` triples into the block the model reads.
fn render_results(hits: &[(String, String, String)]) -> String {
    if hits.is_empty() {
        return "no results".into();
    }
    let mut out = String::new();
    for (i, (title, url, snippet)) in hits.iter().enumerate() {
        let entry = format!(
            "[{}] {}\n{}\n{}\n\n",
            i + 1,
            title.trim(),
            url.trim(),
            clip(snippet.trim(), 700),
        );
        if out.len() + entry.len() > TOOL_BUDGET {
            break;
        }
        out.push_str(&entry);
    }
    out
}

/// SearXNG's JSON API. The instance must have `json` in its `search.formats`.
fn searx_search(cfg: &ChatCfg, query: &str) -> Result<String, String> {
    let url = format!(
        "{}/search?q={}&format=json&safesearch=0&language=en",
        cfg.search_url,
        urlenc(query)
    );
    let mut args = vec!["-H", "Accept: application/json", url.as_str()];
    let key_hdr;
    if !cfg.search_key.is_empty() {
        key_hdr = format!("Authorization: Bearer {}", cfg.search_key);
        args.splice(0..0, ["-H", key_hdr.as_str()]);
    }
    let raw = curl(&args)?;
    parse_searx(&raw, &short_url(&cfg.search_url))
}

fn parse_searx(raw: &str, host: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(raw).map_err(|_| {
        // Almost always an HTML error page: JSON not enabled, or a captcha.
        format!("{host} did not answer with JSON")
    })?;
    let mut out = String::new();
    // SearXNG's own direct answers (calculator, infoboxes) beat any snippet.
    for a in v["answers"].as_array().into_iter().flatten() {
        let text = a.as_str().unwrap_or_else(|| a["answer"].as_str().unwrap_or(""));
        if !text.is_empty() {
            out.push_str(&format!("answer: {}\n\n", clip(text, 500)));
        }
    }
    let hits: Vec<_> = v["results"]
        .as_array()
        .into_iter()
        .flatten()
        .take(6)
        .map(|r| {
            (
                r["title"].as_str().unwrap_or("").to_string(),
                r["url"].as_str().unwrap_or("").to_string(),
                r["content"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    out.push_str(&render_results(&hits));
    Ok(out)
}

fn brave_search(cfg: &ChatCfg, query: &str) -> Result<String, String> {
    if cfg.search_key.is_empty() {
        return Err("no chat_search_key set for Brave".into());
    }
    let url = format!(
        "https://api.search.brave.com/res/v1/web/search?q={}&count=6",
        urlenc(query)
    );
    let token = format!("X-Subscription-Token: {}", cfg.search_key);
    let raw = curl(&[
        "-H",
        "Accept: application/json",
        "-H",
        &token,
        url.as_str(),
    ])?;
    parse_brave(&raw)
}

fn parse_brave(raw: &str) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| "Brave did not answer with JSON".to_string())?;
    if let Some(msg) = v["error"]["detail"].as_str().or(v["message"].as_str()) {
        return Err(msg.to_string());
    }
    let hits: Vec<_> = v["web"]["results"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            (
                r["title"].as_str().unwrap_or("").to_string(),
                r["url"].as_str().unwrap_or("").to_string(),
                r["description"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    Ok(render_results(&hits))
}

/// The keyless fallback: Wikipedia's API, article intros for the best
/// matching pages. No key, no captcha, no news.
fn wiki_search(query: &str) -> Result<String, String> {
    let url = format!(
        "https://en.wikipedia.org/w/api.php?action=query&generator=search&gsrsearch={}\
         &gsrlimit=3&prop=extracts&exintro=1&explaintext=1&format=json&formatversion=2",
        urlenc(query)
    );
    let raw = curl(&["-H", "Accept: application/json", url.as_str()])?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| "Wikipedia did not answer with JSON".to_string())?;
    let hits: Vec<_> = v["query"]["pages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            let title = p["title"].as_str().unwrap_or("").to_string();
            (
                title.clone(),
                format!("https://en.wikipedia.org/wiki/{}", urlenc(&title)),
                p["extract"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    if hits.is_empty() {
        return Ok("no Wikipedia article matched. Wikipedia is the only search backend \
                   configured, so current events, prices and releases are out of reach — say so \
                   rather than searching again."
            .into());
    }
    Ok(format!(
        "(Wikipedia only — no news, prices or current releases. If these extracts do not answer \
         it, say so instead of searching again.)\n\n{}",
        render_results(&hits)
    ))
}

/// Fetch a page and hand back its readable text.
fn fetch_url(url: &str) -> Result<String, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("only http:// and https:// URLs can be fetched".into());
    }
    let raw = curl(&["--max-filesize", "4000000", url])?;
    let text = if raw.trim_start().starts_with('{') || raw.trim_start().starts_with('[') {
        raw
    } else {
        strip_html(&raw)
    };
    let text = text.trim();
    if text.is_empty() {
        return Err("the page came back empty".into());
    }
    Ok(clip(text, TOOL_BUDGET).to_string())
}

/// Tags out, entities in, whitespace collapsed. Not a browser — just enough
/// to let the model read prose off a page.
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let tail = &rest[lt..];
        let lower = clip(tail, 16).to_ascii_lowercase();
        // Script and style bodies are noise; skip the whole element. (An
        // ASCII lowercasing keeps byte offsets, so the hit indexes `tail`.)
        let skipped = ["script", "style", "head", "svg"]
            .iter()
            .find(|t| lower.starts_with(&format!("<{t}")))
            .and_then(|t| tail.to_ascii_lowercase().find(&format!("</{t}")));
        if let Some(p) = skipped {
            rest = &tail[p..];
            continue;
        }
        // Block-level tags are where the line breaks belong.
        if ["<p", "<br", "<div", "<li", "<tr", "<h1", "<h2", "<h3", "</p", "</div"]
            .iter()
            .any(|t| lower.starts_with(t))
        {
            out.push('\n');
        }
        match tail.find('>') {
            Some(p) => rest = &tail[p + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    let out = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    // Collapse the acres of whitespace HTML leaves behind, but keep
    // paragraph breaks — they carry the structure.
    let mut clean = String::with_capacity(out.len());
    let mut blank = 0;
    for line in out.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        clean.push_str(&line);
        clean.push('\n');
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trips through the real state dir under a unique session name
    /// so it can't collide with an actual zodiac session — exercises the
    /// exact path production code takes rather than a mocked one.
    #[test]
    fn history_round_trips_through_disk() {
        let session = "chat-persist-roundtrip-test";
        let _ = std::fs::remove_file(history_path(session));
        assert!(load_history(session).is_empty(), "started with leftover state");

        let history: Vec<(&'static str, String)> =
            vec![("user", "hello".into()), ("assistant", "hi there".into())];
        save_history(session, &history);
        let loaded = load_history(session);
        assert_eq!(loaded, history);

        let _ = std::fs::remove_dir_all(history_path(session).parent().unwrap());
    }

    #[test]
    fn missing_history_file_loads_empty_not_error() {
        let session = "wizard-persist-missing-test";
        let _ = std::fs::remove_dir_all(history_path(session).parent().unwrap());
        assert_eq!(load_history(session), Vec::new());
    }

    #[test]
    fn corrupt_history_file_loads_empty_not_panic() {
        let session = "wizard-persist-corrupt-test";
        let path = history_path(session);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(load_history(session), Vec::new());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn persona_denies_hands_when_act_is_off() {
        for face in ["assistant", "oracle", "hal"] {
            let p = persona(face, false);
            assert!(
                p.contains("no hands") || p.contains("cannot act"),
                "{face} persona should still deny acting when act=false: {p}"
            );
            assert!(!p.contains("prompt_pane"), "{face} persona leaked act-on text: {p}");
        }
    }

    #[test]
    fn persona_grants_hands_when_act_is_on() {
        // Also guards against a typo in the `disabled` substring silently
        // no-opping the replace and leaving the "I have no hands" claim in
        // place even though the tools are now offered.
        for face in ["assistant", "oracle", "hal"] {
            let p = persona(face, true);
            assert!(p.contains("prompt_pane"), "{face} persona didn't grant hands: {p}");
            assert!(
                !p.contains("cannot act") && !p.contains("no hands"),
                "{face} persona still denies acting even with act=true: {p}"
            );
        }
    }

    #[test]
    fn write_tools_only_offered_when_act_is_on() {
        let mut cfg = ChatCfg::from_settings(&Settings::load());
        cfg.act = false;
        let names: Vec<String> = tools_json(&cfg)
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect();
        assert!(!names.contains(&"prompt_pane".to_string()));
        assert!(!names.contains(&"send_keys".to_string()));

        cfg.act = true;
        let names: Vec<String> = tools_json(&cfg)
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"prompt_pane".to_string()));
        assert!(names.contains(&"send_keys".to_string()));
    }

    #[test]
    fn cast_action_describe_matches_the_plan_format() {
        let a = ActionRequest::SendKeys { pane: 3, keys: "y".into() };
        assert_eq!(a.describe(), "send_keys(3, \"y\")");
        let a = ActionRequest::PromptPane { pane: 2, text: "yes please".into() };
        assert_eq!(a.describe(), "prompt_pane(2, \"yes please\")");
    }

    #[test]
    fn write_tool_call_refused_when_act_is_off() {
        let mut cfg = ChatCfg::from_settings(&Settings::load());
        cfg.act = false;
        let call = ToolCall {
            id: "1".into(),
            name: "send_keys".into(),
            args: r#"{"pane":1,"keys":"y"}"#.into(),
        };
        let mut asked = Vec::new();
        let result = run_tool(&cfg, &call, "session 'x': 0 cards", &mut asked, &|_| {});
        assert!(result.contains("turned off"), "{result}");
    }

    #[test]
    fn strips_tags_scripts_and_entities() {
        let html = "<html><head><title>x</title></head><body>\
                    <script>var a = 1 < 2;</script>\
                    <p>Caf\u{e9} &amp; cr\u{e8}me</p><p>second</p>\
                    <style>p{color:red}</style></body></html>";
        let text = strip_html(html);
        assert!(text.contains("Caf\u{e9} & cr\u{e8}me"), "{text:?}");
        assert!(text.contains("second"));
        assert!(!text.contains("var a"), "script body survived: {text:?}");
        assert!(!text.contains("color:red"), "style body survived: {text:?}");
    }

    #[test]
    fn strip_html_keeps_utf8_intact() {
        // A byte-wise walk would turn these into mojibake.
        let text = strip_html("<p>\u{6f22}\u{5b57} \u{2014} \u{1f52e}</p>");
        assert!(text.contains("\u{6f22}\u{5b57}") && text.contains('\u{1f52e}'), "{text:?}");
    }

    #[test]
    fn strip_html_survives_an_unclosed_tag() {
        assert!(strip_html("before <div class=\"x").contains("before"));
    }

    #[test]
    fn urlenc_escapes_the_awkward_bytes() {
        assert_eq!(urlenc("rust lang?q=a&b"), "rust+lang%3Fq%3Da%26b");
        assert_eq!(urlenc("caf\u{e9}"), "caf%C3%A9");
    }

    #[test]
    fn clip_cuts_on_char_boundaries() {
        // 3 bytes into "\u{6f22}\u{5b57}" is mid-character.
        assert_eq!(clip("\u{6f22}\u{5b57}", 4), "\u{6f22}");
        assert_eq!(clip("short", 99), "short");
    }

    #[test]
    fn results_render_within_budget() {
        let hits: Vec<_> = (0..40)
            .map(|i| (format!("t{i}"), format!("https://e/{i}"), "x".repeat(900)))
            .collect();
        let out = render_results(&hits);
        assert!(out.len() <= TOOL_BUDGET, "{} bytes", out.len());
        assert!(out.starts_with("[1] t0"));
        assert_eq!(render_results(&[]), "no results");
    }

    /// Push text through the filter one character at a time — the worst case
    /// for a marker straddling token boundaries.
    fn run_prose(text: &str) -> (String, Option<ToolCall>) {
        let mut p = Prose::default();
        let mut seen = String::new();
        for c in text.chars() {
            let mut buf = [0u8; 4];
            p.feed(c.encode_utf8(&mut buf), &mut |t| seen.push_str(t));
        }
        let shown = p.finish(&mut |t| seen.push_str(t));
        assert_eq!(seen, shown, "streamed text and final text disagree");
        (shown, p.recovered())
    }

    #[test]
    fn plain_prose_passes_through_whole() {
        let (shown, call) = run_prose("The sky is blue — Rayleigh scattering. 3 < 4, x<y.");
        assert_eq!(shown, "The sky is blue — Rayleigh scattering. 3 < 4, x<y.");
        assert!(call.is_none());
    }

    #[test]
    fn leaked_xml_call_is_hidden_and_recovered() {
        let (shown, call) = run_prose(
            "Let me look.<tool_call>\n<function=web_search>\n<parameter=query>\n\
             rust latest release\n</parameter>\n</function>\n</tool_call>",
        );
        assert_eq!(shown, "Let me look.");
        let call = call.expect("no call recovered");
        assert_eq!(call.name, "web_search");
        let args: serde_json::Value = serde_json::from_str(&call.args).unwrap();
        assert_eq!(args["query"], "rust latest release");
    }

    #[test]
    fn leaked_json_call_is_hidden_and_recovered() {
        let (shown, call) = run_prose(
            "<tool_call>{\"name\": \"fetch_url\", \"arguments\": {\"url\": \"https://a.b/c\"}}\
             </tool_call>",
        );
        assert!(shown.is_empty(), "{shown:?}");
        let call = call.expect("no call recovered");
        assert_eq!(call.name, "fetch_url");
        let args: serde_json::Value = serde_json::from_str(&call.args).unwrap();
        assert_eq!(args["url"], "https://a.b/c");
    }

    #[test]
    fn a_dangling_marker_prefix_still_gets_shown() {
        // "<tool" never completes — it is ordinary text and must not vanish.
        let (shown, call) = run_prose("beware the <tool");
        assert_eq!(shown, "beware the <tool");
        assert!(call.is_none());
    }

    #[test]
    fn trim_to_json_stops_at_the_matching_brace() {
        let s = "{\"a\": {\"b\": \"}\"}} trailing</tool_call>";
        assert_eq!(trim_to_json(s), "{\"a\": {\"b\": \"}\"}}");
    }

    #[test]
    fn parses_a_searx_payload() {
        let raw = r#"{"query":"q","answers":["42"],"results":[
            {"title":"First","url":"https://a.example/1","content":"snippet one"},
            {"title":"Second","url":"https://b.example/2","content":"snippet two"}],
            "infoboxes":[]}"#;
        let out = parse_searx(raw, "searx.local").unwrap();
        assert!(out.starts_with("answer: 42"), "{out}");
        assert!(out.contains("[1] First") && out.contains("https://a.example/1"));
        assert!(out.contains("[2] Second") && out.contains("snippet two"));
    }

    #[test]
    fn searx_answers_may_be_objects() {
        let raw = r#"{"answers":[{"answer":"an object answer"}],"results":[]}"#;
        let out = parse_searx(raw, "searx.local").unwrap();
        assert!(out.contains("an object answer"), "{out}");
    }

    #[test]
    fn a_captcha_page_reads_as_a_clear_failure() {
        let err = parse_searx("<!doctype html><html>nope", "searx.local").unwrap_err();
        assert!(err.contains("searx.local") && err.contains("JSON"), "{err}");
    }

    #[test]
    fn parses_a_brave_payload() {
        let raw = r#"{"web":{"results":[
            {"title":"Rust","url":"https://rust-lang.org","description":"a language"}]}}"#;
        let out = parse_brave(raw).unwrap();
        assert!(out.contains("[1] Rust") && out.contains("a language"), "{out}");
    }

    #[test]
    fn brave_rejection_surfaces_its_own_words() {
        let raw = r#"{"error":{"detail":"Subscription token invalid"}}"#;
        assert_eq!(
            parse_brave(raw).unwrap_err(),
            "Subscription token invalid"
        );
    }

    #[test]
    fn session_words_stay_sorted_for_the_binary_search() {
        let mut sorted = SESSION_WORDS.to_vec();
        sorted.sort_unstable();
        assert_eq!(SESSION_WORDS, sorted.as_slice());
    }

    #[test]
    fn session_questions_get_the_pane_list() {
        for q in [
            "anything need me right now?",
            "What is card 3 doing, and what should I do about it?",
            "is the api agent stuck?",
            "which panes are still working",
            "what's going on in the session",
            "Is claude done yet?",
            "any tabs waiting on approval?",
        ] {
            assert!(about_session(q), "missed: {q}");
        }
    }

    #[test]
    fn worldly_questions_do_not() {
        for q in [
            "why is the sky blue?",
            "how long do I braise short ribs?",
            "what is the newest Rust release?",
            "explain a monad in two sentences",
            "who won the 1998 world cup",
            "translate 'good evening' into Welsh",
        ] {
            assert!(!about_session(q), "false positive: {q}");
        }
    }

    #[test]
    fn word_matching_does_not_fire_on_fragments() {
        // "discard" ends in "card", "agentic" starts with "agent".
        assert!(!about_session("should I discard the marinade?"));
        assert!(!about_session("what does agentic mean"));
        // …but real words still count, whatever the punctuation.
        assert!(about_session("(card 2?)"));
    }

    /// One turn through the policy: attach, then settle.
    fn turn(p: &mut PanesPolicy, text: &str, consulted: bool) -> bool {
        let attach = p.attach(text, false);
        p.settle(consulted);
        attach
    }

    #[test]
    fn momentum_carries_one_follow_up_then_lapses() {
        let mut p = PanesPolicy::default();
        assert!(!turn(&mut p, "why is the sky blue?", false));
        assert!(turn(&mut p, "anything need me?", false), "named the session");
        assert!(turn(&mut p, "is that safe?", false), "follow-up rides along");
        assert!(!turn(&mut p, "how do I braise short ribs?", false), "momentum lapsed");
    }

    #[test]
    fn reaching_for_the_spread_renews_momentum() {
        let mut p = PanesPolicy::default();
        // No session words, but the model went and looked anyway.
        assert!(!turn(&mut p, "should I be worried?", true));
        assert!(turn(&mut p, "what about the other one?", false));
    }

    #[test]
    fn forcing_beats_the_heuristic() {
        let mut p = PanesPolicy::default();
        assert!(p.attach("what is it doing?", true));
    }

    #[test]
    fn inline_spread_tells_the_model_not_to_call_the_tool() {
        let b = panes_block("card 1: 'x'");
        assert!(b.contains("no need to call read_panes"));
        assert!(b.contains("card 1: 'x'"));
    }

    /// Live check against the configured endpoint — needs bigbox awake, so
    /// it stays out of the normal run: `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_wizard_answers_and_searches() {
        let cfg = ChatCfg::from_settings(&Settings::load());
        let digest = "session 'lab': 2 cards\n\
                      card 1: 'api' — claude 1.2 — needs_input — up 4m — ~/src/api\n\
                      card 2: 'docs' — shell — idle — up 1h\n\
                      \n--- card 1 'api' — screen ---\n\
                      Bash(rm -rf ./build)\n  Do you want to proceed? (y/n)\n";
        for (label, q) in [
            ("offtopic", "In one sentence: why is the sky blue?"),
            ("recipe", "How long do I braise short ribs, and at what temperature?"),
            ("cards", "Anything need me right now?"),
            ("search", "What is the newest Rust release? Search if you must."),
            ("obscure", "Look up the population of Vaduz, Liechtenstein."),
            ("fetch", "Read https://example.com and tell me its first sentence."),
            // Carries none of the session words — he has to reach for
            // read_panes himself to answer it.
            ("implicit", "Should I be worried about anything?"),
        ] {
            let mut acc = String::new();
            let history = vec![("user", q.to_string())];
            let attach = about_session(q);
            println!("[{label}] spread attached: {attach}");
            let no_cancel = std::sync::atomic::AtomicBool::new(false);
            let r = chat_stream(&cfg, "wizard", &history, digest, attach, &no_cancel, &|ev| {
                if let ChatEvent::Note(n) = ev {
                    println!("  note: {n}");
                }
            }, &mut acc);
            assert!(r.is_ok(), "{label}: request failed");
            println!("{label}: {acc}\n");
            assert!(!acc.trim().is_empty(), "{label}: empty answer");
            assert!(!acc.contains("<tool_call>"), "{label}: leaked tool syntax");
        }
    }

    /// With `act` on and Des asking for it outright, the model should
    /// reach for `send_keys` rather than just describing what to type —
    /// and the consent round-trip (Cast event out, outcome back in) has
    /// to actually unblock the tool call and let the answer complete.
    #[test]
    #[ignore]
    fn live_send_keys_asks_for_consent_and_completes() {
        let mut cfg = ChatCfg::from_settings(&Settings::load());
        cfg.act = true;
        let digest = "session 'lab': 1 cards\n\
                      card 1: 'api' — claude 1.2 — needs_input — up 4m — ~/src/api\n\
                      \n--- card 1 'api' — screen ---\n\
                      Bash(rm -rf ./build)\n  Do you want to proceed? (y/n)\n";
        let q = "Card 1 is waiting on that y/n prompt and I want you to answer it yourself: \
                 send 'y' to card 1 with send_keys. I'm explicitly asking you to act, right now.";
        let mut acc = String::new();
        let history = vec![("user", q.to_string())];
        let no_cancel = std::sync::atomic::AtomicBool::new(false);
        let cast_seen: std::cell::RefCell<Option<(String, ActionRequest)>> =
            std::cell::RefCell::new(None);
        let out = chat_stream(
            &cfg,
            "wizard",
            &history,
            digest,
            true,
            &no_cancel,
            &|ev| match ev {
                ChatEvent::Note(n) => println!("  note: {n}"),
                ChatEvent::Cast { desc, action, decision } => {
                    println!("  cast requested: {desc}");
                    *cast_seen.borrow_mut() = Some((desc, action));
                    // Simulate Des pressing 'y' — the client would execute
                    // it and report back; here we just report success.
                    let _ = decision.send(ActionOutcome::Sent);
                }
                _ => {}
            },
            &mut acc,
        );
        assert!(out.is_ok(), "request failed");
        println!("answer: {acc}\n");
        let (desc, action) = cast_seen.into_inner().expect("model never called a write-tool");
        match action {
            ActionRequest::SendKeys { pane, keys } => {
                assert_eq!(pane, 1, "wrong card: {desc}");
                assert!(keys.contains('y'), "keys didn't include y: {desc}");
            }
            ActionRequest::PromptPane { .. } => panic!("expected send_keys, got prompt_pane: {desc}"),
        }
        assert!(!acc.trim().is_empty(), "no final answer after the cast resolved");
    }

    /// A question greedy enough to spend every tool round must still come
    /// back with words — the refusal round is what guarantees it.
    #[test]
    #[ignore]
    fn live_exhausting_the_tool_budget_still_answers() {
        let cfg = ChatCfg::from_settings(&Settings::load());
        let q = "Look up each of these separately, one search at a time, before you answer: \
                 the population of Vaduz, the height of Mont Blanc, the year Liechtenstein was \
                 founded, the currency of Bhutan, and the depth of Lake Baikal.";
        let mut acc = String::new();
        let history = vec![("user", q.to_string())];
        let no_cancel = std::sync::atomic::AtomicBool::new(false);
        let out = chat_stream(
            &cfg,
            "wizard",
            &history,
            "session 'x': 0 cards",
            false,
            &no_cancel,
            &|ev| {
                if let ChatEvent::Note(n) = ev {
                    println!("  note: {n}");
                }
            },
            &mut acc,
        );
        assert!(out.is_ok(), "request failed");
        println!("answer: {acc}\n");
        assert!(!acc.trim().is_empty(), "spent the budget and said nothing");
        assert!(!acc.contains("<tool_call>"), "leaked tool syntax");
        assert!(!acc.contains("<function="), "leaked tool syntax");
    }

    /// Esc mid-stream (`ChatCmd::Chat`'s cancel flag) must actually cut
    /// the network read short, not just stop rendering tokens client-side —
    /// proves the flag reaches all the way into `http`'s chunk loop.
    #[test]
    #[ignore]
    fn live_cancel_cuts_the_stream_short() {
        let cfg = ChatCfg::from_settings(&Settings::load());
        let q = "Write a very long, detailed essay — at least 500 words — about the history \
                 of clockmaking.";
        let history = vec![("user", q.to_string())];
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flip = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(800));
            flip.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let mut acc = String::new();
        let start = Instant::now();
        let out = chat_stream(
            &cfg,
            "wizard",
            &history,
            "session 'x': 0 cards",
            false,
            &cancel,
            &|_| {},
            &mut acc,
        );
        let elapsed = start.elapsed();
        assert!(matches!(out, Err(NetErr::Cancelled)), "expected Cancelled, got Ok/other Err");
        assert!(!acc.is_empty(), "cancel fired before any tokens arrived — flakier timing needed");
        assert!(elapsed < Duration::from_secs(10), "took {elapsed:?} — cancel did not cut in");
        println!("cancelled after {elapsed:?}, {} chars captured", acc.len());
    }

    /// The other live path: a follow-up that names nothing must still know
    /// which card was being discussed. Same run conditions as above.
    #[test]
    #[ignore]
    fn live_follow_up_keeps_the_thread() {
        let cfg = ChatCfg::from_settings(&Settings::load());
        let digest = "session 'lab': 2 cards\n\
                      card 1: 'api' — claude 1.2 — needs_input — up 4m — ~/src/api\n\
                      card 2: 'docs' — shell — idle — up 1h\n\
                      \n--- card 1 'api' — screen ---\n\
                      Bash(rm -rf ./build)\n  Do you want to proceed? (y/n)\n";
        let mut policy = PanesPolicy::default();
        let mut history: Vec<(&'static str, String)> = Vec::new();
        let mut attached = Vec::new();
        for q in ["anything need me right now?", "is that safe?"] {
            let attach = policy.attach(q, false);
            attached.push(attach);
            println!("[{q}] spread attached: {attach}");
            history.push(("user", q.to_string()));
            let mut acc = String::new();
            let no_cancel = std::sync::atomic::AtomicBool::new(false);
            let out = chat_stream(&cfg, "wizard", &history, digest, attach, &no_cancel, &|ev| {
                if let ChatEvent::Note(n) = ev {
                    println!("  note: {n}");
                }
            }, &mut acc);
            policy.settle(*out.as_ref().unwrap_or(&false));
            assert!(out.is_ok(), "request failed");
            println!("  {acc}\n");
            history.push(("assistant", acc));
        }
        // "is that safe?" names nothing; momentum is what puts the cards in
        // front of it.
        assert_eq!(attached, vec![true, true], "the thread was dropped");
    }
}
