//! UI/behavior preferences, persisted to ~/.config/zodiac/config.json.
//! Shared by all sessions: the client's settings page writes it, and the
//! server re-reads it on each watchdog tick, so toggles apply live without
//! any protocol traffic.

fn default_true() -> bool {
    true
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub spinner: String,
    #[serde(default)]
    pub spinner_color: String,
    #[serde(default)]
    pub shimmer_color: String,
    #[serde(default)]
    pub shimmer_speed: String,
    #[serde(default)]
    pub eye: String,
    /// Sidebar border: "separator" (right line only), "surround", or
    /// "rounded" (surround with rounded corners).
    #[serde(default)]
    pub sidebar_frame: String,
    /// Border line weight: "normal", "thick", or "double".
    #[serde(default)]
    pub sidebar_weight: String,
    #[serde(default)]
    pub sidebar_color: String,
    /// Home page layout: "cards" (tarot grid) or "list" (stacked blocks).
    #[serde(default)]
    pub home_view: String,
    /// Home-page card dimensions: small/medium/large/huge.
    #[serde(default)]
    pub card_size: String,
    /// Cards per row on the home page: "auto" (fill the width) or "1".."6".
    #[serde(default)]
    pub card_columns: String,
    /// Color of the line separators in the blocks home view.
    #[serde(default)]
    pub home_sep_color: String,
    /// Chat panel persona: "wizard", "oracle" (ascii orb), or "hal".
    #[serde(default)]
    pub chat_face: String,
    /// Size of the emblem painted on home-page cards: small/medium/large/huge.
    #[serde(default)]
    pub card_icon: String,
    /// Card frame style on home-page cards: double/single/none.
    #[serde(default)]
    pub card_outline: String,
    /// Color of the selected-card outline.
    #[serde(default)]
    pub select_color: String,
    /// Thickness of the selected-card outline: thin/normal/thick/heavy.
    #[serde(default)]
    pub select_weight: String,
    /// Selection ring look on painted cards: "glow" (rounded + halo) or
    /// "ring" (hard square).
    #[serde(default)]
    pub select_style: String,
    /// Card numbering: "roman" (default), "arabic", or "zodiac".
    #[serde(default)]
    pub card_numeral: String,
    /// Mascot body shape: "hard" (boxy, default) or "soft" (rounded).
    #[serde(default)]
    pub claude_style: String,
    /// Watchdog for Claude Code's "API Error: Connection closed
    /// mid-response" line — fires `--resume` immediately on sight.
    #[serde(default = "default_true")]
    pub connection_watch: bool,
    /// Cursor shape in panes: "auto" (follow the inner app's DECSCUSR),
    /// "block", "underline", or "bar".
    #[serde(default)]
    pub cursor_style: String,
    /// Cursor blink in panes: "auto" (follow the inner app), "on", "off".
    #[serde(default)]
    pub cursor_blink: String,
    /// Cursor tint while a pane is focused: a color name or "off".
    #[serde(default)]
    pub cursor_color: String,
    /// Hide the keybinding hints in the bottom status bar (they stay
    /// visible in the settings page's Controls column).
    #[serde(default)]
    pub hide_controls: bool,
    /// Ringtone played when an agent finishes (working → done). A file name
    /// inside the ringtones dir, "off" to disable, or "" for the default
    /// (first ringtone alphabetically).
    #[serde(default)]
    pub finish_sound: String,
    /// The Wizard chat panel on the home page (talks to a llama-server
    /// endpoint over the tailnet).
    #[serde(default = "default_true")]
    pub wizard_chat: bool,
    /// OpenAI-compatible endpoint, e.g. "http://bigbox:8091" (the default).
    #[serde(default)]
    pub wizard_endpoint: String,
    /// Model name sent in completion requests ("" = qwen3.6-35b-a3b).
    #[serde(default)]
    pub wizard_model: String,
    /// ssh destination used to wake/dispell the model ("" = des@bigbox).
    #[serde(default)]
    pub wizard_ssh: String,
    /// systemd --user unit started/stopped over ssh ("" = llama-server).
    #[serde(default)]
    pub wizard_service: String,
    /// Where the wizard's `web_search` tool looks. Empty = Wikipedia only
    /// (keyless, works anywhere). A SearXNG base URL, e.g.
    /// "http://bigbox:8888", uses its JSON API. "https://api.search.brave.com"
    /// uses Brave's API and needs `wizard_search_key`.
    #[serde(default)]
    pub wizard_search_url: String,
    /// API key for the search endpoint, when it wants one (Brave).
    #[serde(default)]
    pub wizard_search_key: String,
    /// Chat panel width in cells ("" = 40).
    #[serde(default)]
    pub wizard_width: String,
    /// Headless background sentinel: classifies stuck/needs-input panes
    /// against a local policy file and notifies with the reason. Advisory
    /// only — it never touches a pane. Runs server-side against a separate
    /// CPU-only model, independent of the wizard chat panel above.
    #[serde(default = "default_true")]
    pub wizard_watch: bool,
}

/// Audio files with these extensions in the ringtones dir are offered as
/// finish sounds.
const AUDIO_EXTS: &[&str] = &[
    "mp3", "m4a", "m4r", "aac", "wav", "ogg", "oga", "opus", "flac", "aif", "aiff", "caf",
];

fn config_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .unwrap_or_default()
        .join("zodiac")
}

/// Where finish ringtones live; drop audio files here to offer them in the
/// settings page.
pub fn ringtones_dir() -> std::path::PathBuf {
    config_dir().join("ringtones")
}

/// Audio files in the ringtones dir, sorted by name. Recurses into
/// subdirectories (a few levels), so dropping in a whole folder of
/// ringtones works — entries are paths relative to the ringtones dir.
pub fn list_ringtones() -> Vec<String> {
    fn walk(dir: &std::path::Path, base: &std::path::Path, depth: u8, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth < 3 {
                    walk(&p, base, depth + 1, out);
                }
            } else if p
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| AUDIO_EXTS.contains(&x.to_ascii_lowercase().as_str()))
            {
                if let Ok(rel) = p.strip_prefix(base) {
                    out.push(rel.to_string_lossy().into_owned());
                }
            }
        }
    }
    let base = ringtones_dir();
    let mut names = Vec::new();
    walk(&base, &base, 0, &mut names);
    names.sort();
    names
}

impl Default for Settings {
    fn default() -> Self {
        serde_json::from_str("{}").expect("all fields have serde defaults")
    }
}

impl Settings {
    pub fn path() -> std::path::PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
            .unwrap_or_default()
            .join("zodiac/config.json")
    }

    pub fn load() -> Self {
        std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// The finish sound as it would resolve right now: "off", a file name
    /// from the ringtones dir, or "off" when the dir has no audio files. A
    /// configured file that has since disappeared falls back to the default.
    pub fn effective_finish_sound(&self) -> String {
        if self.finish_sound == "off" {
            return "off".into();
        }
        let files = list_ringtones();
        if files.iter().any(|f| *f == self.finish_sound) {
            self.finish_sound.clone()
        } else {
            files.into_iter().next().unwrap_or_else(|| "off".into())
        }
    }

    /// Full path of the ringtone to play on finish, or None when disabled
    /// (or no ringtones exist).
    pub fn finish_sound_path(&self) -> Option<std::path::PathBuf> {
        match self.effective_finish_sound().as_str() {
            "off" => None,
            name => Some(ringtones_dir().join(name)),
        }
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}
