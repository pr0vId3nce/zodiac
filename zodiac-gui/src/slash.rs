//! Slash-command discovery for structured Claude Code panes.
//!
//! Claude Code resolves a leading `/name` in a user message even in
//! `--input-format stream-json` mode (verified against the CLI: `/cost` sent as
//! a user message comes back with real output), so a structured pane can run
//! them — it just had no way to *show* which ones exist. This enumerates them
//! the way the CLI does: built-ins, plus user/project command files, plus
//! skills, each of which is invocable as `/name`.

/// One invocable slash command.
#[derive(Clone, Debug, PartialEq)]
pub struct SlashCmd {
    /// Without the leading slash.
    pub name: String,
    pub desc: String,
    /// Where it came from, shown as a dim tag ("built-in", "skill", "project").
    pub origin: &'static str,
}

/// Built-in commands that behave sensibly in a non-interactive stream-json
/// session. Deliberately excludes TUI-only affordances (`/vim`, `/terminal-setup`)
/// and anything whose whole job is to redraw an interactive screen.
///
/// `/resume` is listed because zodiac handles it itself — see
/// [`is_zodiac_handled`] — rather than passing it to the CLI, which can't run
/// an interactive session picker over a pipe.
const BUILTINS: &[(&str, &str)] = &[
    ("resume", "Pick an earlier session to resume in this pane"),
    ("clear", "Clear the conversation history"),
    ("compact", "Compact the conversation to free context"),
    ("context", "Show what's using the context window"),
    ("cost", "Show token/usage cost for this session"),
    ("status", "Show session, model and account status"),
    ("model", "Show or set the model for this session"),
    ("doctor", "Check the health of the installation"),
    ("init", "Create a CLAUDE.md with codebase docs"),
    ("memory", "Edit memory files"),
    ("review", "Review a pull request"),
    ("security-review", "Security review of the pending changes"),
    ("agents", "Manage agents"),
    ("mcp", "Manage MCP servers"),
    ("help", "List available commands"),
];

/// Commands zodiac answers itself instead of forwarding to the harness,
/// because they need multiplexer-side action (spawning a pane). Kept as the
/// single source of truth for the interception in `composer_bar`.
pub fn is_zodiac_handled(name: &str) -> bool {
    matches!(name, "resume")
}

/// Every command available to a pane of `harness` rooted at `cwd`.
///
/// Only claude has an enumerable command set: its built-ins are known, and
/// user/project command files and skills are on disk. Pi's built-ins live
/// inside its binary with nothing on disk to read, so rather than ship a
/// guessed list that might not work, pi gets only what is genuinely
/// discoverable — today, nothing, so no picker appears.
pub fn commands_for(harness: &str, cwd: Option<&std::path::Path>) -> Vec<SlashCmd> {
    match harness {
        "claude" => commands(cwd),
        _ => Vec::new(),
    }
}

/// Every command available to a claude pane rooted at `cwd`, sorted by name.
/// Discovery touches the filesystem, so results are cached per cwd.
pub fn commands(cwd: Option<&std::path::Path>) -> Vec<SlashCmd> {
    use std::sync::Mutex;
    static CACHE: Mutex<Option<(String, Vec<SlashCmd>)>> = Mutex::new(None);
    let key = cwd.map(|p| p.display().to_string()).unwrap_or_default();
    if let Ok(g) = CACHE.lock() {
        if let Some((k, v)) = g.as_ref() {
            if *k == key {
                return v.clone();
            }
        }
    }
    let found = discover(cwd);
    if let Ok(mut g) = CACHE.lock() {
        *g = Some((key, found.clone()));
    }
    found
}

fn discover(cwd: Option<&std::path::Path>) -> Vec<SlashCmd> {
    let mut out: Vec<SlashCmd> = BUILTINS
        .iter()
        .map(|(n, d)| SlashCmd {
            name: (*n).to_string(),
            desc: (*d).to_string(),
            origin: "built-in",
        })
        .collect();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    if let Some(h) = &home {
        collect_commands(&h.join(".claude/commands"), "user", &mut out);
        collect_skills(&h.join(".claude/skills"), "skill", &mut out);
    }
    if let Some(c) = cwd {
        collect_commands(&c.join(".claude/commands"), "project", &mut out);
        collect_skills(&c.join(".claude/skills"), "project skill", &mut out);
    }
    // Deduplicate by name, keeping the first (built-ins and closer scopes win).
    let mut seen = std::collections::HashSet::new();
    out.retain(|c| seen.insert(c.name.clone()));
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `<dir>/**/*.md` → `/name`, nested directories joining as `dir:name` (the
/// CLI's namespacing).
fn collect_commands(dir: &std::path::Path, origin: &'static str, out: &mut Vec<SlashCmd>) {
    fn walk(
        dir: &std::path::Path,
        prefix: &str,
        origin: &'static str,
        depth: u8,
        out: &mut Vec<SlashCmd>,
    ) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let path = e.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if path.is_dir() && depth < 3 {
                let next = if prefix.is_empty() {
                    stem.to_string()
                } else {
                    format!("{prefix}:{stem}")
                };
                walk(&path, &next, origin, depth + 1, out);
            } else if path.extension().is_some_and(|x| x == "md") {
                let name = if prefix.is_empty() {
                    stem.to_string()
                } else {
                    format!("{prefix}:{stem}")
                };
                out.push(SlashCmd {
                    desc: front_matter_desc(&path).unwrap_or_default(),
                    name,
                    origin,
                });
            }
        }
    }
    walk(dir, "", origin, 0, out);
}

