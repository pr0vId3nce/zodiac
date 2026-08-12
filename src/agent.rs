//! Agent panes (roadmap Phase 2, ADR 0002): a structured agent process on
//! pipes — no PTY, no VT engine. The server relays the agent's native
//! NDJSON lines to clients (`T_AGENT_EVENT`) and parses only what it must:
//! session ids (resume), working/idle status, permission requests, and
//! failures (structured retry).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::Instant;

use anyhow::Result;

use crate::protocol::PermRequest;
use crate::server::SrvEvent;

/// Transcript ring bound (bytes of NDJSON kept per pane for attach replay).
const RING_BYTES: usize = 2 * 1024 * 1024;
/// Retry ceiling for structured resume-after-crash (2.7).
pub const RETRY_MAX: u32 = 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentKind {
    Claude,
    Pi,
}

impl AgentKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Self::Claude),
            "pi" => Some(Self::Pi),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Pi => "pi",
        }
    }
}

/// Everything the server tracks for one agent pane beyond the generic
/// `SrvPane` bookkeeping. Owned by `SrvPane` (`kind == "agent"`).
pub struct AgentRuntime {
    pub kind: AgentKind,
    pub cwd: Option<PathBuf>,
    /// The model to run, as the harness expects it (a claude `--model` alias
    /// like "opus", or a pi `provider/id` like "llama-local/qwen3.6-35b-a3b").
    /// `None` = the harness default.
    pub model: Option<String>,
    /// Captured from the agent's init/session event; used for `--resume` /
    /// `--session-id` relaunches (structured retry + snapshot restore).
    pub session_id: Option<String>,
    /// Bounded native-NDJSON transcript ring (2.3), replayed on attach.
    events: VecDeque<Vec<u8>>,
    events_bytes: usize,
    /// Pending permission requests (2.5). Server-side inbox is
    /// authoritative: entries persist until answered or timed out.
    pub pending: Vec<(PermRequest, Instant)>,
    pub working: bool,
    /// Human-readable failure from the last error result, if any.
    pub failed: Option<String>,
    /// Structured retry (2.7): count, next attempt time, and the last user
    /// input to re-send after a resume-relaunch.
    pub retries: u32,
    pub retry_at: Option<Instant>,
    pub last_input: Option<String>,
    /// Reader threads tag events with the generation that spawned them so
    /// lines from a replaced process can't corrupt the current one.
    pub generation: u64,
    stdin: Option<ChildStdin>,
    child: Option<Child>,
    pub pid: Option<u32>,
}

impl AgentRuntime {
    pub fn new(kind: AgentKind, cwd: Option<PathBuf>, model: Option<String>) -> Self {
        Self {
            kind,
            cwd,
            model,
            session_id: None,
            events: VecDeque::new(),
            events_bytes: 0,
            pending: Vec::new(),
            working: false,
            failed: None,
            retries: 0,
            retry_at: None,
            last_input: None,
            generation: 0,
            stdin: None,
            child: None,
            pid: None,
        }
    }

    pub fn alive(&mut self) -> bool {
        match self.child.as_mut() {
            Some(c) => c.try_wait().ok().flatten().is_none(),
            None => false,
        }
    }

    /// Spawn (or respawn, resuming the captured session) the agent process
    /// and its reader threads. Stdout lines arrive as
    /// `SrvEvent::AgentLine(pane, gen, line)`, stderr lines as
    /// `AgentStderr`, stream end as `AgentGone`.
    pub fn spawn_process(&mut self, pane: u64, tx: &Sender<SrvEvent>) -> Result<()> {
        self.generation += 1;
        let gen = self.generation;
        let mut cmd = match self.kind {
            AgentKind::Claude => {
                let mut c = Command::new("claude");
                c.args([
                    "-p",
                    "--input-format",
                    "stream-json",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--include-partial-messages",
                    "--permission-prompt-tool",
                    "stdio",
                ]);
                if let Some(sid) = &self.session_id {
                    c.args(["--resume", sid]);
                }
                if let Some(m) = &self.model {
                    c.args(["--model", m]);
                }
                c
            }
            AgentKind::Pi => {
                let mut c = Command::new("pi");
                c.args(["--mode", "rpc"]);
                if let Some(sid) = &self.session_id {
                    c.args(["--session-id", sid]);
                }
                if let Some(m) = &self.model {
                    c.args(["--model", m]);
                }
                c
            }
        };
        // Same env scrub as pty panes: a server started from inside an
        // agent session must not leak that session's markers into panes.
        for k in [
            "CLAUDECODE",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SSE_PORT",
            "CLAUDE_CODE_ENTRYPOINT",
            "PI_CODING_AGENT",
            "PI_SESSION_ID",
            "PI_SESSION_FILE",
            "PI_PROVIDER",
            "PI_MODEL",
            "PI_REASONING_LEVEL",
        ] {
            cmd.env_remove(k);
        }
        if let Some(dir) = self.cwd.clone().filter(|d| d.is_dir()) {
            cmd.current_dir(dir);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        self.pid = Some(child.id());
        self.stdin = child.stdin.take();

        let stdout = child.stdout.take().expect("stdout piped");
        let txo = tx.clone();
        std::thread::spawn(move || {
            let mut rd = BufReader::new(stdout);
            let mut line = Vec::new();
            loop {
                line.clear();
                match rd.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        while line.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
                            line.pop();
                        }
                        if !line.is_empty()
                            && txo
                                .send(SrvEvent::AgentLine(pane, gen, line.clone()))
                                .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            let _ = txo.send(SrvEvent::AgentGone(pane, gen));
        });

        let stderr = child.stderr.take().expect("stderr piped");
        let txe = tx.clone();
        std::thread::spawn(move || {
            for l in BufReader::new(stderr).lines().map_while(|l| l.ok()) {
                if txe.send(SrvEvent::AgentStderr(pane, gen, l)).is_err() {
                    break;
                }
            }
        });

        self.child = Some(child);
        Ok(())
    }

    fn write_line(&mut self, line: &str) -> bool {
        let Some(stdin) = self.stdin.as_mut() else {
            return false;
        };
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())
            .is_ok()
    }

