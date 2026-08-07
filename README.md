# zodiac

**A terminal multiplexer built around AI coding agents.**

tmux will happily run six agents for you and tell you nothing about any of
them. zodiac's whole premise is the opposite: the sidebar lists every pane,
and each row tells you what's running there, what it's doing right now, and
whether it's blocked waiting on you. The right side is a full terminal
emulator for the focused pane; background panes keep running and rendering,
so switching costs nothing.

Each pane spawns `$SHELL` as a login shell — your profile, prompt and PATH
are rebuilt fresh even under the long-lived server — with
`TERM=xterm-256color` and `COLORTERM=truecolor`. Run whatever agent you like
inside it; zodiac recognizes claude, opencode, codex, aider, gemini and
goose, and treats everything else as an ordinary shell.

Also here: a phone UI over Tailscale ([astrolabe](astrolabe/README.md)), a
scripting CLI, kitty-graphics support inside panes, and a watchdog that
un-sticks Claude Code when the API stalls.

The built-in chat panel is disabled until you give it an endpoint. To point
it at a model of your own instead, see [LOCAL_MODEL.md](LOCAL_MODEL.md).

---

## Install

```sh
cargo build --release && ./target/release/zodiac
```

On NixOS, `cc` is needed for build scripts: `nix shell nixpkgs#gcc -c cargo
build --release`. There's also a flake — `nix run github:pr0vId3nce/zodiac`,
or add `zodiac.packages.${system}.default` to your system packages. Tagged
releases (`v*`) build binaries for x86_64/aarch64 Linux and both macOS
architectures via GitHub Actions.

**Platforms.** Linux is the primary target; macOS works, including process,
working-directory and agent detection (via libproc rather than `/proc`).
Desktop notifications go through `notify-send`, so they're a no-op on macOS
until something equivalent is wired up. Finish sounds use the first of
mpv/ffplay/pw-play/paplay found on PATH.

## Sessions

zodiac is client–server like tmux: a background server owns the PTYs and the
UI attaches to it. `Alt+Q`, or closing the terminal window, **detaches** —
every agent keeps working. Run `zodiac` again to reattach exactly where you
left off, scrollback included. `zodiac <name>` opens a separate named session
(default `main`); attaching from a second terminal takes the session over
from the first.

Reboots can't preserve running processes, but zodiac saves pane names, order,
working directories, the active pane, and each pane's scrollback — metadata
on every change, scrollback every 60 s and on `SIGTERM`, so a normal reboot
saves cleanly. On next launch each shell comes back in its old directory with
a "restored session" banner above the recovered scrollback. State lives in
`~/.local/state/zodiac/<session>/`.

### Bringing the agents back

The server also writes `snapshot.json` next to that state every 60 s: session
name, save time, and per pane its index, name, directory, the agent running
there, that agent's model, and — for claude — the **chat id** of the
conversation on screen. At startup the previous file is kept as
`snapshot.prev.json`, but only if it had agents in it, so restarting twice
can't push the last useful snapshot out.

**`Alt+Shift+R`** replays it. The overlay lists what the snapshot holds and
`Enter` puts it back: each pane gets `cd <directory> && claude --resume <chat
id>` typed in, so claude reopens the same conversation instead of a blank
one. Other agents relaunch by name. Panes the snapshot had but this session
doesn't are opened first. Panes already running that agent, or with anything
else in the foreground, are left alone — nothing gets typed into your vim,
and pressing the key twice is harmless.

`zodiac restore` does the same from a script. `scripts/zodiac-restore.sh`
reads the JSON itself, for driving it from outside the session:

```sh
scripts/zodiac-restore.sh                # session 'main', last snapshot
scripts/zodiac-restore.sh -s work -n     # another session, dry run
scripts/zodiac-restore.sh --from <file>  # a specific snapshot
```

## Panes name themselves

A new pane is named for its working directory (`zodiac`, not the whole path;
`~` for home). Open something in it and the name follows: `nvim`, `htop`,
`psql`. SSH into a box and it becomes the hostname. Start an agent and it
becomes the agent — and for claude and opencode, the **model** that agent is
currently using: `opus 5`, `sonnet 4.5`, `fable 5`.

