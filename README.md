# 🧙‍♂️ zodiac

> *A TUI agent multiplexer — a summoning circle for your AI agents.*

You are a wizard. Your agents are familiars: eager, tireless, occasionally
in need of a stern tap on the shoulder when the astral connection drops.
zodiac gives you one scrying glass to command them all — the left sidebar
lists every agent pane (numbered top-to-bottom, renamable); the right side
is a full terminal emulator running that pane's shell. Background panes
keep running and rendering, so you can flip between long-running agents
like turning cards. 🔮

Each pane spawns `$SHELL` as a login shell (so your profile, prompt — e.g.
starship ✨ — and PATH are rebuilt fresh even under the long-lived server),
with `TERM=xterm-256color` and `COLORTERM=truecolor`. Summon whatever agent
you like inside it.

Panes are text terminals: the inner emulator (vt100) does not implement the
kitty graphics protocol or sixel, even when zodiac itself runs in a
graphics-capable terminal like ghostty — inline images inside panes won't
render. `TERM=xterm-256color` deliberately tells apps not to attempt it.
Some magic is forbidden. 📜

zodiac answers the terminal queries apps whisper on startup (vim's t_RV,
nvim's background-color probe, crossterm's kitty-keyboard check): DA1/DA2,
DSR/CPR, DECRQM, XTVERSION, window-size, XTGETTCAP (as "not supported"),
and OSC 10/11 color queries — which report white-on-black, so theme-sniffing
apps see a dark terminal, as is proper for the occult. Without replies these
apps would sit in their probe timeouts, which is why some TUIs used to take
seconds to start inside zodiac. The spirits demand acknowledgment.

## 🕯️ Sessions persist (necromancy included)

`zodiac` is client–server, like tmux: a background server owns the PTYs, the
UI just attaches. Closing the app (`Alt+Q`, or closing the terminal window)
**detaches** — every agent keeps toiling in the dark. Run `zodiac` again to
reattach exactly where you left off, scrollback included. `zodiac <name>`
opens a separate named session (default is `main`); attaching from a second
terminal takes the session over from the first. There can be only one
wizard per circle. 🧙

Across reboots, running processes cannot be preserved — nothing can do
that, not even magic — but zodiac saves pane names, order, working
directories, the active pane, and each pane's scrollback (metadata on every
change, scrollback every 60 s and on SIGTERM, so a normal reboot saves
cleanly). On next launch it raises each shell from the grave in its old
directory with a "restored session" banner above the old scrollback; revive
agents inside with e.g. `claude --resume`. State lives in
`~/.local/state/zodiac/<session>/` (config and state dirs from the
pre-rename `coop` days are migrated automatically on first launch — the
rebrand was foretold).

## 🃏 The home page (yes, it's a tarot spread)