/// `<dir>/<name>/SKILL.md` → `/name`.
fn collect_skills(dir: &std::path::Path, origin: &'static str, out: &mut Vec<SlashCmd>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        let skill = path.join("SKILL.md");
        if !skill.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        out.push(SlashCmd {
            name: name.to_string(),
            desc: front_matter_desc(&skill).unwrap_or_default(),
            origin,
        });
    }
}

/// First-line-ish `description:` from a markdown file's YAML front matter.
/// Reads only the head of the file — these are documents, not config.
fn front_matter_desc(path: &std::path::Path) -> Option<String> {
    let text = read_head(path, 4096)?;
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        let t = line.trim();
        if t == "---" {
            break;
        }
        if let Some(v) = t.strip_prefix("description:") {
            let v = v.trim().trim_matches(['"', '\'']).trim();
            if !v.is_empty() {
                return Some(first_sentence(v));
            }
        }
    }
    None
}

fn first_sentence(s: &str) -> String {
    let cut = s.find(". ").map(|i| i + 1).unwrap_or(s.len());
    let s = &s[..cut];
    if s.chars().count() > 90 {
        let t: String = s.chars().take(89).collect();
        format!("{t}…")
    } else {
        s.to_string()
    }
}

fn read_head(path: &std::path::Path, max: usize) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; max];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// A past Claude Code session that can be resumed.
#[derive(Clone, Debug)]
pub struct SessionEntry {
    /// The session UUID, passed to the CLI as `--resume <id>`.
    pub id: String,
    /// First user message, for recognising the session.
    pub summary: String,
    /// "3m", "2h", "4d" — how long since it was last written.
    pub age: String,
}

/// Past Claude Code sessions for `cwd`, newest first.
///
/// The CLI keeps them at `~/.claude/projects/<cwd-with-slashes-as-dashes>/
/// <uuid>.jsonl`. `/resume` can't run in a piped session ("isn't available in
/// this environment"), so zodiac enumerates them itself and resumes by
/// spawning the harness with `--resume <id>`.
pub fn sessions(cwd: &std::path::Path) -> Vec<SessionEntry> {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return Vec::new();
    };
    let slug = cwd.display().to_string().replace('/', "-");
    let dir = home.join(".claude/projects").join(slug);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<(std::time::SystemTime, SessionEntry)> = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "jsonl") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let modified = e
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        out.push((
            modified,
            SessionEntry {
                id: id.to_string(),
                summary: first_user_message(&path).unwrap_or_else(|| "(no prompt)".into()),
                age: age_str(modified),
            },
        ));
    }
    out.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    out.into_iter().map(|(_, s)| s).take(30).collect()
}

/// The first user prompt in a session transcript, clipped for display.
fn first_user_message(path: &std::path::Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path).ok()?;
    for line in BufReader::new(f).lines().map_while(Result::ok).take(400) {
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }
        let content = v.get("message").and_then(|m| m.get("content"))?;
        let text = match content {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(a) => a
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(" "),
            _ => continue,
        };
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            continue;
        }
        return Some(if text.chars().count() > 80 {
            let t: String = text.chars().take(79).collect();
            format!("{t}…")
        } else {
            text
        });
    }
    None
}

fn age_str(t: std::time::SystemTime) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(t)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match secs {
        s if s < 90 => "just now".to_string(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86400),
    }
}

/// The in-progress `/command` token being typed, if the composer holds one:
/// text starts with `/` and no whitespace has been typed yet (once there are
/// arguments the picker gets out of the way).
pub fn active_query(composer: &str) -> Option<&str> {
    let rest = composer.strip_prefix('/')?;
    if rest.chars().any(char::is_whitespace) {
        return None;
    }
    Some(rest)
}

/// Commands matching `query`, prefix matches first, then substring.
pub fn matching(cmds: &[SlashCmd], query: &str) -> Vec<SlashCmd> {
    let q = query.to_ascii_lowercase();
    let mut pre: Vec<SlashCmd> = Vec::new();
    let mut sub: Vec<SlashCmd> = Vec::new();
    for c in cmds {
        let n = c.name.to_ascii_lowercase();
        if n.starts_with(&q) {
            pre.push(c.clone());
        } else if !q.is_empty() && n.contains(&q) {
            sub.push(c.clone());
        }
    }
    pre.extend(sub);
    pre
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_only_while_typing_the_command_token() {
        assert_eq!(active_query("/res"), Some("res"));
        assert_eq!(active_query("/"), Some(""));
        // Arguments started — the picker should get out of the way.
        assert_eq!(active_query("/review this"), None);
        // Not a command at all.
        assert_eq!(active_query("hello"), None);
        assert_eq!(active_query(""), None);
    }

    #[test]
    fn matching_prefers_prefix_then_substring() {
        let cmds = vec![
            SlashCmd {
                name: "cost".into(),
                desc: String::new(),
                origin: "b",
            },
            SlashCmd {
                name: "compact".into(),
                desc: String::new(),
                origin: "b",
            },
            SlashCmd {
                name: "security-review".into(),
                desc: String::new(),
                origin: "b",
            },
        ];
        let names: Vec<String> = matching(&cmds, "co").into_iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["cost", "compact"]);
        // Substring match still found, but after prefix matches.
        let names: Vec<String> = matching(&cmds, "review")
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, vec!["security-review"]);
        // Empty query lists everything.
        assert_eq!(matching(&cmds, "").len(), 3);
    }

    #[test]
    fn resume_is_handled_by_zodiac_not_the_cli() {
        assert!(is_zodiac_handled("resume"));
        assert!(!is_zodiac_handled("cost"));
        // The interception in composer_bar routes on exactly this.
        assert!(!is_zodiac_handled("compact"));
    }
}
