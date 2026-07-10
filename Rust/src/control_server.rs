use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{Mutex, watch},
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::audio_capture::spawn_speaker_loopback_udp_pipeline;
use crate::audio_device_events::spawn_default_output_change_watcher;
use crate::audio_protocol::{CONTROL_PORT, FRAME_MS, PROTOCOL};

pub async fn run(shutdown: CancellationToken) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", CONTROL_PORT)).await?;
    let (device_change_tx, device_change_rx) = watch::channel(0_u64);
    let session = Arc::new(Mutex::new(AudioSession::default()));
    spawn_default_output_change_watcher(device_change_tx)?;
    info!("control TCP server listening on 0.0.0.0:{CONTROL_PORT}");

    loop {
        let (stream, addr) = tokio::select! {
            result = listener.accept() => result?,
            () = shutdown.cancelled() => {
                stop_udp_audio_session(session).await;
                info!("control TCP server stopped");
                return Ok(());
            }
        };
        info!("control TCP client connected: {addr}");
        let session = Arc::clone(&session);
        let device_change_rx = device_change_rx.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_client(stream, addr, session, device_change_rx).await {
                warn!("control TCP client {addr} failed: {err:?}");
            }
        });
    }
}

#[derive(Default)]
struct AudioSession {
    stop_tx: Option<watch::Sender<bool>>,
    target_addr: Option<SocketAddr>,
}

async fn handle_client(
    stream: TcpStream,
    client_addr: SocketAddr,
    session: Arc<Mutex<AudioSession>>,
    device_change_rx: watch::Receiver<u64>,
) -> Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let read = reader.read_line(&mut line).await?;
    if read == 0 {
        return Ok(());
    }

    let command = line.trim();
    if command == "START_UDP_PING" {
        let port = start_udp_ping_responder(client_addr.ip()).await?;
        let response = format!("OK udp_ping_port={port}\n");
        reader.get_mut().write_all(response.as_bytes()).await?;
        reader.get_mut().flush().await?;
        return Ok(());
    }

    if command == "SERVER_TIME" {
        let response = format!("OK server_unix_ms={}\n", current_unix_time_ms());
        reader.get_mut().write_all(response.as_bytes()).await?;
        reader.get_mut().flush().await?;
        return Ok(());
    }

    if command == "STATUS" {
        let response = server_status_response(session).await;
        reader.get_mut().write_all(response.as_bytes()).await?;
        reader.get_mut().flush().await?;
        return Ok(());
    }

    let response = match handle_command(command, client_addr, session, device_change_rx).await {
        Ok(state) => format!(
            "OK volume={} muted={} frame_ms={}\n",
            state.volume, state.muted as u8, FRAME_MS
        ),
        Err(err) => format!("ERR {err}\n"),
    };

    reader.get_mut().write_all(response.as_bytes()).await?;
    reader.get_mut().flush().await?;
    Ok(())
}

async fn server_status_response(session: Arc<Mutex<AudioSession>>) -> String {
    let audio = crate::audio_stats::snapshot();
    let session = session.lock().await;
    let target_addr = session
        .target_addr
        .map(|addr| addr.to_string())
        .unwrap_or_default();
    format!(
        "OK running=1 udp_session={} audio_active={} target_addr={} frame_ms={} protocol={}\n",
        session.stop_tx.is_some() as u8,
        audio.active as u8,
        target_addr,
        FRAME_MS,
        PROTOCOL
    )
}

