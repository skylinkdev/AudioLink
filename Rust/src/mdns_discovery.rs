use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::audio_protocol::{CHANNELS, CONTROL_PORT, FORMAT, FRAME_MS, PROTOCOL, SAMPLE_RATE};

const SERVICE_TYPE: &str = "_lan-audio._tcp.local.";
const INSTANCE_NAME: &str = "Audio Link";
const HOST_NAME: &str = "lan-audio.local.";
const NETWORK_CHANGE_DEBOUNCE: Duration = Duration::from_secs(1);

pub fn publish() -> Result<ServiceDaemon> {
    let mdns = ServiceDaemon::new().context("failed to create mDNS daemon")?;
    mdns.set_ip_check_interval(0)
        .context("failed to disable mDNS IP polling")?;
    mdns.disable_interface(IfKind::IPv6)
        .context("failed to disable IPv6 mDNS interfaces")?;

    let properties = HashMap::from([
        ("format".to_string(), FORMAT.to_string()),
        ("sample_rate".to_string(), SAMPLE_RATE.to_string()),
        ("channels".to_string(), CHANNELS.to_string()),
        ("frame_ms".to_string(), FRAME_MS.to_string()),
        ("protocol".to_string(), PROTOCOL.to_string()),
        ("control_port".to_string(), CONTROL_PORT.to_string()),
        ("audio_transport".to_string(), "udp-client-port".to_string()),
    ]);

    let mut service = ServiceInfo::new(
        SERVICE_TYPE,
        INSTANCE_NAME,
        HOST_NAME,
        (),
        CONTROL_PORT,
        properties,
    )
    .context("failed to create mDNS service info")?
    .enable_addr_auto();
    service.set_interfaces(vec![IfKind::IPv4]);

    mdns.register(service)
        .context("failed to register mDNS service")?;
    info!("mDNS service published: {INSTANCE_NAME}.{SERVICE_TYPE} on port {CONTROL_PORT}");

    Ok(mdns)
}

pub async fn run(shutdown: CancellationToken) -> Result<()> {
    let mut mdns = publish()?;
    let (network_change_tx, mut network_change_rx) = watch::channel(0_u64);
    let _network_watcher = crate::network_events::watch_network_changes(network_change_tx)?;

    loop {
        tokio::select! {
            () = shutdown.cancelled() => {
                shutdown_mdns(mdns);
                info!("mDNS publisher stopped");
                return Ok(());
            }
            changed = network_change_rx.changed() => {
                if changed.is_err() {
                    warn!("network change watcher stopped");
                    continue;
                }

                tokio::time::sleep(NETWORK_CHANGE_DEBOUNCE).await;
                info!("network changed; refreshing mDNS publisher");
                shutdown_mdns(mdns);
                mdns = publish()?;
            }
        }
    }
}

fn shutdown_mdns(mdns: ServiceDaemon) {
    match mdns.shutdown() {
        Ok(status_rx) => {
            if let Err(err) = status_rx.recv_timeout(Duration::from_secs(2)) {
                warn!("timed out waiting for mDNS daemon shutdown: {err:?}");
            }
        }
        Err(err) => warn!("failed to shutdown mDNS daemon: {err:?}"),
    }
}
