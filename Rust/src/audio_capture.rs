use std::{
    net::{SocketAddr, UdpSocket},
    sync::{
        Arc,
        OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc as std_mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use crossbeam_queue::ArrayQueue;
use rubato::{Fft, FixedSync, Resampler, audioadapter_buffers::owned::InterleavedOwned};
use tokio::sync::{oneshot, watch};
use tracing::{info, warn};

use crate::audio_protocol::{
    CHANNELS, FRAME_MS, MAX_FRAME_BYTES, MAX_FRAME_SAMPLES, MAX_FRAME_SAMPLES_PER_CHANNEL,
    SAMPLE_RATE, UDP_PACKET_FEC_CODEC_NONE, UDP_PACKET_FEC_CODEC_PCM_S16LE_STEREO,
    UDP_PACKET_FEC_ENABLED, UDP_PACKET_HEADER_BYTES, UDP_PACKET_MAGIC, UDP_PACKET_SEND_MULTIPLIER,
    UDP_PACKET_VERSION,
};

// PCM 队列容量。队列满时会丢弃旧数据，保证发送端总是能获取到最新的音频数据。
const PCM_QUEUE_CAPACITY: usize = 32;

enum AudioChunk {
    I16(Vec<i16>),
    F32(Vec<f32>),
}

struct LoopbackStreams {
    stop: Arc<AtomicBool>,
    capture_thread: Option<thread::JoinHandle<()>>,
}

impl Drop for LoopbackStreams {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(capture_thread) = self.capture_thread.take() {
            capture_thread.thread().unpark();
            let _ = capture_thread.join();
        }
    }
}

struct PcmPipe {
    queue: ArrayQueue<AudioChunk>,
    producer_alive: AtomicBool,
    sender_thread: OnceLock<thread::Thread>,
}

type PcmPipeHandle = Arc<PcmPipe>;

impl PcmPipe {
    fn new() -> Self {
        Self {
            queue: ArrayQueue::new(PCM_QUEUE_CAPACITY),
            producer_alive: AtomicBool::new(true),
            sender_thread: OnceLock::new(),
        }
    }
}

fn push_latest_pcm(pipe: &PcmPipeHandle, mut chunk: AudioChunk) {
    loop {
        match pipe.queue.push(chunk) {
            Ok(()) => {
                if let Some(sender_thread) = pipe.sender_thread.get() {
                    sender_thread.unpark();
                }
                return;
            }
            Err(returned_chunk) => {
                chunk = returned_chunk;
                let _ = pipe.queue.pop();
            }
        }
    }
}

pub fn spawn_speaker_loopback_udp_pipeline(
    target_addr: SocketAddr,
    mut device_change_rx: watch::Receiver<u64>,
    mut stop_rx: watch::Receiver<bool>,
) -> Result<()> {
    let pcm_pipe = Arc::new(PcmPipe::new());
    let (restart_tx, restart_rx) = std_mpsc::channel::<()>();
    spawn_capture_control_thread(pcm_pipe.clone(), restart_rx)?;
    let (send_done_tx, mut send_done_rx) = oneshot::channel::<Result<()>>();

    thread::Builder::new()
        .name("speaker-loopback-udp-send".to_string())
        .spawn(move || {
            crate::audio_thread_priority::boost_current_audio_thread("UDP audio send");
            let _ = pcm_pipe.sender_thread.set(thread::current());
            let result = send_pcm_udp(pcm_pipe, target_addr);
            let _ = send_done_tx.send(result);
        })
        .context("failed to spawn UDP audio send thread")?;

    tokio::spawn(async move {
        loop {
            tokio::select! {
                result = &mut send_done_rx => {
                    if let Ok(Err(err)) = result {
                        warn!("udp audio pipeline stopped for {target_addr}: {err:?}");
                    }
                    break;
                }
                changed = device_change_rx.changed() => {
                    if changed.is_err() || restart_tx.send(()).is_err() {
                        break;
                    }
                }
                changed = stop_rx.changed() => {
                    if changed.is_err() || *stop_rx.borrow() {
                        break;
                    }
                }
            }
        }

        crate::audio_stats::mark_udp_stream_inactive();
        drop(restart_tx);
    });

    Ok(())
}

fn spawn_capture_control_thread(
    pcm_pipe: PcmPipeHandle,
    restart_rx: std_mpsc::Receiver<()>,
) -> Result<()> {
    let (startup_tx, startup_rx) = std_mpsc::channel::<Result<()>>();

    thread::Builder::new()
        .name("speaker-loopback-capture".to_string())
        .spawn(move || {
            crate::audio_thread_priority::boost_current_audio_thread("speaker loopback capture");
            let mut capture_stream = Some(match spawn_speaker_loopback_stream(pcm_pipe.clone()) {
                Ok(stream) => {
                    let _ = startup_tx.send(Ok(()));
                    stream
                }
                Err(err) => {
                    let message = format!("{err:#}");
                    let _ = startup_tx.send(Err(err));
                    pcm_pipe.producer_alive.store(false, Ordering::Release);
                    if let Some(sender_thread) = pcm_pipe.sender_thread.get() {
                        sender_thread.unpark();
                    }
                    warn!("failed to start loopback capture: {message}");
                    return;
                }
            });

            while restart_rx.recv().is_ok() {
                info!("default output device changed; restarting loopback capture");
                capture_stream.take();
                match spawn_speaker_loopback_stream(pcm_pipe.clone()) {
                    Ok(stream) => {
                        capture_stream = Some(stream);
                        info!("loopback capture restarted");
                    }
                    Err(err) => {
                        warn!("failed to restart loopback capture: {err:?}");
                    }
                }
            }

            pcm_pipe.producer_alive.store(false, Ordering::Release);
            if let Some(sender_thread) = pcm_pipe.sender_thread.get() {
                sender_thread.unpark();
            }
        })
        .context("failed to spawn capture control thread")?;

    startup_rx
        .recv()
        .context("capture control thread stopped before startup")??;

    Ok(())
}

fn spawn_speaker_loopback_stream(pcm_pipe: PcmPipeHandle) -> Result<LoopbackStreams> {
    platform::spawn_speaker_loopback_stream(pcm_pipe)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WasapiSampleFormat {
    F32,
    I16,
    I24,
    I32,
}

impl WasapiSampleFormat {
    fn from_wave_format(format: &WaveFormat) -> Result<Self> {
        if format.is_float() && format.bits_per_sample == 32 {
            return Ok(Self::F32);
        }
        if format.is_pcm() {
            return match format.bits_per_sample {
                16 => Ok(Self::I16),
                24 => Ok(Self::I24),
                32 => Ok(Self::I32),
                bits => Err(anyhow!("Unsupported WASAPI PCM bit depth: {bits}")),
            };
        }
        Err(anyhow!(
            "Unsupported WASAPI mix format: tag={} bits={} subtype={:?}",
            format.format_tag,
            format.bits_per_sample,
            format.sub_format
        ))
    }
}

#[derive(Clone, Debug)]
struct WaveFormat {
    format_tag: u16,
    channels: u16,
    sample_rate: u32,
    block_align: u16,
    bits_per_sample: u16,
    sub_format: Option<windows_core::GUID>,
}

impl WaveFormat {
    fn is_float(&self) -> bool {
        platform::wave_format_is_float(self)
    }

    fn is_pcm(&self) -> bool {
        platform::wave_format_is_pcm(self)
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{ffi::c_void, ptr};
    use windows_core::{GUID, Interface};
    use windows::Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0},
        Media::{
            Audio::{
                AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                ERole, IAudioCaptureClient, IAudioClient, IAudioClient3, IAudioRenderClient,
                IMMDeviceEnumerator, MMDeviceEnumerator, WAVE_FORMAT_PCM, WAVEFORMATEX,
                WAVEFORMATEXTENSIBLE, eMultimedia, eRender,
            },
            KernelStreaming::{
                KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE,
            },
        },
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree, CoUninitialize,
            },
            Threading::{CreateEventW, INFINITE, WaitForMultipleObjectsEx},
        },
    };

    const REFTIMES_PER_SEC: i64 = 10_000_000;
    const WAVE_FORMAT_IEEE_FLOAT: u32 = 3;
    const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID =
        GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

    pub fn spawn_speaker_loopback_stream(pcm_pipe: PcmPipeHandle) -> Result<LoopbackStreams> {
        let stop = Arc::new(AtomicBool::new(false));
        let (startup_tx, startup_rx) = std_mpsc::channel::<Result<()>>();
        let thread_stop = stop.clone();

        let capture_thread = thread::Builder::new()
            .name("wasapi-loopback-capture".to_string())
            .spawn(move || {
                crate::audio_thread_priority::boost_current_audio_thread("native WASAPI loopback");
                let mut startup_tx = Some(startup_tx);
                let result = run_wasapi_loopback(pcm_pipe, thread_stop, &mut startup_tx);
                if let Err(err) = &result {
                    if let Some(startup_tx) = startup_tx.take() {
                        let _ = startup_tx.send(Err(anyhow!("{err:#}")));
                    }
                    warn!("native WASAPI loopback stopped: {err:?}");
                }
            })
            .context("failed to spawn native WASAPI loopback thread")?;

        match startup_rx.recv().context("WASAPI loopback thread stopped before startup")? {
            Ok(()) => Ok(LoopbackStreams {
                stop,
                capture_thread: Some(capture_thread),
            }),
            Err(err) => {
                let _ = capture_thread.join();
                Err(err)
            }
        }
    }

    pub fn wave_format_is_float(format: &WaveFormat) -> bool {
        format.format_tag as u32 == WAVE_FORMAT_IEEE_FLOAT
            || (format.format_tag as u32 == WAVE_FORMAT_EXTENSIBLE
                && format.sub_format == Some(KSDATAFORMAT_SUBTYPE_IEEE_FLOAT))
    }

    pub fn wave_format_is_pcm(format: &WaveFormat) -> bool {
        format.format_tag as u32 == WAVE_FORMAT_PCM
            || (format.format_tag as u32 == WAVE_FORMAT_EXTENSIBLE
                && format.sub_format == Some(KSDATAFORMAT_SUBTYPE_PCM))
    }

    fn run_wasapi_loopback(
        pcm_pipe: PcmPipeHandle,
        stop: Arc<AtomicBool>,
        startup_tx: &mut Option<std_mpsc::Sender<Result<()>>>,
    ) -> Result<()> {
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
                .context("failed to get default render endpoint")?
        };

        let capture_client: IAudioClient3 = unsafe {
            device
                .Activate(CLSCTX_ALL, None)
                .context("failed to activate capture IAudioClient3")?
        };
        let render_client: IAudioClient3 = unsafe {
            device
                .Activate(CLSCTX_ALL, None)
                .context("failed to activate render IAudioClient3")?
        };

        let mix_format_ptr = unsafe {
            capture_client
                .GetMixFormat()
                .context("failed to get WASAPI mix format")?
        };
        let _mix_format = CoTaskMemFormat(mix_format_ptr.cast());
        let format = unsafe { wave_format_from_ptr(mix_format_ptr)? };
        let sample_format = WasapiSampleFormat::from_wave_format(&format)?;
        let period_frames = shared_engine_period_frames(&capture_client, mix_format_ptr)?;

        info!(
            "using native WASAPI loopback: {:?}, {} Hz, {} ch, {} bits, {} frames/period",
            sample_format,
            format.sample_rate,
            format.channels,
            format.bits_per_sample,
            period_frames
        );

        initialize_capture_client(&capture_client, mix_format_ptr, period_frames, format.sample_rate)
            .context("failed to initialize WASAPI loopback capture")?;
        initialize_render_client(&render_client, mix_format_ptr, period_frames, format.sample_rate)
            .context("failed to initialize WASAPI silence clock")?;

        let capture_event = EventHandle::new().context("failed to create capture event")?;
        let render_event = EventHandle::new().context("failed to create render event")?;
        unsafe {
            capture_client
                .SetEventHandle(capture_event.0)
                .context("failed to set WASAPI capture event")?;
            render_client
                .SetEventHandle(render_event.0)
                .context("failed to set WASAPI render event")?;
        }

        let capture_service = unsafe {
            capture_client
                .GetService::<IAudioCaptureClient>()
                .context("failed to get IAudioCaptureClient")?
        };
        let render_service = unsafe {
            render_client
                .GetService::<IAudioRenderClient>()
                .context("failed to get IAudioRenderClient")?
        };

        prime_silence_render(&render_client, &render_service)?;
        unsafe {
            capture_client
                .Start()
                .context("failed to start WASAPI loopback capture")?;
            render_client
                .Start()
                .context("failed to start WASAPI silence clock")?;
        }
        if let Some(startup_tx) = startup_tx.take() {
            let _ = startup_tx.send(Ok(()));
        }
        let _capture_running = AudioClientStopGuard(&capture_client);
        let _render_running = AudioClientStopGuard(&render_client);

        let mut processor = CaptureProcessor::new(format.sample_rate, format.channels as usize);
        let handles = [capture_event.0, render_event.0];
        let _ = pcm_pipe.sender_thread.get();

        loop {
            if stop.load(Ordering::Acquire) {
                return Ok(());
            }

            let signaled = wait_for_event(&handles)?;
            if signaled == 0 {
                process_capture_packets(
                    &capture_service,
                    sample_format,
                    format.block_align as usize,
                    &mut processor,
                    &pcm_pipe,
                )?;
            } else {
                write_silence_if_needed(&render_client, &render_service)?;
            }
        }
    }

    fn initialize_capture_client(
        audio_client: &IAudioClient3,
        format: *const WAVEFORMATEX,
        period_frames: u32,
        sample_rate: u32,
    ) -> Result<()> {
        let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_LOOPBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;

        unsafe {
            match audio_client.InitializeSharedAudioStream(flags, period_frames, format, None) {
                Ok(()) => Ok(()),
                Err(err) => {
                    warn!(
                        "InitializeSharedAudioStream loopback failed ({err}); falling back to IAudioClient::Initialize"
                    );
                    let duration = frames_to_hns(period_frames, sample_rate);
                    let client: IAudioClient = audio_client.cast()?;
                    client.Initialize(
                        AUDCLNT_SHAREMODE_SHARED,
                        flags,
                        duration,
                        0,
                        format,
                        None,
                    )?;
                    Ok(())
                }
            }
        }
    }

    fn initialize_render_client(
        audio_client: &IAudioClient3,
        format: *const WAVEFORMATEX,
        period_frames: u32,
        sample_rate: u32,
    ) -> Result<()> {
        let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;

        unsafe {
            match audio_client.InitializeSharedAudioStream(flags, period_frames, format, None) {
                Ok(()) => Ok(()),
                Err(err) => {
                    warn!(
                        "InitializeSharedAudioStream render failed ({err}); falling back to IAudioClient::Initialize"
                    );
                    let duration = frames_to_hns(period_frames, sample_rate);
                    let client: IAudioClient = audio_client.cast()?;
                    client.Initialize(
                        AUDCLNT_SHAREMODE_SHARED,
                        flags,
                        duration,
                        0,
                        format,
                        None,
                    )?;
                    Ok(())
                }
            }
        }
    }

    fn shared_engine_period_frames(
        audio_client: &IAudioClient3,
        format: *const WAVEFORMATEX,
    ) -> Result<u32> {
        let mut default_frames = 0_u32;
        let mut fundamental_frames = 0_u32;
        let mut min_frames = 0_u32;
        let mut max_frames = 0_u32;
        unsafe {
            audio_client.GetSharedModeEnginePeriod(
                format,
                &mut default_frames,
                &mut fundamental_frames,
                &mut min_frames,
                &mut max_frames,
            )?;
        }
        Ok(min_frames.max(fundamental_frames).max(1))
    }

    fn prime_silence_render(
        audio_client: &IAudioClient3,
        render_client: &IAudioRenderClient,
    ) -> Result<()> {
        let buffer_frames = unsafe { audio_client.GetBufferSize()? };
        if buffer_frames == 0 {
            return Ok(());
        }
        unsafe {
            let _buffer = render_client.GetBuffer(buffer_frames)?;
            render_client.ReleaseBuffer(buffer_frames, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)?;
        }
        Ok(())
    }

    fn write_silence_if_needed(
        audio_client: &IAudioClient3,
        render_client: &IAudioRenderClient,
    ) -> Result<()> {
        let buffer_frames = unsafe { audio_client.GetBufferSize()? };
        let padding = unsafe { audio_client.GetCurrentPadding()? };
        let available = buffer_frames.saturating_sub(padding);
        if available == 0 {
            return Ok(());
        }

        unsafe {
            let _buffer = render_client.GetBuffer(available)?;
            render_client.ReleaseBuffer(available, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)?;
        }
        Ok(())
    }

    fn process_capture_packets(
        capture_client: &IAudioCaptureClient,
        sample_format: WasapiSampleFormat,
        block_align: usize,
        processor: &mut CaptureProcessor,
        pcm_pipe: &PcmPipeHandle,
    ) -> Result<()> {
        loop {
            let frames = unsafe { capture_client.GetNextPacketSize()? };
            if frames == 0 {
                return Ok(());
            }

            let mut data = ptr::null_mut::<u8>();
            let mut frames_read = frames;
            let mut flags = 0_u32;
            unsafe {
                capture_client.GetBuffer(
                    &mut data,
                    &mut frames_read,
                    &mut flags,
                    None,
                    None,
                )?;
            }

            let byte_len = frames_read as usize * block_align;
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            if silent {
                processor.send_silence(frames_read as usize, pcm_pipe);
            } else if !data.is_null() && byte_len > 0 {
                let bytes = unsafe { std::slice::from_raw_parts(data, byte_len) };
                processor.send_wasapi_bytes(bytes, sample_format, pcm_pipe);
            }

            unsafe {
                capture_client
                    .ReleaseBuffer(frames_read)
                    .context("failed to release WASAPI capture buffer")?;
            }
        }
    }

    fn wait_for_event(handles: &[HANDLE]) -> Result<usize> {
        let result = unsafe { WaitForMultipleObjectsEx(handles, false, INFINITE, false) };
        if result == WAIT_FAILED {
            return Err(anyhow!("failed to wait for WASAPI event"));
        }
        Ok((result.0 - WAIT_OBJECT_0.0) as usize)
    }

    unsafe fn wave_format_from_ptr(format: *const WAVEFORMATEX) -> Result<WaveFormat> {
        if format.is_null() {
            return Err(anyhow!("WASAPI returned null mix format"));
        }

        let format_ref = unsafe { &*format };
        let format_tag = format_ref.wFormatTag;
        let channels = format_ref.nChannels;
        let sample_rate = format_ref.nSamplesPerSec;
        let block_align = format_ref.nBlockAlign;
        let bits_per_sample = format_ref.wBitsPerSample;
        let mut sub_format = None;

        if format_tag as u32 == WAVE_FORMAT_EXTENSIBLE {
            let extensible = unsafe { &*(format as *const WAVEFORMATEXTENSIBLE) };
            sub_format = Some(extensible.SubFormat);
        }

        Ok(WaveFormat {
            format_tag,
            channels,
            sample_rate,
            block_align,
            bits_per_sample,
            sub_format,
        })
    }

    fn frames_to_hns(frames: u32, sample_rate: u32) -> i64 {
        if sample_rate == 0 {
            return REFTIMES_PER_SEC / 100;
        }
        ((frames as i64 * REFTIMES_PER_SEC) / sample_rate as i64).max(1)
    }

    struct EventHandle(HANDLE);

    impl EventHandle {
        fn new() -> Result<Self> {
            let handle = unsafe { CreateEventW(None, false, false, None)? };
            Ok(Self(handle))
        }
    }

    impl Drop for EventHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    struct AudioClientStopGuard<'a>(&'a IAudioClient3);

    impl Drop for AudioClientStopGuard<'_> {
        fn drop(&mut self) {
            unsafe {
                let _ = self.0.Stop();
            }
        }
    }

    struct CoTaskMemFormat(*const c_void);

    impl Drop for CoTaskMemFormat {
        fn drop(&mut self) {
            unsafe {
                CoTaskMemFree(Some(self.0));
            }
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
    use super::*;

    pub fn spawn_speaker_loopback_stream(_pcm_pipe: PcmPipeHandle) -> Result<LoopbackStreams> {
        Err(anyhow!("native WASAPI loopback capture is only supported on Windows"))
    }

    pub fn wave_format_is_float(_format: &WaveFormat) -> bool {
        false
    }

    pub fn wave_format_is_pcm(_format: &WaveFormat) -> bool {
        false
    }
}

struct CaptureProcessor {
    source_rate: u32,
    source_channels: usize,
    resampler: Option<StreamResampler>,
    last_trigger: Option<Instant>,
    seen_clear_epoch: u64,
}

impl CaptureProcessor {
    fn new(source_rate: u32, source_channels: usize) -> Self {
        let resampler = if source_rate == SAMPLE_RATE {
            None
        } else {
            match StreamResampler::new(source_rate, SAMPLE_RATE) {
                Ok(resampler) => Some(resampler),
                Err(err) => {
                    warn!(
                        "failed to create high-quality resampler {source_rate}Hz -> {SAMPLE_RATE}Hz; falling back to linear resampling: {err}"
                    );
                    None
                }
            }
        };

        Self {
            source_rate,
            source_channels,
            resampler,
            last_trigger: None,
            seen_clear_epoch: crate::audio_stats::clear_epoch(),
        }
    }

    fn record_trigger(&mut self) {
        let now = Instant::now();
        let clear_epoch = crate::audio_stats::clear_epoch();
        if self.seen_clear_epoch != clear_epoch {
            self.seen_clear_epoch = clear_epoch;
            self.last_trigger = Some(now);
            return;
        }
        if let Some(last_trigger) = self.last_trigger {
            let trigger_gap = now.saturating_duration_since(last_trigger);
            crate::audio_stats::record_wasapi_trigger_gap(trigger_gap.as_micros() as u64);
        }
        self.last_trigger = Some(now);
    }

    fn send_wasapi_bytes(
        &mut self,
        input: &[u8],
        sample_format: WasapiSampleFormat,
        pipe: &PcmPipeHandle,
    ) {
        if input.is_empty() {
            return;
        }
        self.record_trigger();

        match sample_format {
            WasapiSampleFormat::F32 => {
                self.send_stereo_f32(to_stereo_f32(input, self.source_channels, 4, read_f32), pipe);
            }
            WasapiSampleFormat::I16 => {
                if self.source_rate == SAMPLE_RATE && self.source_channels == CHANNELS {
                    push_latest_pcm(pipe, AudioChunk::I16(read_i16_samples(input)));
                } else {
                    self.send_stereo_f32(
                        to_stereo_f32(input, self.source_channels, 2, read_i16_f32),
                        pipe,
                    );
                }
            }
            WasapiSampleFormat::I24 => {
                self.send_stereo_f32(
                    to_stereo_f32(input, self.source_channels, 3, read_i24_f32),
                    pipe,
                );
            }
            WasapiSampleFormat::I32 => {
                self.send_stereo_f32(
                    to_stereo_f32(input, self.source_channels, 4, read_i32_f32),
                    pipe,
                );
            }
        }
    }

    fn send_silence(&mut self, frames: usize, pipe: &PcmPipeHandle) {
        if frames == 0 {
            return;
        }
        self.record_trigger();

        if self.source_rate == SAMPLE_RATE && self.source_channels == CHANNELS {
            push_latest_pcm(pipe, AudioChunk::I16(vec![0; frames * CHANNELS]));
            return;
        }

        self.send_stereo_f32(vec![0.0; frames * CHANNELS], pipe);
    }

    fn send_stereo_f32(&mut self, stereo: Vec<f32>, pipe: &PcmPipeHandle) {
        let pcm = if self.source_rate == SAMPLE_RATE {
            stereo
        } else if let Some(resampler) = self.resampler.as_mut() {
            match resampler.process(&stereo) {
                Ok(pcm) => pcm,
                Err(err) => {
                    warn!(
                        "high-quality resampling failed; falling back to linear resampling: {err}"
                    );
                    resample_linear(&stereo, self.source_rate, SAMPLE_RATE)
                }
            }
        } else {
            resample_linear(&stereo, self.source_rate, SAMPLE_RATE)
        };

        push_latest_pcm(pipe, AudioChunk::F32(pcm));
    }

}

struct StreamResampler {
    inner: Fft<f32>,
    pending: Vec<f32>,
    scratch_input: Vec<f32>,
}

impl StreamResampler {
    fn new(source_rate: u32, target_rate: u32) -> Result<Self> {
        let inner = Fft::<f32>::new(
            source_rate as usize,
            target_rate as usize,
            MAX_FRAME_SAMPLES_PER_CHANNEL,
            1,
            CHANNELS,
            FixedSync::Input,
        )?;
        info!("using rubato FFT resampler: {source_rate}Hz -> {target_rate}Hz");
        Ok(Self {
            inner,
            pending: Vec::with_capacity(MAX_FRAME_SAMPLES * 4),
            scratch_input: Vec::with_capacity(MAX_FRAME_SAMPLES),
        })
    }

    fn process(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        self.pending.extend_from_slice(input);
        let mut out = Vec::new();

        loop {
            let input_frames = self.inner.input_frames_next();
            let input_samples = input_frames * CHANNELS;
            if self.pending.len() < input_samples {
                break;
            }

            self.scratch_input.clear();
            self.scratch_input
                .extend_from_slice(&self.pending[..input_samples]);
            consume_prefix(&mut self.pending, input_samples);

            let input_adapter = InterleavedOwned::new_from(
                std::mem::take(&mut self.scratch_input),
                CHANNELS,
                input_frames,
            )?;
            let output_adapter = self.inner.process(&input_adapter, 0, None)?;
            self.scratch_input = input_adapter.take_data();
            out.extend(output_adapter.take_data());
        }

        Ok(out)
    }
}

fn to_stereo_f32(
    input: &[u8],
    source_channels: usize,
    sample_size: usize,
    read_sample: fn(&[u8]) -> f32,
) -> Vec<f32> {
    if sample_size == 0 {
        return Vec::new();
    }

    if source_channels == CHANNELS {
        return input
            .chunks_exact(sample_size)
            .map(read_sample)
            .collect();
    }

    let source_channels = source_channels.max(1);
    let frame_size = source_channels * sample_size;
    let frames = input.len() / frame_size;
    let mut out = Vec::with_capacity(frames * CHANNELS);
    for frame in input.chunks_exact(frame_size) {
        let left = read_sample(&frame[..sample_size]);
        let right = if source_channels > 1 {
            read_sample(&frame[sample_size..sample_size * 2])
        } else {
            left
        };
        out.push(left);
        out.push(right);
    }
    out
}

fn read_i16_samples(input: &[u8]) -> Vec<i16> {
    input.chunks_exact(2).map(read_i16).collect()
}

fn read_i16(bytes: &[u8]) -> i16 {
    i16::from_le_bytes([bytes[0], bytes[1]])
}

fn read_i16_f32(bytes: &[u8]) -> f32 {
    read_i16(bytes) as f32 / i16::MAX as f32
}

fn read_i24_f32(bytes: &[u8]) -> f32 {
    let value = ((bytes[0] as i32) << 8)
        | ((bytes[1] as i32) << 16)
        | ((bytes[2] as i32) << 24);
    (value >> 8) as f32 / 8_388_607.0
}

fn read_i32_f32(bytes: &[u8]) -> f32 {
    i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32 / i32::MAX as f32
}

fn read_f32(bytes: &[u8]) -> f32 {
    f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn resample_linear(input: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    let source_frames = input.len() / CHANNELS;
    if source_frames < 2 {
        return input.to_vec();
    }

    let target_frames = ((source_frames as u64 * target_rate as u64) / source_rate as u64) as usize;
    let ratio = source_rate as f64 / target_rate as f64;
    let mut out = Vec::with_capacity(target_frames * CHANNELS);

    for frame in 0..target_frames {
        let pos = frame as f64 * ratio;
        let idx = pos.floor() as usize;
        let frac = (pos - idx as f64) as f32;
        let next = (idx + 1).min(source_frames - 1);

        for ch in 0..CHANNELS {
            let a = input[idx * CHANNELS + ch];
            let b = input[next * CHANNELS + ch];
            out.push(a + (b - a) * frac);
        }
    }

    out
}

fn send_pcm_udp(
    pcm_pipe: PcmPipeHandle,
    target_addr: SocketAddr,
) -> Result<()> {
    let mut pending = Vec::<i16>::with_capacity(MAX_FRAME_SAMPLES * 2);
    let mut frame_sizer = FrameSizer::new();
    let mut last_log = Instant::now();
    let mut last_send = None::<Instant>;
    let mut send_gap = Duration::ZERO;
    let mut sequence = 0_u64;
    let mut timestamp_samples = 0_u64;
    let mut packet = Vec::with_capacity(UDP_PACKET_HEADER_BYTES + MAX_FRAME_BYTES * 2);
    let mut previous_fec = Vec::<u8>::new();
    let mut previous_fec_sequence = 0_u64;
    let mut has_previous_fec = false;
    let bind_addr = if target_addr.is_ipv4() {
        SocketAddr::from(([0, 0, 0, 0], 0))
    } else {
        SocketAddr::from(([0_u16; 8], 0))
    };
    let socket = UdpSocket::bind(bind_addr)?;

    crate::audio_stats::reset_udp_stream(&target_addr.to_string());
    info!("sending UDP PCM frames to {target_addr}");

    while let Some(pcm) = recv_pcm_low_latency(&pcm_pipe, last_send) {
        match pcm {
            AudioChunk::I16(mut samples) => pending.append(&mut samples),
            AudioChunk::F32(samples) => pcm_f32_to_i16(&samples, &mut pending),
        }

        loop {
            let frame_samples = frame_sizer.next_frame_samples() * CHANNELS;
            if pending.len() < frame_samples {
                break;
            }

            let frame = &pending[..frame_samples];
            let is_silent_frame = frame.iter().all(|sample| *sample == 0);
            let fec = if UDP_PACKET_FEC_ENABLED && has_previous_fec {
                Some((previous_fec_sequence, previous_fec.as_slice()))
            } else {
                None
            };
            write_udp_packet_header(sequence, timestamp_samples, 0, frame.len(), fec, &mut packet);
            pcm_i16_to_le(frame, &mut packet);
            if let Some((_, fec_payload)) = fec {
                packet.extend_from_slice(fec_payload);
            }
            for _ in 0..UDP_PACKET_SEND_MULTIPLIER.max(1) {
                write_udp_packet_sent_at(&mut packet);
                socket
                    .send_to(&packet, target_addr)
                    .context("Failed to send PCM frame to UDP client")?;
            }
            let now = Instant::now();
            if let Some(last_send) = last_send {
                send_gap = now.saturating_duration_since(last_send);
                crate::audio_stats::record_udp_send_gap(send_gap.as_micros() as u64);
            }
            last_send = Some(now);
            if UDP_PACKET_FEC_ENABLED {
                encode_pcm_s16le_stereo_fec(frame, &mut previous_fec);
                previous_fec_sequence = sequence;
                has_previous_fec = true;
            }
            consume_prefix(&mut pending, frame_samples);
            sequence = sequence.wrapping_add(1);
            timestamp_samples = timestamp_samples.wrapping_add((frame_samples / CHANNELS) as u64);

            if last_log.elapsed() >= Duration::from_secs(1) {
                let send_gap_us = send_gap.as_micros() as u64;
                crate::audio_stats::update_udp_send_window(0, send_gap_us);
                last_log = Instant::now();
            }
        }
    }

    crate::audio_stats::mark_udp_stream_inactive();
    Ok(())
}

fn recv_pcm_low_latency(
    pcm_pipe: &PcmPipeHandle,
    _last_send: Option<Instant>,
) -> Option<AudioChunk> {
    recv_pcm_wait(pcm_pipe)
}

fn recv_pcm_wait(pcm_pipe: &PcmPipeHandle) -> Option<AudioChunk> {
    loop {
        if let Some(pcm) = pcm_pipe.queue.pop() {
            return Some(pcm);
        }
        if !pcm_pipe.producer_alive.load(Ordering::Acquire) {
            return None;
        }
        thread::park();
    }
}

fn consume_prefix<T: Copy>(samples: &mut Vec<T>, count: usize) {
    if count >= samples.len() {
        samples.clear();
        return;
    }

    samples.copy_within(count.., 0);
    samples.truncate(samples.len() - count);
}

fn write_udp_packet_header(
    sequence: u64,
    timestamp_samples: u64,
    flags: u16,
    primary_samples: usize,
    fec: Option<(u64, &[u8])>,
    out: &mut Vec<u8>,
) {
    out.clear();
    out.extend_from_slice(&UDP_PACKET_MAGIC);
    out.push(UDP_PACKET_VERSION);
    out.push(UDP_PACKET_HEADER_BYTES as u8);
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&sequence.to_le_bytes());
    out.extend_from_slice(&timestamp_samples.to_le_bytes());
    out.extend_from_slice(&0_u64.to_le_bytes());
    out.extend_from_slice(&((primary_samples * std::mem::size_of::<i16>()) as u32).to_le_bytes());
    if let Some((fec_sequence, fec_payload)) = fec {
        out.extend_from_slice(&fec_sequence.to_le_bytes());
        out.push(UDP_PACKET_FEC_CODEC_PCM_S16LE_STEREO);
        out.push(0);
        out.extend_from_slice(&(fec_payload.len() as u16).to_le_bytes());
    } else {
        out.extend_from_slice(&0_u64.to_le_bytes());
        out.push(UDP_PACKET_FEC_CODEC_NONE);
        out.push(0);
        out.extend_from_slice(&0_u16.to_le_bytes());
    }
}

fn write_udp_packet_sent_at(packet: &mut [u8]) {
    packet[24..32].copy_from_slice(&current_unix_time_ms().to_le_bytes());
}

fn current_unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

struct FrameSizer {
    carried_samples: u32,
}

impl FrameSizer {
    fn new() -> Self {
        Self { carried_samples: 0 }
    }

    fn next_frame_samples(&mut self) -> usize {
        let total = SAMPLE_RATE * FRAME_MS + self.carried_samples;
        let samples = total / 1000;
        self.carried_samples = total % 1000;
        samples as usize
    }
}

fn pcm_f32_to_i16(samples: &[f32], out: &mut Vec<i16>) {
    out.reserve(samples.len());
    for sample in samples {
        let sample = sample.clamp(-1.0, 1.0);
        out.push((sample * i16::MAX as f32).round() as i16);
    }
}

fn pcm_i16_to_le(frame: &[i16], out: &mut Vec<u8>) {
    for sample in frame {
        out.extend_from_slice(&sample.to_le_bytes());
    }
}

fn encode_pcm_s16le_stereo_fec(frame: &[i16], out: &mut Vec<u8>) {
    out.clear();
    pcm_i16_to_le(frame, out);
}
