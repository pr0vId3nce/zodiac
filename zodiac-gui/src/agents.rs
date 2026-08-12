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

/// Pi's selectable models. Sourced from `pi --list-models` (cached), which is
/// authoritative: it lists every provider pi actually has — including ones
/// registered by an extension at runtime, like the `claude-bridge` provider
/// (`claude-bridge/claude-opus-4-8`, …) that `models.json` never mentions.
/// Falls back to reading `models.json` directly if the CLI can't be run, then
/// to a single "Default". Each choice's value is a `provider/id` string, which
/// pi's `--model` accepts.
fn pi_models() -> Vec<ModelChoice> {
    // Ensure a background fetch is in flight, then use it if it has landed.
    prewarm_pi_models();
    if let Some(models) = PI_MODELS_CACHE.lock().ok().and_then(|g| g.clone()) {
        if !models.is_empty() {
            return models;
        }
    }
    // Not ready yet (or pi absent): fall back to models.json without blocking.
    // Once the background fetch lands, a later open shows the full list.
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

/// Background cache of `pi --list-models`, filled by [`prewarm_pi_models`].
static PI_MODELS_CACHE: std::sync::Mutex<Option<Vec<ModelChoice>>> = std::sync::Mutex::new(None);
static PI_WARM_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Kick off `pi --list-models` in a background thread (at most once) and cache
/// the parsed result. Non-blocking and safe to call from the UI thread — call
/// it at startup so the new-agent picker has the full list (including the
/// runtime-registered `claude-bridge` models) ready by the time it opens,
/// without ever freezing the UI while pi (a Node program) starts up.
pub fn prewarm_pi_models() {
    use std::sync::atomic::Ordering;
    if PI_WARM_STARTED.swap(true, Ordering::SeqCst) {
        return; // already started this process
    }
    let Some(pi) = search_dirs()
        .iter()
        .map(|d| d.join("pi"))
        .find(|p| p.is_file())
    else {
        return;
    };
    // A GUI launched from the Dock has only launchd's bare PATH, and pi needs
    // Node — hand it the fuller PATH detection built.
    let path = std::env::join_paths(search_dirs()).unwrap_or_default();
    std::thread::spawn(move || {
        let out = std::process::Command::new(&pi)
            .arg("--list-models")
            .env("PATH", &path)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output();
        let models = match out {
            Ok(o) if o.status.success() => {
                parse_pi_list_models(&String::from_utf8_lossy(&o.stdout))
            }
            _ => Vec::new(),
        };
        if !models.is_empty() {
            if let Ok(mut guard) = PI_MODELS_CACHE.lock() {
                *guard = Some(models);
            }
        }
    });
}

/// Parse the `pi --list-models` table. Columns are whitespace-separated with
/// `provider` and `model` first; the header row (`provider model …`) and blank
/// lines are skipped. Display names from `models.json` are used when present,
/// else the bare model id.
fn parse_pi_list_models(text: &str) -> Vec<ModelChoice> {
    let names = pi_model_names();
    let mut out = Vec::new();
    for line in text.lines() {
        let mut cols = line.split_whitespace();
        let (Some(provider), Some(id)) = (cols.next(), cols.next()) else {
            continue;
        };
        if provider == "provider" {
            continue; // header
        }
        let value = format!("{provider}/{id}");
        let label = names.get(&value).cloned().unwrap_or_else(|| id.to_string());
        out.push(ModelChoice {
            label,
            value: Some(value),
        });
    }
    out
}

/// Map of `provider/id` → display name from `models.json`, to prettify labels
/// for models it knows (bridge-provided models aren't in it, so they keep the
/// bare id).
fn pi_model_names() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for c in read_pi_models().unwrap_or_default() {
        if let Some(v) = c.value {
            map.insert(v, c.label);
        }
    }
    map
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
    fn parses_pi_list_models_table_including_bridge() {
        // Real `pi --list-models` output shape: header + provider/model columns.
        let table = "provider       model              context  max-out  thinking  images\n\
                     claude-bridge  claude-opus-4-8    1M       128K     yes       yes\n\
                     llama-local    qwen3.6-35b-a3b    32.8K    8.2K     yes       no\n";
        let choices = super::parse_pi_list_models(table);
        let vals: Vec<Option<&str>> = choices.iter().map(|c| c.value.as_deref()).collect();
        assert_eq!(
            vals,
            vec![
                Some("claude-bridge/claude-opus-4-8"),
                Some("llama-local/qwen3.6-35b-a3b"),
            ]
        );
        // The header row must not become a phantom model.
        assert!(choices.iter().all(|c| c.label != "model"));
    }

    #[test]
    fn search_covers_real_install_dirs_under_a_bare_path() {
        // SAFETY: single-threaded test, set before the OnceLock is filled.
        unsafe { std::env::set_var("PATH", "/usr/bin:/bin:/usr/sbin:/sbin") };
        let dirs = super::search_dirs();
        let has = |p: &str| dirs.iter().any(|d| d.ends_with(p));
        assert!(has(".local/bin"), "~/.local/bin missing: {dirs:?}");
        assert!(
            dirs.iter()
                .any(|d| d == std::path::Path::new("/usr/local/bin")),
            "/usr/local/bin missing: {dirs:?}"
        );
        assert!(
            dirs.iter()
                .any(|d| d == std::path::Path::new("/opt/homebrew/bin")),
            "/opt/homebrew/bin missing: {dirs:?}"
        );
    }
}
