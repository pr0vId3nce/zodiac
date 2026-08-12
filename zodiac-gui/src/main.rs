//! zodiac-gui: the Phase 3 GUI client (roadmap 3.3–3.6). A third client on
//! the session socket: it attaches with the gfx-capable T_ATTACH payload,
//! mirrors panes through `zodiac::client_core::CPane` exactly like the TUI,
//! and renders the active pane with wgpu + glyphon per ADR 0004 — including
//! zodiac's first actual pixel decode of the kitty image mirror.

// Transitional during the ADR-0006 native rebuild: the legacy grid renderer
// (`render::Renderer::render` and its tab/settings/grid/agent builders) is
// parked while egui owns the screens. It is reintroduced verbatim as the
// per-pane *terminal mode* (task #26) and the grouped settings page (task
// #28); until then those paths are unreferenced. Removed once terminal mode
// re-consumes them.
#![allow(dead_code)]

mod agents;
mod anim;
mod app;
mod font;
mod img;
mod keys;
#[cfg(target_os = "macos")]
mod macos_menu;
mod palette;
mod placeholder;
mod render;
mod slash;
mod theme;
mod ui;

use app::{GuiApp, UserEvent};
use zodiac::protocol::{read_frame, write_frame, T_ATTACH};

/// Locate the `zodiac` server binary: the sibling of this executable when
/// present (the install + dev layout both place `zodiac` next to
/// `zodiac-gui`), else `zodiac` on `PATH`. `zodiac-gui` is a client only and
/// cannot serve, so it must start the real server binary rather than itself.
fn server_binary() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("zodiac")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| std::path::PathBuf::from("zodiac"))
}

fn main() -> anyhow::Result<()> {
    let arg = std::env::args().nth(1);
    if matches!(arg.as_deref(), Some("-h") | Some("--help")) {
        println!(
            "usage: zodiac-gui [session]\n\n\
             Attaches to the zodiac session (default: main), starting the\n\
             server if needed. Keys go to the active pane; Ctrl+PageUp /\n\
             Ctrl+PageDown switch panes; click a tab to focus it.\n\n\
             env: ZODIAC_GUI_FONT=<family>  ZODIAC_GUI_EXIT_AFTER_MS=<ms>"
        );
        return Ok(());
    }
    let session = arg.unwrap_or_else(|| "main".into());

    // Font first: the attach payload carries the cell size in px so the
    // server's PTYs can report pixel dimensions to inner apps.
    let mut fonts = font::Fonts::load();
    let (cw, ch) = fonts.cell_size(1.0);
    let mut sock = zodiac::client_core::connect_or_spawn_with(&session, server_binary())?;
    let (cw16, ch16) = (cw.round() as u16, ch.round() as u16);
    let hello = [
        1u8, // gfx capable
        cw16.to_le_bytes()[0],
        cw16.to_le_bytes()[1],
        ch16.to_le_bytes()[0],
        ch16.to_le_bytes()[1],
    ];
    write_frame(&mut sock, T_ATTACH, 0, &hello)?;

    let event_loop = winit::event_loop::EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    {
        let mut rd = sock.try_clone()?;
        std::thread::spawn(move || loop {
            match read_frame(&mut rd) {
                Ok(f) => {
                    if proxy.send_event(UserEvent::Srv(f)).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    let _ = proxy.send_event(UserEvent::SrvGone);
                    break;
                }
            }
        });
    }

    let mut app = GuiApp::new(session, sock, fonts);
    event_loop.run_app(&mut app)?;
    if let Some(msg) = app.exit_msg.take() {
        println!("{msg}");
    }
    // The e2e harness reports through the process exit code so CI (and a
    // human running it) can't miss a failure buried in the log.
    if app.e2e_failed {
        std::process::exit(1);
    }
    Ok(())
}
