use std::io::{Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// client -> server
pub const T_INPUT: u8 = 1;
pub const T_RESIZE: u8 = 2; // payload: rows u16 LE, cols u16 LE
pub const T_NEW_PANE: u8 = 3;
pub const T_CLOSE_PANE: u8 = 4;
pub const T_RENAME: u8 = 5; // payload: utf8 name
pub const T_MOVE: u8 = 6; // payload[0]: 0 = up, 1 = down
pub const T_FOCUS: u8 = 7;
pub const T_DETACH: u8 = 8;
pub const T_SHUTDOWN: u8 = 9;
pub const T_ATTACH: u8 = 10; // become the UI client (hello + replay follow)
pub const T_QUERY: u8 = 11; // request SessionState JSON
pub const T_READ_SCREEN: u8 = 12; // request rendered screen text of pane id
pub const T_AUTORESUME: u8 = 13; // payload[0]: 0 = off, 1 = on (per pane)
pub const T_WATCH: u8 = 14; // become a read-only observer (state + replay + live output)
pub const T_RESTORE: u8 = 15; // re-launch the agents from the last snapshot (see snapshot.rs)
/// Encoded mouse report for pane id. Separate from `T_INPUT` so the server
/// can drop it when no application owns the pane's pty — see the gate in
/// `Server::handle` and `Pane::app_foreground`.
pub const T_MOUSE: u8 = 16;
/// An observer viewed pane f.id: clear its sticky "finished" flag, the
/// same acknowledgement focusing it in the TUI performs — a green pane
/// seen on the phone shouldn't stay green forever. Leaves the
/// needs-input flag alone: peeking at a question isn't answering it.
pub const T_SEEN: u8 = 17;
/// Request the agent-transcript entries for pane f.id (see
/// `SrvPane::transcript_json`) — answered with a `T_TRANSCRIPT` to the
/// requesting connection only. The pty ring can't serve read mode's
/// history (agent TUIs repaint in place, so scrollback never forms);
/// the session transcript on disk is the real record.
pub const T_TRANSCRIPT_REQ: u8 = 18;

// server -> client
pub const T_HELLO: u8 = 20; // payload: Hello JSON
pub const T_REPLAY: u8 = 21; // payload: pty backlog (graphics stripped)
pub const T_OUTPUT: u8 = 22; // payload: pty bytes (graphics stripped)
pub const T_PANE_OPENED: u8 = 23; // payload: utf8 name
pub const T_PANE_CLOSED: u8 = 24;
pub const T_SERVER_EXIT: u8 = 25;
pub const T_STATE: u8 = 26; // payload: SessionState JSON
pub const T_SCREEN: u8 = 27; // payload: utf8 screen text
pub const T_GFX_STATE: u8 = 28; // payload: GfxSnapshot JSON for pane f.id
pub const T_GFX_IMG: u8 = 29; // payload: image data chunk, see gfx_img_*
pub const T_PANE_RENAMED: u8 = 30; // payload: utf8 name (auto-naming, see pane.rs)
pub const T_TRANSCRIPT: u8 = 31; // payload: transcript entries JSON (empty when unavailable)

/// T_GFX_IMG payload: 26-byte header + data chunk. Images larger than
/// GFX_CHUNK arrive as several frames distinguished by `off`; the client
/// assembles until `off + chunk_len == total`.
pub const GFX_IMG_HDR: usize = 26;
pub const GFX_CHUNK: usize = 1024 * 1024;

pub struct GfxImgHdr {
    pub img: u32,
    pub ver: u32,
    pub format: u8,
    pub zlib: bool,
    pub w: u32,
    pub h: u32,
    pub off: u32,
    pub total: u32,
}

impl GfxImgHdr {
    pub fn encode(&self) -> [u8; GFX_IMG_HDR] {
        let mut hdr = [0u8; GFX_IMG_HDR];
        hdr[0..4].copy_from_slice(&self.img.to_le_bytes());
        hdr[4..8].copy_from_slice(&self.ver.to_le_bytes());
        hdr[8] = self.format;
        hdr[9] = self.zlib as u8;
        hdr[10..14].copy_from_slice(&self.w.to_le_bytes());
        hdr[14..18].copy_from_slice(&self.h.to_le_bytes());
        hdr[18..22].copy_from_slice(&self.off.to_le_bytes());
        hdr[22..26].copy_from_slice(&self.total.to_le_bytes());
        hdr
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < GFX_IMG_HDR {
            return None;
        }
        let u = |r: std::ops::Range<usize>| {
            u32::from_le_bytes(data[r].try_into().unwrap())
        };
        Some(Self {
            img: u(0..4),
            ver: u(4..8),
            format: data[8],
            zlib: data[9] != 0,
            w: u(10..14),
            h: u(14..18),
            off: u(18..22),
            total: u(22..26),
        })
    }
}

const MAX_FRAME: usize = 8 * 1024 * 1024;

pub struct Frame {
    pub typ: u8,
    pub id: u64,
    pub data: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
pub struct HelloPane {
    pub id: u64,
    pub name: String,
    pub activity: bool,
    pub attention: bool,
    pub last_ms: Option<u64>,
}

#[derive(Serialize, Deserialize)]
pub struct Hello {
    pub panes: Vec<HelloPane>,
    pub active: u64,
    /// This server understands `T_MOUSE`. Missing from an older server's
    /// hello, where the client keeps sending mouse reports as `T_INPUT`
    /// (ungated, as before) rather than into a frame type that would be
    /// silently dropped.
    #[serde(default)]
    pub mouse_gate: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PaneState {
    pub index: usize,
    pub id: u64,
    pub name: String,
    pub title: String,
    pub status: String, // working | idle | done | needs_input
    pub agent: Option<String>, // claude | opencode | ... | None = plain shell
    pub cwd: Option<String>,
    pub focused: bool,
    pub auto_resume: bool, // API-stall watchdog enabled for this pane
    /// Milliseconds since the pane's shell was spawned (this boot).
    #[serde(default)]
    pub uptime_ms: u64,
    /// First line of `<agent> --version`, once probed (cached server-side).
    #[serde(default)]
    pub version: Option<String>,
    /// Claude's live "esc to interrupt" spinner row is on screen.
    #[serde(default)]
    pub thinking: bool,
    /// The agent's most recent transcript bullet ("⏺ …"), for the home
    /// page's blocks view. None for shells or when nothing matched.
    #[serde(default)]
    pub recap: Option<String>,
    /// A ≤6-word LLM summary of what the pane is doing right now, painted
    /// on the home-page card under the status line. None for shells, panes with
    /// no agent, or before the first summarization pass completes.
    #[serde(default)]
    pub subtitle: Option<String>,
    /// The host this pane's shell is `ssh`'d into, if any.
    #[serde(default)]
    pub ssh: Option<String>,
    /// The last few non-blank lines of the pane's rendered screen — the
    /// charts view's transcript well. Empty for shells.
    #[serde(default)]
    pub tail: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SessionState {
    pub session: String,
    pub attached: bool,
    /// Pane grid size (all panes share it) — lets observers (the astrolabe
    /// web mirror) size their emulator to match the byte stream exactly.
    #[serde(default)]
    pub rows: u16,
    #[serde(default)]
    pub cols: u16,
    pub panes: Vec<PaneState>,
    /// Host vitals for the home page's top strip: uptime, cpu, mem.
    /// Absent from old servers, so everything reading it tolerates None.
    #[serde(default)]
    pub host: Option<HostVitals>,
    /// Random secret generated once per server process (see `server::run`) —
    /// rides along on every state broadcast so an observer (the astrolabe
    /// bridge) always knows the current pairing token without a separate
    /// handshake. Rotates on a fresh process launch, stable across
    /// detach/reattach, gone when the process exits.
    #[serde(default)]
    pub pairing_token: String,
}

/// A coarse picture of the host the server runs on, probed locally and
/// cached a few seconds (see `server::host_vitals`). cpu_pct is the 1-min
/// load average over the core count — a steadiness read, not a spot sample.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct HostVitals {
    pub uptime_ms: u64,
    pub cpu_pct: u8,
    pub mem_pct: u8,
}

#[derive(PartialEq)]
pub enum TitleState {
    Working,
    Idle,
    Unknown,
}

/// Per-agent title patterns. Claude Code prefixes the terminal title with a
/// braille spinner while working and "✳" when idle. opencode sets a static
/// "OpenCode" title and pi a static "π - <dir>" (identity, no state) —
/// Unknown means "fall back to output-timing heuristics".
pub fn title_state(title: &str) -> TitleState {
    match title.chars().next() {
        Some(c) if ('\u{2800}'..='\u{28ff}').contains(&c) => TitleState::Working,
        Some('✳') => TitleState::Idle,
        _ => TitleState::Unknown,
    }
}

/// Identify the agent from the pane's terminal title, if it gives itself away.
pub fn agent_from_title(title: &str) -> Option<&'static str> {
    match title_state(title) {
        TitleState::Working | TitleState::Idle => Some("claude"),
        TitleState::Unknown => {
            if title.to_ascii_lowercase().starts_with("opencode") {
                Some("opencode")
            } else if is_pi_title(title) {
                Some("pi")
            } else {
                None
            }
        }
    }
}

/// pi titles its terminal "π - <dir>", or "π - <session name> - <dir>" once
/// the session is named. The separator is required: a bare "π" (or a title
/// that merely opens with the letter) shouldn't claim the pane.
fn is_pi_title(title: &str) -> bool {
    title.strip_prefix('π').is_some_and(|rest| rest.starts_with(" - "))
}

/// Fire-and-forget desktop notification; silently a no-op without notify-send.
pub fn notify(summary: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "zodiac", summary, body])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Fire-and-forget audio playback via the first available player; silently
/// a no-op when none is installed. mpv/ffplay decode anything (including
/// Apple .m4r/.m4a ringtones); pw-play/paplay cover the plain formats.
pub fn play_sound(path: &std::path::Path) {
    const PLAYERS: &[(&str, &[&str])] = &[
        ("mpv", &["--no-video", "--no-terminal", "--really-quiet"]),
        ("ffplay", &["-nodisp", "-autoexit", "-loglevel", "quiet"]),
        ("pw-play", &[]),
        ("paplay", &[]),
    ];
    for (bin, args) in PLAYERS {
        let spawned = std::process::Command::new(bin)
            .args(*args)
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if spawned.is_ok() {
            return;
        }
    }
}

/// A frame as one owned wire buffer — for writer threads that queue frames
/// instead of writing them inline.
pub fn encode_frame(typ: u8, id: u64, data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(13 + data.len());
    buf.push(typ);
    buf.extend_from_slice(&id.to_le_bytes());
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(data);
    buf
}

pub fn write_frame(w: &mut impl Write, typ: u8, id: u64, data: &[u8]) -> std::io::Result<()> {
    let mut hdr = [0u8; 13];
    hdr[0] = typ;
    hdr[1..9].copy_from_slice(&id.to_le_bytes());
    hdr[9..13].copy_from_slice(&(data.len() as u32).to_le_bytes());
    w.write_all(&hdr)?;
    w.write_all(data)?;
    w.flush()
}

pub fn read_frame(r: &mut impl Read) -> std::io::Result<Frame> {
    let mut hdr = [0u8; 13];
    r.read_exact(&mut hdr)?;
    let typ = hdr[0];
    let id = u64::from_le_bytes(hdr[1..9].try_into().unwrap());
    let len = u32::from_le_bytes(hdr[9..13].try_into().unwrap()) as usize;
    if len > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut data = vec![0; len];
    r.read_exact(&mut data)?;
    Ok(Frame { typ, id, data })
}

pub fn socket_path(session: &str) -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/zodiac-{}", unsafe { libc::getuid() })));
    dir.join("zodiac").join(format!("{session}.sock"))
}

