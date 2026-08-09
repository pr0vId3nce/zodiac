//! UI/behavior preferences, persisted to ~/.config/zodiac/config.json.
//! Shared by all sessions: the client's settings page writes it, and the
//! server re-reads it on each watchdog tick, so toggles apply live without
//! any protocol traffic.

fn default_true() -> bool {
    true
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
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
    /// Chat panel character: "assistant", "oracle" (ascii orb), or "hal".
    #[serde(default)]
    pub chat_face: String,
    /// UI palette: "night" (navy + brass), "oled-orange", or "oled-green"
    /// (true-black variants).
    #[serde(default)]
    pub theme: String,
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
    /// Card numbering: "zodiac" (default), "roman", or "arabic".
    #[serde(default)]
    pub card_numeral: String,
    /// Mascot body shape: "hard" (boxy, default) or "soft" (rounded).
    #[serde(default)]
    pub claude_style: String,
    /// Watchdog for Claude Code's "API Error: Connection closed
    /// mid-response" line — fires `--resume` immediately on sight.
    #[serde(default = "default_true")]
    pub connection_watch: bool,
    /// Capability floor (ADR 0005): what the server advertises to child
    /// apps regardless of the attached client — "off", "images" (default),
    /// or "animation". Children keep TERM=xterm-256color; degradation
    /// happens at client render time. Applies to newly probed apps only.
    #[serde(default)]
    pub capability_floor: String,
    /// Advertise the kitty keyboard protocol to children (roadmap 4.4):
    /// when on, the CSI ? u probe is answered with the pane's current
    /// flags, so apps enable CSI-u sequences and clients synthesize them
    /// (the GUI natively; the TUI when its host disambiguates).
    /// Default off — flip it once the GUI is the daily driver.
    #[serde(default)]
    pub kitty_keyboard: bool,
    /// Honor children's OSC 52 clipboard writes (roadmap 4.7): when on,
    /// an allowed write reaches the system clipboard via the GUI. Default
    /// off — a terminal writing your clipboard silently is a footgun, so
    /// it is opt-in, the gate ADR 0005 / Phase 2 inbox anticipates.
    #[serde(default)]
    pub clipboard_write: bool,
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
    /// GUI client only (zodiac-gui): where the pane tabs sit — "top"
    /// (default, a tab bar across the top) or "side" (a left column, like
    /// the TUI's sidebar). Ignored by the TUI/server.
    #[serde(default)]
    pub gui_tabs: String,
    /// GUI backdrop preset: "oled" (#000, default), "charcoal", "midnight",
    /// "slate". GUI-only.
    #[serde(default)]
    pub gui_bg: String,
    /// GUI font scale as a percent string ("100%", "125%", …). GUI-only.
    #[serde(default)]
    pub gui_scale: String,
    /// GUI tab marker glyphs: "dots" (default), "arabic", "roman", "zodiac"
    /// (white/text-presentation sigils). GUI-only.
    #[serde(default)]
    pub gui_tab_marker: String,
    /// Where the braille spinner sits on a working GUI tab: "replace" (the
    /// marker, default), "both" (marker + spinner), or "end" (after the
    /// title). GUI-only.
    #[serde(default)]
    pub gui_spinner_pos: String,
    /// Hide the keybinding hints in the bottom status bar (they stay
    /// visible in the settings page's Controls column).
    #[serde(default)]
    pub hide_controls: bool,
    /// Ringtone played when an agent finishes (working → done). A file name
    /// inside the ringtones dir, "off" to disable, or "" for the default
    /// (first ringtone alphabetically).
    #[serde(default)]
    pub finish_sound: String,
    /// The chat panel on the home page (talks to an OpenAI-compatible
    /// endpoint over the tailnet).
    #[serde(default = "default_true", alias = "wizard_chat")]
    pub chat_panel: bool,
    /// OpenAI-compatible endpoint, e.g. "http://bigbox:8091" (the default).
    #[serde(default, alias = "wizard_endpoint")]
    pub chat_endpoint: String,
    /// Model name sent in completion requests ("" = qwen3.6-35b-a3b).
    #[serde(default, alias = "wizard_model")]
    pub chat_model: String,
    /// ssh destination used to start/stop the model ("" = des@bigbox).
    #[serde(default, alias = "wizard_ssh")]
    pub chat_ssh: String,
    /// systemd --user unit started/stopped over ssh ("" = llama-server).
    #[serde(default, alias = "wizard_service")]
    pub chat_service: String,
    /// Where the chat panel's `web_search` tool looks. Empty = Wikipedia only
    /// (keyless, works anywhere). A SearXNG base URL, e.g.
    /// "http://bigbox:8888", uses its JSON API. "https://api.search.brave.com"
    /// uses Brave's API and needs `chat_search_key`.
    #[serde(default, alias = "wizard_search_url")]
    pub chat_search_url: String,
    /// API key for the search endpoint, when it wants one (Brave).
    #[serde(default, alias = "wizard_search_key")]
    pub chat_search_key: String,
    /// Chat panel width in cells ("" = 40).
    #[serde(default, alias = "wizard_width")]
    pub chat_width: String,
    /// Background pane monitor: classifies stuck/needs-input panes
    /// against a local policy file and notifies with the reason. Advisory
    /// only — it never touches a pane. Runs server-side against a separate
    /// CPU-only model, independent of the chat panel above. Does nothing
    /// until `monitor_endpoint` is set.
    #[serde(default = "default_true", alias = "wizard_watch")]
    pub pane_monitor: bool,
    /// OpenAI-compatible endpoint the pane monitor and card-subtitle
    /// summarizer talk to, e.g. "http://mybox:8092". Blank (the default)
    /// disables both — no background network traffic at all.
    #[serde(default)]
    pub monitor_endpoint: String,
    /// Model name sent in monitor/summarizer requests ("" = whatever
    /// single model the endpoint serves; llama-server ignores it).
    #[serde(default)]
    pub monitor_model: String,
    /// Lets the chat panel offer `prompt_pane`/
    /// `send_keys` — the only way it can ever touch a pane. Off by
    /// default: even enabled, every use still shows a consent chip in the
    /// transcript and waits on a keypress, no timeout, no auto-accept.
    #[serde(default, alias = "wizard_act")]
    pub chat_act: bool,
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
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
            })
            .unwrap_or_default()
            .join("zodiac/config.json")
    }

    /// The effective capability floor (ADR 0005): unset/unknown values
    /// mean "images" — the v1 default that matches what every client
    /// ships. "off" silences graphics probe replies entirely;
    /// "animation" additionally accepts kitty animation frames.
    pub fn floor(&self) -> &'static str {
        match self.capability_floor.as_str() {
            "off" => "off",
            "animation" => "animation",
            _ => "images",
        }
    }

    /// GUI tab placement (zodiac-gui): "side" or "top" (default).
    pub fn gui_tabs(&self) -> &str {
        match self.gui_tabs.as_str() {
            "side" => "side",
            _ => "top",
        }
    }

    /// GUI backdrop preset name (default "oled"). The GUI maps it to RGB.
    pub fn gui_bg(&self) -> &str {
        match self.gui_bg.as_str() {
            "charcoal" => "charcoal",
            "midnight" => "midnight",
            "slate" => "slate",
            _ => "oled",
        }
    }

    /// GUI font-scale multiplier parsed from the percent string (default 1.0).
    /// Clamped to a sane 0.5–3.0 so a bad value can't make the grid unusable.
    pub fn gui_scale(&self) -> f32 {
        self.gui_scale
            .trim_end_matches('%')
            .parse::<f32>()
            .map(|p| (p / 100.0).clamp(0.5, 3.0))
            .unwrap_or(1.0)
    }

    /// GUI tab marker style (default "dots").
    pub fn gui_tab_marker(&self) -> &str {
        match self.gui_tab_marker.as_str() {
            "arabic" => "arabic",
            "roman" => "roman",
            "zodiac" => "zodiac",
            _ => "dots",
        }
    }

    /// GUI working-tab spinner position (default "replace").
    pub fn gui_spinner_pos(&self) -> &str {
        match self.gui_spinner_pos.as_str() {
            "both" => "both",
            "end" => "end",
            _ => "replace",
        }
    }

    pub fn load() -> Self {
        std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// `load()` behind a 1s cache — the server's per-second ticks re-read
    /// config several times a tick to apply toggles live; one disk read +
    /// parse per second is exactly as live.
    pub fn cached() -> Self {
        use std::sync::Mutex;
        use std::time::{Duration, Instant};
        static CACHE: Mutex<Option<(Instant, Settings)>> = Mutex::new(None);
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, s)) = cache.as_ref() {
            if at.elapsed() < Duration::from_secs(1) {
                return s.clone();
            }
        }
        let s = Self::load();
        *cache = Some((Instant::now(), s.clone()));
        s
    }

    /// The finish sound as it would resolve right now: "off", a file name
    /// from the ringtones dir, or "off" when the dir has no audio files. A
    /// configured file that has since disappeared falls back to the default.
    pub fn effective_finish_sound(&self) -> String {
        if self.finish_sound == "off" {
            return "off".into();
        }
        let files = list_ringtones();
        if files.contains(&self.finish_sound) {
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

#[cfg(test)]
mod tests {
    use super::Settings;

    /// A config written before the chat panel and pane monitor were renamed
    /// still loads: every key kept a serde alias for its old name.
    #[test]
    fn legacy_wizard_keys_still_load() {
        let old = r#"{
            "wizard_chat": false,
            "wizard_endpoint": "http://box:9000",
            "wizard_model": "qwen",
            "wizard_ssh": "des@box",
            "wizard_service": "llama",
            "wizard_width": "50",
            "wizard_search_url": "http://box:8888",
            "wizard_search_key": "k",
            "wizard_watch": false,
            "wizard_act": true
        }"#;
        let s: Settings = serde_json::from_str(old).expect("legacy config parses");
        assert!(!s.chat_panel);
        assert_eq!(s.chat_endpoint, "http://box:9000");
        assert_eq!(s.chat_model, "qwen");
        assert_eq!(s.chat_ssh, "des@box");
        assert_eq!(s.chat_service, "llama");
        assert_eq!(s.chat_width, "50");
        assert_eq!(s.chat_search_url, "http://box:8888");
        assert_eq!(s.chat_search_key, "k");
        assert!(!s.pane_monitor);
        assert!(s.chat_act);
    }

    /// New names win when both are present, and defaults hold when neither is.
    #[test]
    fn new_keys_and_defaults() {
        let s: Settings = serde_json::from_str(r#"{"chat_endpoint": "http://new:1"}"#).unwrap();
        assert_eq!(s.chat_endpoint, "http://new:1");
        assert!(s.chat_panel, "chat panel defaults on");
        assert!(s.pane_monitor, "pane monitor defaults on");
        assert!(!s.chat_act, "acting defaults off");
    }
}
