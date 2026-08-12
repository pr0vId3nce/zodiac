//! Local detection of spawnable agent harnesses + their models for the
//! new-agent picker. Only harnesses zodiac can actually spawn as structured
//! panes (claude, pi), and only what is set up locally: a harness appears
//! when its binary is on `$PATH`; pi's models come from its local
//! `models.json`, claude's from the CLI's `--model` aliases.

use crate::ui::{HarnessInfo, ModelChoice};

/// The harnesses installed locally, each with its selectable models.
pub fn harnesses() -> Vec<HarnessInfo> {
    let mut out = Vec::new();
    if which("claude") {
        out.push(HarnessInfo {
            name: "claude".into(),
            label: "Claude".into(),
            models: claude_models(),
        });
    }
    if which("pi") {
        out.push(HarnessInfo {
            name: "pi".into(),
            label: "Pi".into(),
            models: pi_models(),
        });
    }
    out
}

/// Is `bin` an executable anywhere we know to look?
fn which(bin: &str) -> bool {
    search_dirs().iter().any(|dir| dir.join(bin).is_file())
}

/// Where to look for harness binaries.
///
/// `$PATH` alone is not enough for a GUI. A window launched from Finder,
/// the Dock or Spotlight inherits launchd's environment, which on macOS is
/// a bare `/usr/bin:/bin:/usr/sbin:/sbin` — none of the places a user
/// actually installs a coding agent. The harness then looks uninstalled
/// even though it plainly is not, and the picker says "no agent harnesses
/// found" on a machine running one.
///
/// (An `LSEnvironment` PATH in the app bundle is *not* a fix: Launch
/// Services caches it and ignores it often enough to be untrustworthy.
/// This has to be answered by the program, not its Info.plist.)
///
/// Note the panes themselves were never affected — the server spawns each
/// one as a login shell, so it rebuilds the real PATH. It was only ever
/// detection that was blind.
fn search_dirs() -> &'static [std::path::PathBuf] {
    use std::sync::OnceLock;
    static DIRS: OnceLock<Vec<std::path::PathBuf>> = OnceLock::new();
    DIRS.get_or_init(|| {
        let mut out: Vec<std::path::PathBuf> = Vec::new();
        let mut push = |d: std::path::PathBuf| {
            if !out.contains(&d) {
                out.push(d);
            }
        };
        if let Some(paths) = std::env::var_os("PATH") {
            for d in std::env::split_paths(&paths) {
                push(d);
            }
        }
        for d in login_shell_path() {
            push(d);
        }
        // Common install prefixes, so detection still works when the login
        // shell can't be asked (or sets PATH only for interactive use).
        if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
            for rel in [
                ".local/bin",
                ".local/share/claude/bin",
                ".bun/bin",
                ".deno/bin",
                ".cargo/bin",
                ".npm-global/bin",
                ".yarn/bin",
                ".volta/bin",
            ] {
                push(home.join(rel));
            }
        }
        for abs in ["/usr/local/bin", "/opt/homebrew/bin", "/opt/local/bin"] {
            push(std::path::PathBuf::from(abs));
        }
        out
    })
}

/// The PATH the user's login shell builds — the one their terminal has,
/// and the one the pane will get when the server spawns its login shell.
/// Best-effort: a shell that fails, hangs past its own startup, or prints
/// nothing simply contributes no directories.
fn login_shell_path() -> Vec<std::path::PathBuf> {
    let Some(shell) = std::env::var_os("SHELL") else {
        return Vec::new();
    };
    let out = std::process::Command::new(shell)
        .args(["-lc", "printf %s \"$PATH\""])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(out) = out else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    std::env::split_paths(text.trim()).collect()
}

/// Claude's `--model` aliases (cloud models — not local, but this is how you
/// choose which to run). "Default" passes no `--model`.
fn claude_models() -> Vec<ModelChoice> {
    [
        ("Default (account default)", None),
        ("Opus", Some("opus")),
        ("Sonnet", Some("sonnet")),
        ("Haiku", Some("haiku")),
        ("Fable", Some("fable")),
    ]
    .into_iter()
    .map(|(label, v)| ModelChoice {
        label: label.into(),
        value: v.map(String::from),
    })
    .collect()
}

/// Pi's locally-configured models, read from `~/.pi/agent/models.json`
/// (`PI_CODING_AGENT_DIR` overrides the dir). Each becomes a `provider/id`
/// value, which pi's `--model` accepts. Falls back to a single "Default".
fn pi_models() -> Vec<ModelChoice> {
    let models = read_pi_models().unwrap_or_default();
    if models.is_empty() {
        vec![ModelChoice {
            label: "Default".into(),
            value: None,
        }]
    } else {
        models
    }
}

fn read_pi_models() -> Option<Vec<ModelChoice>> {
    let dir = std::env::var_os("PI_CODING_AGENT_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".pi/agent"))
        })?;
    let text = std::fs::read_to_string(dir.join("models.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let providers = v.get("providers")?.as_object()?;
    let mut out = Vec::new();
    for (prov, pv) in providers {
        let Some(models) = pv.get("models").and_then(|m| m.as_array()) else {
            continue;
        };
        for m in models {
            let Some(id) = m.get("id").and_then(|x| x.as_str()) else {
                continue;
            };
            let name = m.get("name").and_then(|x| x.as_str()).unwrap_or(id);
            out.push(ModelChoice {
                label: name.to_string(),
                value: Some(format!("{prov}/{id}")),
            });
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    /// A GUI launched from Finder/Dock/Spotlight inherits launchd's bare
    /// PATH. Detection must not depend on it, or every harness looks
    /// uninstalled on a machine that has one. Asserts on the *directory
    /// list* rather than on a real binary so it means the same thing on a
    /// CI runner with no agent installed.
    #[test]
    fn search_covers_real_install_dirs_under_a_bare_path() {
        // SAFETY: single-threaded test, set before the OnceLock is filled.
        unsafe { std::env::set_var("PATH", "/usr/bin:/bin:/usr/sbin:/sbin") };
        let dirs = super::search_dirs();
        let has = |p: &str| dirs.iter().any(|d| d.ends_with(p));
        assert!(has(".local/bin"), "~/.local/bin missing: {dirs:?}");
        assert!(
            dirs.iter().any(|d| d == std::path::Path::new("/usr/local/bin")),
            "/usr/local/bin missing: {dirs:?}"
        );
        assert!(
            dirs.iter().any(|d| d == std::path::Path::new("/opt/homebrew/bin")),
            "/opt/homebrew/bin missing: {dirs:?}"
        );
    }
}
