//! zodiac as a library: the server, the TUI client, and the shared
//! client_core seam the GUI client consumes (roadmap Phase 3, task 3.2).
//! The binaries (`zodiac`, `ptyrec`) are thin wrappers over these modules.

pub mod agent;
pub mod chat;
pub mod cli;
pub mod client;
pub mod client_core;
pub mod corpus;
pub mod engine;
pub mod gfx;
pub mod kitty;
pub mod monitor;
pub mod pane;
pub mod protocol;
pub mod query;
pub mod server;
pub mod settings;
pub mod snapshot;
pub mod term;
pub mod theme;
