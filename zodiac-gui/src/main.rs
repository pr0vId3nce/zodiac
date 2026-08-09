//! zodiac-gui: the Phase 3 GUI client (roadmap 3.3–3.6). This stub pins
//! the workspace seam: it links the zodiac library (client_core,
//! protocol) — the wgpu grid renderer lands on top of it.

fn main() -> anyhow::Result<()> {
    let session = std::env::args().nth(1).unwrap_or_else(|| "main".into());
    // Prove the seam: resolve the session socket the same way the TUI does.
    let path = zodiac::protocol::socket_path(&session);
    println!(
        "zodiac-gui (Phase 3 stub): would attach to {} — the wgpu grid \
         renderer is the next roadmap task",
        path.display()
    );
    Ok(())
}
