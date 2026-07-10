use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

static AUDIO_STREAM_ACTIVE: AtomicBool = AtomicBool::new(false);
static UDP_TARGET_ADDR: Mutex<String> = Mutex::new(String::new());
static SEND_GAP_US: AtomicU64 = AtomicU64::new(0);
static MAX_SEND_GAP_US: AtomicU64 = AtomicU64::new(0);
static LAST_SENT_FRAMES_PER_SEC: AtomicU64 = AtomicU64::new(0);
static WASAPI_TRIGGER_GAP_US: AtomicU64 = AtomicU64::new(0);
static MAX_WASAPI_TRIGGER_GAP_US: AtomicU64 = AtomicU64::new(0);
static STATS_CLEAR_EPOCH: AtomicU64 = AtomicU64::new(0);

pub fn reset_udp_stream(target_addr: &str) {
    AUDIO_STREAM_ACTIVE.store(true, Ordering::Relaxed);
    if let Ok(mut stored_target_addr) = UDP_TARGET_ADDR.lock() {
        stored_target_addr.clear();
        stored_target_addr.push_str(target_addr);
    }
    SEND_GAP_US.store(0, Ordering::Relaxed);
    MAX_SEND_GAP_US.store(0, Ordering::Relaxed);
    LAST_SENT_FRAMES_PER_SEC.store(0, Ordering::Relaxed);
    WASAPI_TRIGGER_GAP_US.store(0, Ordering::Relaxed);
    MAX_WASAPI_TRIGGER_GAP_US.store(0, Ordering::Relaxed);
}

pub fn record_udp_send_gap(send_gap_us: u64) {
    MAX_SEND_GAP_US.fetch_max(send_gap_us, Ordering::Relaxed);
}

pub fn record_wasapi_trigger_gap(trigger_gap_us: u64) {
    WASAPI_TRIGGER_GAP_US.store(trigger_gap_us, Ordering::Relaxed);
    MAX_WASAPI_TRIGGER_GAP_US.fetch_max(trigger_gap_us, Ordering::Relaxed);
}

pub fn clear_history() {
    SEND_GAP_US.store(0, Ordering::Relaxed);
    MAX_SEND_GAP_US.store(0, Ordering::Relaxed);
    LAST_SENT_FRAMES_PER_SEC.store(0, Ordering::Relaxed);
    WASAPI_TRIGGER_GAP_US.store(0, Ordering::Relaxed);
    MAX_WASAPI_TRIGGER_GAP_US.store(0, Ordering::Relaxed);
    STATS_CLEAR_EPOCH.fetch_add(1, Ordering::Relaxed);
}

pub fn update_udp_send_window(sent_frames: u64, send_gap_us: u64) {
    AUDIO_STREAM_ACTIVE.store(true, Ordering::Relaxed);
    LAST_SENT_FRAMES_PER_SEC.store(sent_frames, Ordering::Relaxed);
    SEND_GAP_US.store(send_gap_us, Ordering::Relaxed);
}

pub fn mark_udp_stream_inactive() {
    AUDIO_STREAM_ACTIVE.store(false, Ordering::Relaxed);
}

pub fn clear_epoch() -> u64 {
    STATS_CLEAR_EPOCH.load(Ordering::Relaxed)
}

pub fn snapshot() -> AudioStatsSnapshot {
    AudioStatsSnapshot {
        active: AUDIO_STREAM_ACTIVE.load(Ordering::Relaxed),
        target_addr: UDP_TARGET_ADDR
            .lock()
            .map(|target_addr| target_addr.clone())
            .unwrap_or_default(),
        send_gap_ms: SEND_GAP_US.load(Ordering::Relaxed) as f64 / 1000.0,
        max_send_gap_ms: MAX_SEND_GAP_US.load(Ordering::Relaxed) as f64 / 1000.0,
        wasapi_trigger_gap_ms: WASAPI_TRIGGER_GAP_US.load(Ordering::Relaxed) as f64 / 1000.0,
        max_wasapi_trigger_gap_ms: MAX_WASAPI_TRIGGER_GAP_US.load(Ordering::Relaxed) as f64 / 1000.0,
    }
}

pub struct AudioStatsSnapshot {
    pub active: bool,
    pub target_addr: String,
    pub send_gap_ms: f64,
    pub max_send_gap_ms: f64,

    pub wasapi_trigger_gap_ms: f64,
    pub max_wasapi_trigger_gap_ms: f64,
}