async fn start_udp_ping_responder(client_ip: IpAddr) -> Result<u16> {
    let bind_addr = if client_ip.is_ipv4() {
        SocketAddr::from(([0, 0, 0, 0], 0))
    } else {
        SocketAddr::from(([0_u16; 8], 0))
    };
    let socket = UdpSocket::bind(bind_addr).await?;
    let port = socket.local_addr()?.port();

    tokio::spawn(async move {
        let mut buffer = [0_u8; 256];
        let deadline = Duration::from_secs(3);
        while let Ok(Ok((length, addr))) = timeout(deadline, socket.recv_from(&mut buffer)).await {
            if addr.ip() != client_ip {
                continue;
            }
            if length == 16 && &buffer[..4] == b"LAT1" {
                let server_receive_ms = current_unix_time_ms();
                buffer[8..16].copy_from_slice(&server_receive_ms.to_le_bytes());
                let server_send_ms = current_unix_time_ms();
                buffer[16..24].copy_from_slice(&server_send_ms.to_le_bytes());
                let _ = socket.send_to(&buffer[..24], addr).await;
            } else {
                let _ = socket.send_to(&buffer[..length], addr).await;
            }
        }
    });

    Ok(port)
}

async fn handle_command(
    command: &str,
    client_addr: SocketAddr,
    session: Arc<Mutex<AudioSession>>,
    device_change_rx: watch::Receiver<u64>,
) -> Result<AudioState> {
    let mut parts = command.split_whitespace();
    match parts.next().unwrap_or_default() {
        "GET" => platform::get_state(),
        "PING" => Ok(best_effort_audio_state()),
        "START_UDP" => {
            let udp_port = parts
                .next()
                .context("START_UDP requires a UDP port")?
                .parse::<u16>()
                .context("invalid UDP port")?;
            let target_addr = SocketAddr::new(client_addr.ip(), udp_port);
            start_udp_audio_session(session, target_addr, device_change_rx).await?;
            Ok(best_effort_audio_state())
        }
        "STOP_UDP" => {
            stop_udp_audio_session(session).await;
            Ok(best_effort_audio_state())
        }
        "SET_VOLUME" => {
            let volume = parts
                .next()
                .context("SET_VOLUME requires a 0-100 value")?
                .parse::<u8>()
                .context("invalid volume value")?;
            platform::set_volume(volume)?;
            platform::get_state()
        }
        "SET_MUTE" => {
            let muted = match parts.next().context("SET_MUTE requires 0 or 1")? {
                "0" => false,
                "1" => true,
                _ => bail!("invalid mute value"),
            };
            platform::set_mute(muted)?;
            platform::get_state()
        }
        "MEDIA_NEXT" => {
            platform::media_next()?;
            platform::get_state()
        }
        "MEDIA_PREVIOUS" => {
            platform::media_previous()?;
            platform::get_state()
        }
        "MEDIA_PLAY_PAUSE" => {
            platform::media_play_pause()?;
            platform::get_state()
        }
        _ => bail!("unknown command"),
    }
}

async fn start_udp_audio_session(
    session: Arc<Mutex<AudioSession>>,
    target_addr: SocketAddr,
    device_change_rx: watch::Receiver<u64>,
) -> Result<()> {
    let (stop_tx, stop_rx) = watch::channel(false);
    let pipeline_device_change_rx = device_change_rx.clone();

    spawn_speaker_loopback_udp_pipeline(
        target_addr,
        pipeline_device_change_rx,
        stop_rx,
    )?;

    let mut session = session.lock().await;
    if let Some(old_stop_tx) = session.stop_tx.take() {
        let _ = old_stop_tx.send(true);
    }

    session.target_addr = Some(target_addr);
    session.stop_tx = Some(stop_tx);
    info!("UDP audio session started for {target_addr}");
    Ok(())
}

async fn stop_udp_audio_session(session: Arc<Mutex<AudioSession>>) {
    let mut session = session.lock().await;
    if let Some(stop_tx) = session.stop_tx.take() {
        let _ = stop_tx.send(true);
    }
    if let Some(target_addr) = session.target_addr.take() {
        info!("UDP audio session stopped for {target_addr}");
    }
}

fn best_effort_audio_state() -> AudioState {
    platform::get_state().unwrap_or(AudioState {
        volume: 0,
        muted: false,
    })
}