Priority runs agent (or its model) → ssh host → foreground app → directory,
re-evaluated once a second. claude's model comes from its session transcript,
so `/model` switches show up within a second; opencode's comes from the
`provider/model` in its footer.

`Alt+R` overrides all of it: a name you type is pinned until you clear it
(empty rename un-pins and hands the name back to the automatic logic).

## Home page

zodiac opens to an overview of every pane — three layouts, switchable in
settings: **cards** (a grid), **list** (stacked rows), **blocks** (wide rows
with recent transcript). A card shows the pane's numeral and name, the agent
and its version (probed once via `--version`), uptime, working directory,
live status, and — when a local model is configured — a short summary of what
that pane is doing and its latest `⏺` transcript line.

Status is one of **thinking** (Claude's `esc to interrupt` spinner is on
screen), **working**, **finished**, **needs approval** (the pane rang the
bell), or idle. Arrow keys move between panes, `Enter` opens one, and a click
does both. `Alt+~` returns here from anywhere; `Esc` goes back to the current
pane.

In a kitty-graphics terminal (ghostty, kitty) cards are painted images — a
night-sky gradient, a vector emblem, a gold frame, a glow in the status
color — composited under the text. Everywhere else they fall back to Unicode
box-drawing. claude panes get an animated mascot while working and Claude's
`✳` when idle; other panes get a `>_` prompt.

## Keys

Everything uses `Alt` so the inner terminal keeps `Tab`, `Ctrl+W`, `Ctrl+N`
and friends for itself.

| Key | Action |
| --- | --- |
| `Alt+~` | Toggle the home page |
| `Alt+N` | New pane (spawns `$SHELL`) |
| `Alt+W` | Close pane — kills its process. Closing the last one ends the session. |
| `Alt+R` | Rename pane (`Enter` save, `Esc` cancel; empty name restores auto-naming) |
| `Alt+Shift+R` | Restore the last session's agents from the snapshot |
| `Alt+/` | Fuzzy-find a pane by name |
| `Alt+1`–`9` | Jump to pane by number |
| `Alt+↑` / `Alt+↓` | Step through the pane list |
| `Alt+PgUp` / `Alt+PgDn` | Move pane up/down (numbers are positional) |
| `Alt+T` | Collapse the sidebar to numbers only |
| `Alt+Z` | Zoom the active pane full-width |
| `Alt+O` | Chat overlay (see below; `Alt+G` still works as a legacy alias) |
| `Alt+P` | Pairing QR for the phone UI |
| `Ctrl+S` | Settings — the one non-Alt binding, since inner apps don't see `Ctrl+S` |
| `Shift+PgUp` / `Shift+PgDn` | Scrollback in the active pane (any keystroke snaps back to live) |
| `Alt+Q` | Detach — session and agents keep running |
| `Alt+Shift+Q` | Kill everything: all panes and the server |

A pane whose shell exits is removed automatically.

## Status signals

The focused pane's name is underlined and its number becomes a blinking eye.
The working indicator shows on every working pane; the *colors* below apply
to background panes only, and focusing a pane clears its sticky state.

- **Working** — an animated indicator flush right in the sidebar, plus a
  bright band sweeping across the pane's name. A pane counts as working when
  a braille spinner frame starts its terminal title (Claude Code animates
  one), or it produced output in the last 5 s *and* is running a known agent.
  Non-agent panes never count output recency, or htop would spin forever.
- **Green — finished.** It did work since you last looked and has gone quiet.
  Sticky until you focus it. This also plays the finish sound: a ringtone
  from `~/.config/zodiac/ringtones/`, played server-side, so it fires for
  background panes while attached and for every pane while detached. The pane
  you're actively watching stays silent.
- **Red — needs approval.** The pane rang the terminal bell while in the
  background, which is Claude Code's "blocked on you" signal (keep its bell
  notifications enabled). Sticky until focused, and also fires a desktop
  notification.

Output arriving within ~1.2 s of a resize is treated as the SIGWINCH repaint
storm rather than agent activity, so `Alt+T` and `Alt+Z` don't light up every
pane at once.