pub fn state_dir(session: &str) -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        });
    base.join("zodiac").join(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_from_an_old_server_has_no_mouse_gate() {
        // Pre-T_MOUSE servers send a hello without the field; the client
        // must read that as "no gate" and keep using T_INPUT, or mouse
        // reports would vanish into a frame type the server ignores.
        let h: Hello = serde_json::from_str(r#"{"panes":[],"active":0}"#).unwrap();
        assert!(!h.mouse_gate);
    }

    #[test]
    fn hello_round_trips_mouse_gate() {
        let json = serde_json::to_string(&Hello {
            panes: Vec::new(),
            active: 3,
            mouse_gate: true,
        })
        .unwrap();
        let back: Hello = serde_json::from_str(&json).unwrap();
        assert!(back.mouse_gate);
        assert_eq!(back.active, 3);
    }

    #[test]
    fn agents_that_name_themselves_in_the_title() {
        assert_eq!(agent_from_title("✳ claude"), Some("claude"));
        assert_eq!(agent_from_title("⠼ claude"), Some("claude"));
        assert_eq!(agent_from_title("OpenCode"), Some("opencode"));
        assert_eq!(agent_from_title("π - zodiac"), Some("pi"));
        assert_eq!(agent_from_title("π - fix the parser - zodiac"), Some("pi"));
        // pi's title carries no working/idle state.
        assert!(title_state("π - zodiac") == TitleState::Unknown);
        // Near misses: a shell whose prompt happens to start with the
        // letter, or a bare glyph, mustn't claim the pane.
        assert_eq!(agent_from_title("π"), None);
        assert_eq!(agent_from_title("πalpha - beta"), None);
        assert_eq!(agent_from_title("zsh"), None);
    }

    #[test]
    fn client_frame_types_are_distinct() {
        let types = [
            T_INPUT, T_RESIZE, T_NEW_PANE, T_CLOSE_PANE, T_RENAME, T_MOVE, T_FOCUS, T_DETACH,
            T_SHUTDOWN, T_ATTACH, T_QUERY, T_READ_SCREEN, T_AUTORESUME, T_WATCH, T_RESTORE,
            T_MOUSE, T_SEEN,
        ];
        let mut seen = types.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), types.len(), "two client frame types collide");
    }
}
