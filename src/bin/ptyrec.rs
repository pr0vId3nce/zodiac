//! Record a command's exact PTY byte stream into a .ptyrec corpus file.
//!
//!   ptyrec -o tests/corpus/vim.ptyrec [--rows 50] [--cols 120] -- vim file
//!
//! The session is interactive: the child's output is mirrored to this
//! terminal and stdin is forwarded, so a human (or a piped script) can
//! drive it. Only child output and resizes are recorded — input is not,
//! because the goldens replay what a pane *received*, not what was typed.
//! The recording size is fixed for the whole session (no SIGWINCH
//! handling); the resize chunk kind exists for future use.

#[path = "../corpus.rs"]
mod corpus;

use std::io::{Read, Write};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("ptyrec: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let mut out_path = None;
    let mut rows: u16 = 50;
    let mut cols: u16 = 120;
    let mut cmd_args: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-o" => out_path = Some(args.next().ok_or("-o needs a path")?),
            "--rows" => rows = args.next().ok_or("--rows needs a value")?.parse()?,
            "--cols" => cols = args.next().ok_or("--cols needs a value")?.parse()?,
            "--" => {
                cmd_args = args.collect();
                break;
            }
            "-h" | "--help" => {
                println!("usage: ptyrec -o FILE [--rows N] [--cols N] -- CMD [ARGS...]");
                return Ok(ExitCode::SUCCESS);
            }
            other => return Err(format!("unknown argument {other:?}").into()),
        }
    }
    let out_path = out_path.ok_or("missing -o FILE")?;
    if cmd_args.is_empty() {
        cmd_args = vec![std::env::var("SHELL").unwrap_or_else(|_| "sh".into())];
    }

    let file = std::io::BufWriter::new(std::fs::File::create(&out_path)?);
    let rec = Arc::new(Mutex::new(corpus::Writer::new(file, rows, cols)?));

    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(&cmd_args[0]);
    cmd.args(&cmd_args[1..]);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    // Mirror + record child output.
    let mut reader = pair.master.try_clone_reader()?;
    let rec_out = Arc::clone(&rec);
    let reader_thread = std::thread::spawn(move || {
        let mut stdout = std::io::stdout();
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            let _ = stdout.write_all(&buf[..n]);
            let _ = stdout.flush();
            if rec_out.lock().unwrap().output(&buf[..n]).is_err() {
                break;
            }
        }
    });

    // Forward our stdin to the child. Raw mode only when stdin is a tty —
    // piped scripts drive the child without it.
    let raw = crossterm::terminal::enable_raw_mode().is_ok();
    let mut writer = pair.master.take_writer()?;
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 1024];
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || writer.write_all(&buf[..n]).is_err() {
                break;
            }
        }
    });

    let status = child.wait()?;
    // Give the reader a moment to drain the final output burst.
    std::thread::sleep(std::time::Duration::from_millis(200));
    drop(pair.master);
    let _ = reader_thread.join();
    rec.lock().unwrap().flush()?;
    if raw {
        let _ = crossterm::terminal::disable_raw_mode();
    }
    eprintln!("\r\nptyrec: wrote {out_path}");
    // The stdin thread may still be blocked on read; exiting reaps it.
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