## Auto-resume watchdog

Claude Code sessions sometimes wedge on API errors. When a claude pane shows
one of these at the bottom of its screen, zodiac presses `Esc`, clears the
input box with `Ctrl+U` (so a leftover `--resume` can't double up), and
submits `--resume`:

- `API Error: Response stalled mid-stream` — after the message has sat there
  ~6 s.
- `API Error: Connection closed mid-response` — immediately; a closed
  connection is never transient. Toggleable via **Conn-error resume** in
  settings, re-read by the server every second.
- `Waiting for API response` — only after ~30 s, since this also appears
  briefly on healthy requests.

Matching is whitespace-insensitive, limited to the bottom 15 rows, and only
fires in panes running claude. The row must also carry the visual signature
of a real status line — the error phrase starting its row in an error color,
the waiting phrase on a live spinner line — so an agent merely *discussing*
these strings doesn't trip it. After intervening, zodiac waits for the phrase
to leave the screen before it can fire again (with a 90 s retry if it never
clears). Each intervention fires a desktop notification, and the keystrokes
are paced on their own thread so other panes keep streaming.

On by default; `zodiac autoresume <pane> on|off` toggles it per pane, and the
choice persists with the session.

## CLI

Every command talks to the running server over its socket without disturbing
the attached UI. `-s <session>` selects a session (default `main`).

```
zodiac ls [--json]                   # panes with status (alias: list)
zodiac read <pane>                   # print a pane's rendered screen
zodiac send <pane> <text> [--enter]  # type into a pane
zodiac prompt <pane> <text>          # submit text + Enter (prompt an agent)
zodiac rename <pane> <name>
zodiac focus <pane>
zodiac new
zodiac close <pane>
zodiac wait <pane> [--state s1,s2] [--timeout secs]   # block until a state is reached
zodiac autoresume <pane> on|off      # per-pane API-stall watchdog
zodiac restore                       # re-launch the agents from the last snapshot
zodiac kill-server
zodiac --remote <ssh-host> [session] # attach to zodiac on another machine (ssh -t)
```

Pane numbers are the 1-based sidebar positions. Text passed to `send` and
`prompt` is typed verbatim, flags included, so `zodiac send 2 "claude
--resume $id" --enter` does what it looks like. `zodiac wait 3 --state
needs_input,idle` is the building block for "tell me when the agent is done",
and agents can drive other agents with `prompt` and `read`.

## astrolabe — the phone UI

`astrolabe/` is a companion you open on your phone over Tailscale: every
pane's status at a glance, a live colored terminal mirror per pane with
searchable scrollback, a slash-command palette, a keys pad, and a reply box
your phone's own voice dictation works in. When an agent asks a numbered
question, the options become buttons — tap one, optionally with a note.

It rides a read-only observer mode in the server (`T_WATCH`): observers get
state, replay and live output without ever disturbing the attached UI.
Pairing is by QR (`Alt+P`) carrying a token the server mints per launch.

Two live pieces: a TypeScript **bridge** (zodiac's socket on one side, HTTP +
WebSocket on the other) and a native **iOS** app — named zodiac on the phone
too — that holds a list of paired computers, renders panes natively (herd
view, terminal mirror, answer buttons, widgets), delivers push notifications,
and shows the selected machine's uptime, battery, CPU and memory. A React
web mirror also lives in `astrolabe/web` but is paused in favor of the
native app — the bridge serves it only with `?web=1`. The iOS sources are
developed in-tree but not published with this repo; everything they talk to
is here. See [astrolabe/README.md](astrolabe/README.md).

## Chat panel

`Alt+O` summons a floating chat overlay on the home page (Esc or a click
outside minimizes it; `Alt+G` remains as a legacy alias) that talks to an
OpenAI-compatible endpoint over your tailnet (a llama-server, by default).
It's a general assistant, not a session narrator: the pane overview isn't fed
to it unless the question sounds like it concerns the session, plus one turn
of momentum so follow-ups still land. It can pull the overview in itself via
a `read_panes` tool, and `/why <n>` attaches it outright (`/read <n>` — or
its older name `/scry <n>` — quotes a pane's screen into the transcript).
`/wake` and
`/sleep` start and stop the model's systemd unit over ssh.

It can also search the web and fetch URLs when a question turns on facts it
doesn't have — three tool rounds per question, then it has to answer.
Searching shells out to `curl`, which must be on the PATH of whatever machine
runs the client.

`chat_endpoint`, `chat_model`, `chat_ssh`, and `chat_service` are editable
right from the settings page (see below) — that's how you point zodiac at
your own model. See
[LOCAL_MODEL.md](LOCAL_MODEL.md) for a walkthrough. The rest live only in
`~/.config/zodiac/config.json`:

| key | default | what |
| --- | --- | --- |
| `chat_panel` | `true` | show the panel at all |
| `chat_endpoint` | *(empty — chat disabled until set)* | OpenAI-compatible endpoint, e.g. `http://mybox:8091` |
| `chat_model` | `qwen3.6-35b-a3b` | model name sent with each request |
| `chat_ssh` | *(empty — `/wake`/`/sleep` disabled)* | where `/wake` and `/sleep` ssh to, e.g. `me@mybox` |
| `chat_service` | *(empty)* | the systemd --user unit they start/stop, e.g. `llama-server` |
| `chat_width` | `40` | panel width in cells |
| `chat_search_url` | *(empty)* | Wikipedia when empty; a SearXNG base URL (needs `json` in `search.formats`) or `https://api.search.brave.com` otherwise |
| `chat_search_key` | *(empty)* | API key, for backends that want one |
| `pane_monitor` | `true` | the background summarizer and stuck-pane check (see below; inert until `monitor_endpoint` is set) |
| `monitor_endpoint` | *(empty — monitor disabled until set)* | OpenAI-compatible endpoint for the background model |
| `monitor_model` | *(empty)* | model name for monitor requests (llama-server ignores it) |
| `chat_act` | `false` | allow `prompt_pane`/`send_keys` — see below |

Blank means *off*, not a hidden default: with no `chat_endpoint` the panel
shows "not configured" and makes no network calls at all, and with no
`chat_ssh`/`chat_service` the wake/sleep spells simply aren't offered.

These were once named `wizard_*`; a config using the old names still loads
(each key kept an alias), and gets rewritten to the new ones the next time
settings are saved. Changes to the four connection settings apply on the
*next* zodiac launch (`Alt+Q` to detach, then `zodiac` to reattach) — the
chat worker connects once at startup, not live.

With `chat_act` on, the panel can offer to submit a prompt to a pane or
send raw keystrokes, but neither ever fires on its own: each call shows a
consent line in the transcript and waits for `y` or `n`. No timeout, no
auto-accept.

### Background summaries

Separately from the chat panel, the server runs a small model to write the
one-line summary under each pane's status and to flag panes that have been
"working" with an unchanged screen for three minutes. It never writes to a
pane — it produces a notification and a log line, nothing more.

Point it at any OpenAI-compatible endpoint with the `Monitor endpoint` /
`Monitor model` fields in `Ctrl+S` (or `monitor_endpoint` / `monitor_model`
in config.json) — a small CPU-class model is plenty. Until an endpoint is
set, the monitor and summaries are fully disabled; changes apply live, no
restart needed. A single llama-server can back both this and the chat panel,
though a separate small model keeps background ticks off your chat GPU.

## Settings

`Ctrl+S` opens the settings page: `↑`/`↓` select, `←`/`→` change with a live
preview, `Esc` or `Ctrl+S` closes. Settings persist to
`~/.config/zodiac/config.json` and apply to every session. The page also
carries a keybinding reference in its right column.

| Setting | What |
| --- | --- |
| Working animation | Sidebar indicator style — `equalizer` (default) plus 16 from [FGRibreau/spinners](https://github.com/FGRibreau/spinners) |
| Spinner / Shimmer color | Indicator color and the band that sweeps a working pane's name |
| Shimmer speed | slow, normal, fast, zippy |
| Focus eye | The blinking marker on the focused pane — eye, dot, star, heart, diamond, pulse, flower, note, arrow |
| Sidebar frame / weight / color | Separator, surround or rounded; normal, thick or double; any palette color |
| Home view | cards, list, or blocks |
| Card size / Cards per row | Card dimensions; `auto` or a fixed 1–6 columns |
| Separator color | Line color in the blocks view |
| Card icon | Emblem size on painted cards |
| Card outline | double, single, or none |
| Select color / weight / style | The selected card's ring — palette color, thin→heavy, `glow` or `ring` |
| Card numeral | roman, arabic, or zodiac signs (♈ ♉ ♊ …) |
| Claude style | Mascot body shape: `hard` or `soft` |
| Finish sound | Ringtone on working → finished, from `~/.config/zodiac/ringtones/` (mp3/m4a/m4r/aac/wav/ogg/opus/flac/aiff/caf, picked up live); stepping the list previews each |
| Conn-error resume | Immediate `--resume` on "Connection closed mid-response" |
| Cursor type / blink / color | Follow the inner app or force block/underline/bar; blink on/off; focused-pane tint |
| Bottom controls | Hide the keybinding hints in the status bar |
| Theme | `night` (navy + brass), `oled-orange`, or `oled-green` (true-black variants) |
| Chat character | Who answers in the chat panel: a plain `assistant`, an ascii `oracle`, or `hal` |
| Chat endpoint / model / ssh / service | Free-text fields (`Enter` to edit, `Enter` again to save, `Esc` to cancel) pointing the chat panel at your own OpenAI-compatible server — see [LOCAL_MODEL.md](LOCAL_MODEL.md) |
| Monitor endpoint / model | Same free-text mechanics for the background summarizer/monitor; blank keeps it off, changes apply live |

## Mouse

zodiac owns the mouse, so a selection can never accidentally include the
sidebar.

- **Drag to select, release to copy** — to the clipboard via OSC 52 (works
  over ssh and `--remote`) plus `wl-copy` when available. The status bar
  flashes `· copied`; any keystroke or click clears the highlight.
- **Click a sidebar row** to focus that pane.
- **Wheel** scrolls zodiac's scrollback in shell panes; in fullscreen apps it
  becomes arrow keys, the usual alternate-scroll convention.
- Apps that ask for mouse reporting (vim, htop, agent TUIs) get the events
  forwarded instead — hold **Shift** while dragging to select there anyway.

## Graphics and terminal queries

Panes are graphics terminals. zodiac implements the kitty graphics protocol
inside every pane — transmit/place/delete, chunking, PNG and raw formats,
file/tmp/shm media, queries — and composites images through the outer
terminal when it speaks the protocol too (ghostty, kitty, wezterm). Images
scroll with their text, live in scrollback, and survive detach/reattach; each
pane's image state is isolated, so ids can't collide between panes. Pane PTYs
report true pixel dimensions, so `icat`, matplotlib and yazi previews size
correctly. In a non-graphics terminal, apps cleanly detect "unsupported".
Animations and Unicode placeholders are declined with an error response.
Design notes: [GRAPHICS.md](GRAPHICS.md).

zodiac also answers the queries apps send on startup — DA1/DA2, DSR/CPR,
DECRQM, XTVERSION, window size, XTGETTCAP (as "not supported"), and OSC 10/11
color queries, which report white-on-black so theme-sniffing apps see a dark
terminal. Without replies, those apps sit in their probe timeouts, which is
why some TUIs used to take seconds to start inside a multiplexer.

## Limitations

- `Shift+PgUp`/`PgDn` only reaches zodiac if your terminal passes it through
  in the alternate screen (foot, kitty, alacritty all do).
- Scrollback is 10,000 lines per pane, in memory. Older lines are gone.
- Desktop notifications need `notify-send`, so macOS is silent for now.
- The background summarizer's endpoint is hardcoded in `src/monitor.rs`
  rather than configurable.
- Two claude panes in the *same* directory share a project folder, so
  model-based naming shows whichever session wrote most recently — right when
  they run the same model, a near-miss when they don't.

## License

MIT — see [LICENSE](LICENSE).
