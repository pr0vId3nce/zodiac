use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};

use crate::protocol::*;

pub const COMMANDS: &[&str] = &[
    "ls",
    "list",
    "read",
    "send",
    "prompt",
    "rename",
    "focus",
    "new",
    "perm",
    "close",
    "wait",
    "autoresume",
    "restore",
    "kill-server",
];

pub fn run(mut args: Vec<String>) -> Result<()> {
    let mut session = "main".to_string();
    if let Some(i) = args.iter().position(|a| a == "-s" || a == "--session") {
        if i + 1 >= args.len() {
            bail!("{} needs a value", args[i]);
        }
        session = args.remove(i + 1);
        args.remove(i);
    }
    let json = if let Some(i) = args.iter().position(|a| a == "--json") {
        args.remove(i);
        true
    } else {
        false
    };
    let cmd = args.remove(0);

    let mut sock = connect(&session)?;
    match cmd.as_str() {
        "ls" | "list" => {
            let st = query(&mut sock)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&st)?);
            } else {
                println!(
                    "session '{}' — {}",
                    st.session,
                    if st.attached { "attached" } else { "detached" }
                );
                for p in &st.panes {
                    println!(
                        "{:>3} {} {:<12} {:<10} {:<20} {}",
                        p.index,
                        if p.focused { "*" } else { " " },
                        p.status,
                        p.agent.as_deref().unwrap_or("-"),
                        p.name,
                        if p.title.is_empty() {
                            p.cwd.clone().unwrap_or_default()
                        } else {
                            p.title.clone()
                        }
                    );
                }
            }
        }
        "read" => {
            let id = resolve(&mut sock, &args, 0)?;
            write_frame(&mut sock, T_READ_SCREEN, id, &[])?;
            let f = expect(&mut sock, T_SCREEN)?;
            println!("{}", String::from_utf8_lossy(&f.data));
        }
        "send" => {
            let id = resolve(&mut sock, &args, 0)?;
            let mut rest: Vec<String> = args[1..].to_vec();
            let enter = match rest.iter().position(|a| a == "--enter") {
                Some(i) => {
                    rest.remove(i);
                    true
                }
                None => false,
            };
            let text = join_text(&rest);
            if text.is_empty() && !enter {
                bail!("usage: zodiac send <pane> <text...> [--enter]");
            }
            write_frame(&mut sock, T_INPUT, id, text.as_bytes())?;
            if enter {
                std::thread::sleep(Duration::from_millis(150));
                write_frame(&mut sock, T_INPUT, id, b"\r")?;
            }
        }
        "prompt" => {
            let id = resolve(&mut sock, &args, 0)?;
            let text = join_text(&args[1..]);
            if text.is_empty() {
                bail!("usage: zodiac prompt <pane> <text...>");
            }
            // Agent panes take structured prompts; pty panes get the text
            // typed + Enter, as ever.
            let st = query(&mut sock)?;
            let is_agent = st.panes.iter().any(|p| p.id == id && p.kind == "agent");
            if is_agent {
                write_frame(&mut sock, T_AGENT_INPUT, id, text.as_bytes())?;
            } else {
                write_frame(&mut sock, T_INPUT, id, text.as_bytes())?;
                std::thread::sleep(Duration::from_millis(200));
                write_frame(&mut sock, T_INPUT, id, b"\r")?;
            }
        }
        "perm" => {
            // `zodiac perm <pane> allow|deny [message...]` — answer the
            // pane's oldest pending permission request (agent panes).
            let id = resolve(&mut sock, &args, 0)?;
            let behavior = args.get(1).map(String::as_str).unwrap_or("");
            if behavior != "allow" && behavior != "deny" {
                bail!("usage: zodiac perm <pane> allow|deny [message...]");
            }
            let msg = join_text(&args[2..]);
            let payload = serde_json::json!({
                "request_id": "",
                "behavior": behavior,
                "message": if msg.is_empty() { None } else { Some(msg) },
            });
            write_frame(&mut sock, T_PERM_RESP, id, payload.to_string().as_bytes())?;
        }
        "rename" => {
            let id = resolve(&mut sock, &args, 0)?;
            let name = join_text(&args[1..]);
            if name.is_empty() {
                bail!("usage: zodiac rename <pane> <name>");
            }
            write_frame(&mut sock, T_RENAME, id, name.as_bytes())?;
        }
        "focus" => {
            let id = resolve(&mut sock, &args, 0)?;
            write_frame(&mut sock, T_FOCUS, id, &[])?;
        }
        "close" => {
            let id = resolve(&mut sock, &args, 0)?;
            write_frame(&mut sock, T_CLOSE_PANE, id, &[])?;
        }
        "new" => {
            // `zodiac new [--agent claude|pi] [--cwd DIR]` — with --agent,
            // a structured agent pane (ADR 0002) instead of a shell.
            let payload = match flag_value(&args, "--agent") {
                Some(agent) => serde_json::json!({
                    "kind": "agent",
                    "agent": agent,
                    "cwd": flag_value(&args, "--cwd"),
                })
                .to_string()
                .into_bytes(),
                None => Vec::new(),
            };
            write_frame(&mut sock, T_NEW_PANE, 0, &payload)?;
            let st = query(&mut sock)?;
            if let Some(p) = st.panes.last() {
                println!("pane {} ({})", p.index, p.name);
            }
        }
        "wait" => {
            let wanted: Vec<String> = flag_value(&args, "--state")
                .unwrap_or_else(|| "idle,done,needs_input".into())
                .split(',')
                .map(|s| s.trim().to_string())
                .collect();
            let timeout = flag_value(&args, "--timeout")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(600);
            let idx = pane_index(&args, 0)?;
            let start = Instant::now();
            loop {
                let st = query(&mut sock)?;
                let p = st
                    .panes
                    .get(idx - 1)
                    .ok_or_else(|| anyhow!("pane {idx} is gone"))?;
                if wanted.iter().any(|w| w == &p.status) {
                    println!("{}", p.status);
                    return Ok(());
                }
                if start.elapsed() > Duration::from_secs(timeout) {
                    bail!("timeout: pane {idx} still '{}'", p.status);
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        "autoresume" => {
            let id = resolve(&mut sock, &args, 0)?;
            let on = match args.get(1).map(String::as_str) {
                Some("on") => 1u8,
                Some("off") => 0u8,
                _ => bail!("usage: zodiac autoresume <pane> on|off"),
            };
            write_frame(&mut sock, T_AUTORESUME, id, &[on])?;
        }
        "restore" => {
            write_frame(&mut sock, T_RESTORE, 0, &[])?;
        }
        "kill-server" => {
            write_frame(&mut sock, T_SHUTDOWN, 0, &[])?;
        }
        other => bail!("unknown command '{other}'"),
    }
    Ok(())
}

fn connect(session: &str) -> Result<UnixStream> {
    UnixStream::connect(socket_path(session))
        .map_err(|_| anyhow!("no zodiac server running for session '{session}'"))
}

fn query(sock: &mut UnixStream) -> Result<SessionState> {
    write_frame(sock, T_QUERY, 0, &[])?;
    let f = expect(sock, T_STATE)?;
    Ok(serde_json::from_slice(&f.data)?)
}

fn expect(sock: &mut UnixStream, typ: u8) -> Result<Frame> {
    loop {
        let f = read_frame(sock)?;
        if f.typ == typ {
            return Ok(f);
        }
    }
}

fn pane_index(args: &[String], pos: usize) -> Result<usize> {
    let n: usize = args
        .get(pos)
        .filter(|a| !a.starts_with("--"))
        .ok_or_else(|| anyhow!("missing pane number"))?
        .parse()
        .map_err(|_| anyhow!("pane must be a number (see `zodiac ls`)"))?;
    if n == 0 {
        bail!("panes are numbered from 1");
    }
    Ok(n)
}

fn resolve(sock: &mut UnixStream, args: &[String], pos: usize) -> Result<u64> {
    let n = pane_index(args, pos)?;
    let st = query(sock)?;
    st.panes
        .get(n - 1)
        .map(|p| p.id)
        .ok_or_else(|| anyhow!("no pane {n} (session has {})", st.panes.len()))
}

/// Everything the user typed, verbatim — text destined for a pane must keep
/// its `--flags` (`claude --resume <id>` is a command, not zodiac options).
/// Commands with their own flags strip them before calling this.
fn join_text(args: &[String]) -> String {
    args.join(" ")
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}
