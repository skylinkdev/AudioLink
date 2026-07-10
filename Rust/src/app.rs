use anyhow::Result;
use tokio_util::sync::CancellationToken;
use tracing::info;

pub const WEB_ADMIN_ADDR: &str = "127.0.0.1:19092";

pub async fn run(shutdown: CancellationToken) -> Result<()> {
    let mdns_shutdown = shutdown.child_token();
    let control_shutdown = shutdown.child_token();
    let web_shutdown = shutdown.child_token();

    let mdns_task = tokio::spawn(crate::mdns_discovery::run(mdns_shutdown));
    let control_task = tokio::spawn(crate::control_server::run(control_shutdown));
    let web_task = tokio::spawn(crate::web_admin::run(WEB_ADMIN_ADDR, web_shutdown));

    tokio::select! {
        result = mdns_task => result??,
        result = control_task => result??,
        result = web_task => result??,
        () = shutdown.cancelled() => {}
    }

    shutdown.cancel();
    info!("Audio Link service stopped");
    Ok(())
}
