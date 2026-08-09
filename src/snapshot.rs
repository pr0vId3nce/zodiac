//! The session snapshot: a picture of what was running where, rewritten
//! once a minute so it can be raised again after a reboot.
//!
//! zodiac's own `state.json` restores panes, directories and scrollback.
//! This file exists for what that can't carry — the agent inside each pane
//! and, for claude, the id of the conversation on its screen. The server
//! writes it (`server::Server::save_snapshot`), the client previews it for
//! the Alt+⇧R overlay, and `scripts/zodiac-restore.sh` reads the same JSON
//! from outside.

use serde::{Deserialize, Serialize};

use crate::protocol::state_dir;

#[derive(Serialize, Deserialize, Default)]
pub struct Snapshot {
    #[serde(default)]
    pub session: String,
    /// Seconds since the epoch — how stale this picture is.
    #[serde(default)]
    pub saved_at: u64,
    /// 1-based index of the focused pane.
    #[serde(default)]
    pub active: usize,
    #[serde(default)]
    pub panes: Vec<SnapPane>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct SnapPane {
    /// 1-based, matching `zodiac ls` and every pane-taking CLI command.
    pub index: usize,
    pub name: String,
    /// "pty" (default; also every pre-agent snapshot) or "agent" — an
    /// agent pane restores by structured relaunch, never by typing a
    /// shell line (roadmap 2.8).
    #[serde(default)]
    pub kind: Option<String>,
    pub cwd: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    /// Claude Code session id — `claude --resume <chat_id>`. Only ever set
    /// for claude panes; other agents expose nothing we can read.
    pub chat_id: Option<String>,
    #[serde(default)]
    pub auto_resume: bool,
    #[serde(default)]
    pub renamed: bool,
}

impl Snapshot {
    pub fn has_agents(&self) -> bool {
        self.panes.iter().any(|p| p.agent.is_some())
    }
}

fn read(path: &std::path::Path) -> Option<Snapshot> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Write `snapshot.json`, whole file then rename, so a reader never catches
/// it half-written.
pub fn write(session: &str, snap: &Snapshot) {
    let dir = state_dir(session);
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_vec_pretty(snap) {
        let tmp = dir.join("snapshot.json.tmp");
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join("snapshot.json"));
        }
    }
}

/// Called once at server startup: the snapshot on disk describes the
/// session that just ended, and the fresh one is about to overwrite it.
/// Only a snapshot with agents in it is worth keeping — otherwise two
/// restarts in a row would push the last useful picture out of `prev`.
pub fn rotate(session: &str) {
    let dir = state_dir(session);
    let cur = dir.join("snapshot.json");
    if read(&cur).is_some_and(|s| s.has_agents()) {
        let _ = std::fs::rename(&cur, dir.join("snapshot.prev.json"));
    }
}

/// The snapshot worth restoring from: the pre-restart one if it holds
/// agents, else the live one (the session is still up and nothing was
/// rotated). None when neither has an agent to raise.
pub fn best(session: &str) -> Option<Snapshot> {
    let dir = state_dir(session);
    [dir.join("snapshot.prev.json"), dir.join("snapshot.json")]
        .into_iter()
        .filter_map(|p| read(&p))
        .find(|s| s.has_agents())
}

impl SnapPane {
    /// The shell line that puts this pane's agent back: `cd` into its old
    /// directory (skipped when the pane is already there) and start the
    /// agent — claude and pi on the very conversation they had open. None
    /// for panes that were sitting at a shell prompt.
    pub fn restore_command(&self, current_cwd: Option<&str>) -> Option<String> {
        // Structured panes never restore via a typed shell line.
        if self.kind.as_deref() == Some("agent") {
            return None;
        }
        let agent = self.agent.as_deref()?;
        let launch = match (agent, self.chat_id.as_deref()) {
            ("claude", Some(chat)) => format!("claude --resume {chat}"),
            ("pi", Some(chat)) => format!("pi --session {chat}"),
            _ => agent.to_string(),
        };
        match self.cwd.as_deref() {
            Some(cwd) if Some(cwd) != current_cwd => {
                Some(format!("cd {} && {launch}", shell_quote(cwd)))
            }
            _ => Some(launch),
        }
    }
}

/// Single-quote a path for the shell, the POSIX way: close the quote,
/// escape the quote, reopen.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(agent: Option<&str>, chat: Option<&str>, cwd: Option<&str>) -> SnapPane {
        SnapPane {
            index: 1,
            name: "p".into(),
            kind: None,
            cwd: cwd.map(str::to_string),
            agent: agent.map(str::to_string),
            model: None,
            chat_id: chat.map(str::to_string),
            auto_resume: true,
            renamed: false,
        }
    }

    #[test]
    fn claude_resumes_its_conversation() {
        let p = pane(Some("claude"), Some("abc-123"), Some("/src"));
        assert_eq!(
            p.restore_command(None).unwrap(),
            "cd '/src' && claude --resume abc-123"
        );
    }

    #[test]
    fn no_cd_when_the_pane_is_already_there() {
        let p = pane(Some("claude"), Some("abc"), Some("/src"));
        assert_eq!(
            p.restore_command(Some("/src")).unwrap(),
            "claude --resume abc"
        );
    }

    #[test]
    fn pi_resumes_its_session() {
        let p = pane(
            Some("pi"),
            Some("019fe295-3523-7d14-8293-6db6edbacb02"),
            Some("/src"),
        );
        assert_eq!(
            p.restore_command(None).unwrap(),
            "cd '/src' && pi --session 019fe295-3523-7d14-8293-6db6edbacb02"
        );
    }

    #[test]
    fn an_agent_without_a_session_still_just_launches() {
        assert_eq!(
            pane(Some("pi"), None, None).restore_command(None).unwrap(),
            "pi"
        );
    }

    #[test]
    fn other_agents_launch_by_name() {
        let p = pane(Some("opencode"), None, None);
        assert_eq!(p.restore_command(None).unwrap(), "opencode");
    }

    #[test]
    fn agent_panes_never_type_shell_lines() {
        let mut p = pane(Some("claude"), Some("abc"), Some("/src"));
        p.kind = Some("agent".into());
        assert!(p.restore_command(None).is_none());
    }

    #[test]
    fn a_shell_pane_has_nothing_to_restore() {
        assert!(pane(None, None, Some("/src"))
            .restore_command(None)
            .is_none());
    }

    #[test]
    fn quoting_survives_spaces_and_quotes() {
        let p = pane(Some("claude"), None, Some("/a dir/o'brien"));
        assert_eq!(
            p.restore_command(None).unwrap(),
            "cd '/a dir/o'\\''brien' && claude"
        );
    }
}