fn current_unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug)]
struct AudioState {
    volume: u8,
    muted: bool,
}

#[cfg(windows)]
mod platform {
    use super::AudioState;
    use anyhow::{Context, Result};
    use windows::Win32::{
        Media::Audio::Endpoints::IAudioEndpointVolume,
        Media::Audio::{ERole, IMMDeviceEnumerator, MMDeviceEnumerator, eMultimedia, eRender},
        System::Com::{
            CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
        },
        UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
            VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK,
        },
    };

    pub fn get_state() -> Result<AudioState> {
        with_endpoint_volume(|endpoint| unsafe {
            let scalar = endpoint
                .GetMasterVolumeLevelScalar()
                .context("failed to get master volume")?;
            let muted = endpoint.GetMute().context("failed to get mute state")?;
            Ok(AudioState {
                volume: (scalar.clamp(0.0, 1.0) * 100.0).round() as u8,
                muted: muted.as_bool(),
            })
        })
    }

    pub fn set_volume(volume: u8) -> Result<()> {
        let scalar = (volume.min(100) as f32) / 100.0;
        with_endpoint_volume(|endpoint| unsafe {
            endpoint
                .SetMasterVolumeLevelScalar(scalar, std::ptr::null())
                .context("failed to set master volume")
        })
    }

    pub fn set_mute(muted: bool) -> Result<()> {
        with_endpoint_volume(|endpoint| unsafe {
            endpoint
                .SetMute(muted, std::ptr::null())
                .context("failed to set mute state")
        })
    }

    pub fn media_next() -> Result<()> {
        send_media_key(VK_MEDIA_NEXT_TRACK)
    }

    pub fn media_previous() -> Result<()> {
        send_media_key(VK_MEDIA_PREV_TRACK)
    }

    pub fn media_play_pause() -> Result<()> {
        send_media_key(VK_MEDIA_PLAY_PAUSE)
    }

    fn with_endpoint_volume<T>(f: impl FnOnce(&IAudioEndpointVolume) -> Result<T>) -> Result<T> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .context("failed to initialize COM")?;
        }
        let _com = ComGuard;

        let enumerator: IMMDeviceEnumerator = unsafe {
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .context("failed to create MMDeviceEnumerator")?
        };
        let device = unsafe {
            enumerator
                .GetDefaultAudioEndpoint(eRender, ERole(eMultimedia.0))
                .context("failed to get default audio endpoint")?
        };
        let endpoint: IAudioEndpointVolume = unsafe {
            device
                .Activate(CLSCTX_ALL, None)
                .context("failed to activate endpoint volume")?
        };

        f(&endpoint)
    }

    fn send_media_key(key: VIRTUAL_KEY) -> Result<()> {
        let inputs = [
            keyboard_input(key, Default::default()),
            keyboard_input(key, KEYEVENTF_KEYUP),
        ];
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent == inputs.len() as u32 {
            Ok(())
        } else {
            anyhow::bail!("failed to send media key")
        }
    }

    fn keyboard_input(
        key: VIRTUAL_KEY,
        flags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS,
    ) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: key,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    struct ComGuard;

    impl Drop for ComGuard {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::AudioState;
    use anyhow::{Result, bail};

    pub fn get_state() -> Result<AudioState> {
        bail!("computer volume control is only supported on Windows")
    }

    pub fn set_volume(_volume: u8) -> Result<()> {
        bail!("computer volume control is only supported on Windows")
    }

    pub fn set_mute(_muted: bool) -> Result<()> {
        bail!("computer volume control is only supported on Windows")
    }

    pub fn media_next() -> Result<()> {
        bail!("media keys are only supported on Windows")
    }

    pub fn media_previous() -> Result<()> {
        bail!("media keys are only supported on Windows")
    }

    pub fn media_play_pause() -> Result<()> {
        bail!("media keys are only supported on Windows")
    }
}
