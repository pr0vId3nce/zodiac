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

/// Is `bin` an executable on `$PATH`?
fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file()))
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
