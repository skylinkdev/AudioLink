#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod audio_capture;
mod audio_device_events;
mod audio_engine_period;
mod audio_protocol;
mod audio_thread_priority;
mod audio_stats;
mod control_server;
mod mdns_discovery;
mod network_events;
mod tray;
mod web_admin;

use anyhow::Result;
use tracing_subscriber::{EnvFilter, fmt};

fn main() -> Result<()> {
    fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    tray::run()
}