    /// Send one user prompt in the agent's native shape. Returns false when
    /// the process is gone (caller respawns and retries).
    pub fn send_user_text(&mut self, text: &str) -> bool {
        let line = match self.kind {
            AgentKind::Claude => serde_json::json!({
                "type": "user",
                "message": {"role": "user",
                            "content": [{"type": "text", "text": text}]},
            })
            .to_string(),
            AgentKind::Pi => serde_json::json!({
                "type": "prompt",
                "message": text,
            })
            .to_string(),
        };
        self.write_line(&line)
    }

    /// Switch the session's permission mode at runtime (claude control
    /// protocol). Verified against the CLI: it answers `control_response`
    /// success and then reports `permissionMode` on a `system` status event,
    /// which is what clients render. Pi has no equivalent, so this is a no-op
    /// there rather than a lie.
    pub fn send_perm_mode(&mut self, mode: &str) -> bool {
        if self.kind != AgentKind::Claude {
            return false;
        }
        let line = serde_json::json!({
            "type": "control_request",
            "request_id": format!("zodiac-mode-{}", self.generation),
            "request": {"subtype": "set_permission_mode", "mode": mode},
        })
        .to_string();
        self.write_line(&line)
    }

    /// Answer a pending permission request (claude control protocol).
    pub fn send_perm_response(
        &mut self,
        request_id: &str,
        allow: bool,
        message: Option<&str>,
    ) -> bool {
        let response = if allow {
            let input = self
                .pending
                .iter()
                .find(|(p, _)| p.request_id == request_id)
                .map(|(p, _)| p.input.clone())
                .unwrap_or(serde_json::Value::Null);
            serde_json::json!({"behavior": "allow", "updatedInput": input})
        } else {
            serde_json::json!({"behavior": "deny",
                               "message": message.unwrap_or("denied from zodiac")})
        };
        let line = serde_json::json!({
            "type": "control_response",
            "response": {"request_id": request_id, "subtype": "success",
                         "response": response},
        })
        .to_string();
        self.write_line(&line)
    }

    /// Append one NDJSON line to the bounded transcript ring (2.3).
    pub fn ring_push(&mut self, line: &[u8]) {
        self.events_bytes += line.len();
        self.events.push_back(line.to_vec());
        while self.events_bytes > RING_BYTES {
            match self.events.pop_front() {
                Some(old) => self.events_bytes -= old.len(),
                None => break,
            }
        }
    }

    /// The whole ring joined with newlines — the attach-replay payload
    /// (clients split on '\n').
    pub fn ring_joined(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.events_bytes + self.events.len());
        for l in &self.events {
            out.extend_from_slice(l);
            out.push(b'\n');
        }
        out
    }

    /// Iterate the transcript ring's raw NDJSON lines (oldest first).
    pub fn ring_lines(&self) -> impl Iterator<Item = &Vec<u8>> {
        self.events.iter()
    }

    pub fn kill(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.stdin = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_is_bounded() {
        let mut rt = AgentRuntime::new(AgentKind::Claude, None, None);
        let line = vec![b'x'; 1024];
        for _ in 0..(3 * RING_BYTES / 1024) {
            rt.ring_push(&line);
        }
        assert!(rt.events_bytes <= RING_BYTES);
        assert!(rt.ring_lines().next().is_some());
        let joined = rt.ring_joined();
        assert!(joined.len() <= RING_BYTES + rt.events.len());
    }

    #[test]
    fn user_text_shapes() {
        // No process: send fails but the shapes are exercised via the
        // builder (the JSON forms are pinned by ADR 0002 probes).
        let mut rt = AgentRuntime::new(AgentKind::Claude, None, None);
        assert!(!rt.send_user_text("hi"));
        let mut rt = AgentRuntime::new(AgentKind::Pi, None, None);
        assert!(!rt.send_user_text("hi"));
    }
}
