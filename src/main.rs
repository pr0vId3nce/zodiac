mod cli;
mod familiar;
mod gfx;
mod client;
mod kitty;
mod pane;
mod protocol;
mod query;
mod server;
mod settings;
mod snapshot;
mod term;
mod wizard;

use anyhow::{Context, Result};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};

fn main() -> Result<()> {
    migrate_legacy_dirs();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--server") => {
            let session = args.get(1).cloned().unwrap_or_else(|| "main".into());
            return server::run(&session);
        }
        Some("--remote") => return remote(&args[1..]),
        Some("-h") | Some("--help") => {
            print_help();
            return Ok(());
        }
        Some("-V") | Some("--version") => {
            println!("zodiac {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(c) if cli::COMMANDS.contains(&c) => return cli::run(args),
        // `zodiac -s <session> <cmd> ...` — session flag before the command.
        Some("-s") | Some("--session")
            if args
                .get(2)
                .is_some_and(|c| cli::COMMANDS.contains(&c.as_str())) =>
        {
            return cli::run(args)
        }
        _ => {}
    }
    let session = args.first().cloned().unwrap_or_else(|| "main".into());

    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableBracketedPaste, EnableMouseCapture);
    let res = client::run(&session, &mut terminal);
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    match res {
        Ok(msg) => {
            println!("{msg}");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn remote(rest: &[String]) -> Result<()> {
    let host = rest
        .first()
        .context("usage: zodiac --remote <ssh-host> [session]")?;
    let session = rest.get(1).map(String::as_str).unwrap_or("main");
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new("ssh")
        .args(["-t", host, "zodiac", session])
        .exec();
    Err(err).context("failed to exec ssh")
}

/// One-time migration from the old on-disk name (`coop`): rename the config
/// and state dirs to `zodiac` when the old ones exist and the new ones don't.
/// Sockets are ephemeral and need no migration — but a server started under
/// the old name keeps its old socket, so shut old sessions down first.
fn migrate_legacy_dirs() {
    use std::path::PathBuf;
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state"));
    for base in [config, state] {
        let (old, new) = (base.join("coop"), base.join("zodiac"));
        if old.is_dir() && !new.exists() {
            let _ = std::fs::rename(&old, &new);
        }
    }
}

fn print_help() {
    println!(
        "zodiac — TUI agent multiplexer

usage:
  zodiac [session]                     attach UI (default session: main)
  zodiac --remote <ssh-host> [session] attach to a session on another machine
  zodiac --server <session>            run the session server (internal)

commands (against a running server; -s <session>, --json where noted):
  zodiac ls [--json]                   list panes with agent status
  zodiac read <pane>                   print a pane's rendered screen
  zodiac send <pane> <text> [--enter]  type text into a pane
  zodiac prompt <pane> <text>          submit text + Enter (agent prompt)
  zodiac rename <pane> <name>          rename a pane
  zodiac focus <pane>                  focus a pane
  zodiac new                           open a new pane
  zodiac close <pane>                  close a pane (kills its process)
  zodiac wait <pane> [--state s1,s2] [--timeout secs]
                                     block until pane reaches a state
                                     (states: working idle done needs_input)
  zodiac autoresume <pane> on|off      toggle the API-stall watchdog (default on:
                                     a claude pane stuck on \"Response stalled
                                     mid-stream\" / \"Waiting for API response\"
                                     gets Esc + --resume automatically)
  zodiac restore                       re-launch the agents from the last
                                     snapshot (claude resumes its chat)
  zodiac kill-server                   shut the session down"
    );
}