zodiac always opens to the home page: a spread of tarot cards, one per
pane, each showing the pane's roman numeral and name, the agent and its
version (`claude 2.1.220`, probed once via `--version`), uptime, live
status — **thinking** (Claude's `esc to interrupt` spinner is on screen),
**working**, **finished**, **needs approval** (rang the bell), or idle —
and the working directory. Arrow keys move between cards, `Enter` drops
you into that agent's pane, and a click does both at once. `Alt+~` (or
`` Alt+` ``) brings the spread back from anywhere; `Esc` returns to the
current pane. Your fortune: *you will review a large diff soon.* ✨

Each card carries an emblem up top: for claude panes, a bouncing coral
mascot while claude is working or thinking, and Claude's `✳` starburst
when it's idle; other panes get a gold `>_` prompt. (The bounce is
painted-art only — the Unicode fallback always shows the `✳`.) The
**Claude style** setting picks his body shape: `hard` (boxy, the default)
or `soft` (rounded).

In a kitty-graphics terminal (ghostty, kitty) each card gets painted art —
a night-sky gradient with stars 🌙, the emblem rendered as vector art, a
gold double frame, and a glow in the status color — placed under the text
via the graphics protocol. Everywhere else the cards fall back to pure
Unicode ornament, which is its own kind of spell.

## ⌨️ Incantations (keys)

All multiplexer keys use `Alt` so the inner terminal keeps `TAB`, `Ctrl+W`,
`Ctrl+N`, etc. for itself. No wand required.

| Key | Action |
| --- | --- |
| `Alt+~` | Toggle the home page (tarot-card overview of every agent) |
| `Alt+N` | New pane (spawns `$SHELL`) |
| `Alt+W` | Close pane — kills its process. Closing the last pane ends the session. |
| `Alt+R` | Rename pane (`Enter` save, `Esc` cancel) |
| `Alt+1`–`9` | Jump to pane by number |
| `Alt+↑` / `Alt+↓` | Step through the pane list |
| `Alt+PgUp` / `Alt+PgDn` | Move pane up/down the list (numbers are positional, so its number changes with its slot) |
| `Alt+T` | Collapse/expand the sidebar to numbers-only |
| `Alt+Z` | Zoom the active pane full-width (hides the sidebar) |
| `Ctrl+S` | Settings (the one non-Alt binding — inner apps don't see Ctrl+S) |
| `Shift+PgUp` / `Shift+PgDn` | Scrollback in the active pane (any keystroke snaps back live) |
| `Alt+Q` | Detach — session and all agents keep running |
| `Alt+Shift+Q` | Banish everything (kills all panes and the server) |

A pane whose shell exits (e.g. you type `exit`) is removed automatically.
It has served its purpose.

## 🌈 Omens (sidebar status colors)

The focused pane's name is underlined (just the name, not the whole row),
and its number is replaced by an eye (`ಠ`, or `◉` — see Settings) that
blinks every few seconds — you're looking at it, and it's looking back. 👁️
The working spinner shows on every working pane, focused or not; the
*color* omens below apply only to background panes (focusing a pane clears
its sticky state):

- **Working**: shown by an animated indicator flush right in the sidebar
  (see Settings below; a single bar when the sidebar is collapsed) — the
  pane's name stays uncolored, only the spinner is orange. While the
  spinner runs, the name also shimmers, Claude Code style: a bright band
  sweeping across dimmed text — on the focused row too, where it layers
  with the underline. A pane counts as working when a braille spinner frame
  starts its terminal title (Claude Code animates `✳`/`⠂`/`⠐`/… while
  working), **or** it produced output in the last 5 s *and* is identified
  as running a known agent. The `✳` frame alone proves nothing — it's part
  of the working animation *and* the resting idle marker — so agent panes
  with non-braille titles fall through to output recency (safe for agent
  TUIs: their spinners keep emitting output while they work; Claude Code
  goes quiet only when idle). Non-agent panes never count recency, or an
  ordinary TUI (htop, a music player) would spin forever, like a cursed
  music box.
- **Green — finished** ✅: it did work since you last looked and has gone
  quiet. Sticky until you focus the pane. This transition also plays the
  **finish sound** (see Settings): a ringtone from
  `~/.config/zodiac/ringtones/`, played server-side, so it fires for
  background panes while attached and for every pane while detached — the
  pane you're actively watching stays silent. The bell tolls only for work
  you haven't seen.
- **Red — needs approval / stopped** 🔴: the pane rang the terminal bell
  while in the background — Claude Code's "blocked on you" signal (keep its
  terminal bell notifications enabled). Sticky until focused.
  Red > orange > green, as in any respectable hierarchy of dread.

A pane turning red also fires a desktop notification via `notify-send`,
from the UI when attached and from the server when detached.

Output arriving within ~1.2 s of a resize (toggling the sidebar or zoom
resizes every pane) is treated as the SIGWINCH repaint storm, not agent
activity — so `Alt+T`/`Alt+Z` don't light up every pane's spinner. Not
every twitch is a portent.

## ⚗️ The watchdog (auto-resume on API stalls)

Every wizard needs a familiar that never sleeps. Claude Code sessions
sometimes wedge on Anthropic API hiccups — the astral link falters. When a
pane running claude shows either of these at the bottom of its screen,
zodiac's watchdog automatically presses `Esc` (interrupt), clears the input
box (`Ctrl+U`, so a leftover `--resume` from an earlier attempt can't
double up), and submits `--resume`:

- `API Error: Response stalled mid-stream. The response above may be
  incomplete.` — acts after the message has sat there for ~6 s.
- `API Error: Connection closed mid-response. The response above may be
  incomplete.` — acts immediately (a closed connection is never transient).
  This phrase can be toggled with the **Conn-error resume** setting
  (`Ctrl+S`, default on); the server re-reads the setting every second, so
  the toggle applies live.
- `Waiting for API response` — acts only after ~30 s, since this also
  appears briefly on healthy requests. Patience is a virtue; 30 seconds of
  it is plenty.

The match is whitespace-insensitive, only the bottom 15 rows count, and it
only fires in panes identified as running claude. A row must also carry the
visual signature of the real status line, so merely *quoting* these phrases
in a conversation doesn't trip the watchdog: the API-error phrase must start
its row (a short decoration prefix like `⎿` is allowed) and be painted in an
error color, and the waiting phrase must sit on a live spinner line (leading
spinner glyph, `esc to interrupt` suffix in the same row). False prophets
are ignored. After intervening, zodiac waits for the phrase to leave the
screen before it can trigger again (with a 90 s retry if it never clears,
in case the first interrupt didn't take). Each intervention fires a desktop
notification. The keystroke sequence is paced on its own thread, so a
firing watchdog doesn't pause I/O for the other panes.

The watchdog is on by default; toggle it per pane with
`zodiac autoresume <pane> on|off` (persisted with the session).

## 📜 Scrolls (CLI / scripting API)

Every command talks to the running server over its socket without
disturbing the attached UI (`-s <session>` selects a session, default
`main`). Agents commanding agents commanding agents — the circle is
complete: 💫

```
zodiac ls [--json]                   # panes with semantic status: working | idle | done | needs_input
zodiac read <pane>                   # print a pane's rendered screen
zodiac send <pane> <text> [--enter]  # type into a pane
zodiac prompt <pane> <text>          # submit text + Enter (prompt an agent)
zodiac rename <pane> <name>
zodiac focus <pane>
zodiac new
zodiac close <pane>
zodiac wait <pane> [--state s1,s2] [--timeout secs]   # block until state reached
zodiac autoresume <pane> on|off      # toggle the API-stall watchdog (default on)
zodiac kill-server
zodiac --remote <ssh-host> [session] # attach to zodiac on another machine (ssh -t)
```

`ls` also divines which agent runs in each pane (`claude`, `opencode`, …),
identified by title patterns plus a /proc walk over the pane's process tree
for known agent binaries (claude, opencode, codex, aider, gemini, goose).

Pane numbers are the 1-based sidebar positions. `zodiac wait 3 --state
needs_input,idle` is the building block for "tell me when the agent is
done" scripting; agents can drive other agents with
`zodiac prompt`/`zodiac read`.

## ⚙️ The grimoire (settings)

`Ctrl+S` opens the settings page. `↑`/`↓` select a setting, `←`/`→` change
its value (with a live preview), `Esc` or `Ctrl+S` closes. Settings persist
to `~/.config/zodiac/config.json` and apply to all sessions.

- **Working animation** — the sidebar indicator for working agents. Default
  is `equalizer` (bounce-and-stretch bars); the other styles are from the
  [FGRibreau/spinners](https://github.com/FGRibreau/spinners) collection:
  dots, line, pipe, arc, triangle, circle-halves, square-corners,
  grow-vertical, noise, toggle, star, point, arrow, bouncing-bar, aesthetic,
  bouncing-ball. In the collapsed sidebar, multi-cell styles fall back to a
  single equalizer bar.
- **Spinner color** — the working indicator's color: orange (default),
  gold, cyan, blue, violet, pink, green, red, or white. Also tints the
  status bar's `· working` note.
- **Shimmer color** — the bright band that sweeps across a working pane's
  name: white (default) or any of the colors above.
- **Shimmer speed** — how fast the band sweeps: slow, normal (default),
  fast, or zippy. Both shimmer rows show a live preview.
- **Focus eye** — the blinking marker on the focused pane. Each style is an
  open/blink glyph pair: `eye` (`ಠ`/`‿`, the default), `dot` (`◉`/`─`),
  `star` (`✦`/`✧`), `heart` (`♥`/`♡`), `diamond` (`◆`/`◇`), `pulse`
  (`●`/`○`), `flower` (`✿`/`❀`), `note` (`♪`/`♫`), `arrow` (`▶`/`▷`).
- **Sidebar frame** — `separator` (just the line between the tabs and the
  terminal, the default), `surround` (a full border around the sidebar), or
  `rounded` (surround with rounded corners).
- **Sidebar weight** — `normal`, `thick`, or `double` border lines.
  Unicode has no thick or double rounded corners, so those weights render
  square corners even in `rounded` mode. Even magic has limits.
- **Sidebar color** — the border's color: `dark` (dim gray, the default)
  or any color from the spinner palette plus `gray`.
- **Card icon** — size of the emblem painted on home-page cards (the `>_`
  prompt / Claude starburst): small, medium (default), large, or huge.
  Applies to the kitty-graphics art; the Unicode fallback emblem is text
  and keeps its size. Settings opened from the home page apply live.
- **Card outline** — the card's own frame: `double` (default), `single`,
  or `none`. In painted-art mode this sets the gold rings in the image —
  the text border is gone there entirely, so card edges no longer layer
  box-drawing lines over the art. The fallback maps double/single to
  double/rounded text borders.
- **Select color** / **Select weight** — the selected-card outline: any
  palette color (gold default) at thin/normal/thick/heavy. With painted
  art the selection is a ring at the card's true pixel edge, so it fully
  surrounds the card instead of running through cell centers; the fallback
  maps thick/heavy to thick/double text borders.
- **Select style** — how the painted ring renders: `glow` (rounded
  corners with a soft halo, the default) or `ring` (hard square).
- **Card numeral** — how cards are numbered: `roman` (I, II, III — the
  default), `arabic` (1, 2, 3), or `zodiac` (♈ ♉ ♊ …, wrapping after ♓).
  You know which one to pick. ♒
- **Finish sound** — the ringtone played when an agent finishes (working →
  green). Choices are `off` plus every audio file in
  `~/.config/zodiac/ringtones/` (mp3/m4a/m4r/aac/wav/ogg/opus/flac/aiff/caf —
  drop files in, they're picked up live); stepping through the list previews
  each sound. Default is the first ringtone alphabetically, so the alert
  works as soon as the folder has files. Playback uses the first of
  mpv/ffplay/pw-play/paplay found on PATH.
- **Conn-error resume** — the immediate auto-resume on Claude Code's
  "Connection closed mid-response" API error (see the watchdog section
  above). Default on. Like Finish sound, this is read by the server and
  re-checked every second, so toggling it applies to running sessions
  right away.

## 🖱️ Mouse

zodiac owns the mouse, so selection is confined to the pane — the sidebar
can never end up in your copy:

- **Drag to select, release to copy.** The selection highlights, and on
  release the text goes to the clipboard via OSC 52 (works over ssh /
  `--remote`) plus `wl-copy` when available. The status bar flashes
  `· copied`. Any keystroke or click clears the highlight.
- **Click a sidebar row** to focus that pane.
- **Wheel** scrolls zodiac's scrollback in shell panes; in fullscreen apps
  it becomes arrow keys (the usual "alternate scroll").
- Apps that ask for mouse reporting (vim, htop, some agent TUIs) get mouse
  events forwarded instead — hold **Shift** while dragging to select in
  those panes anyway, the standard terminal convention.

## ⚠️ Curses (caveats)

- `Shift+PgUp/PgDn` reaches zodiac only if your terminal emulator passes it
  through while in the alternate screen (foot, kitty, alacritty all do).
- Scrollback is 10,000 lines per pane, held in memory. Beyond that, the
  past is lost to the void. 🌌

## 🛠️ The ritual (build)

```sh
nix shell nixpkgs#gcc -c cargo build --release   # NixOS: cc needed for build scripts
./target/release/zodiac
```

No goats were harmed. Rust only asks for your patience at link time. 🧙‍♂️✨
