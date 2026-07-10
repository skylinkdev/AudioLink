use tracing::warn;

pub fn boost_current_audio_thread(label: &str) {
    platform::boost_current_audio_thread(label);
}

#[cfg(windows)]
mod platform {
    use super::warn;
    use windows::{
        Win32::{
            System::Threading::{
                AVRT_PRIORITY_CRITICAL, AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority,
                GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
            },
        },
        core::w,
    };

    pub fn boost_current_audio_thread(label: &str) {
        unsafe {
            let mut task_index = 0_u32;
            match AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) {
                Ok(mmcss_handle) => {
                    if let Err(err) = AvSetMmThreadPriority(mmcss_handle, AVRT_PRIORITY_CRITICAL) {
                        warn!("failed to set MMCSS critical priority for {label}: {err}");
                    }
                }
                Err(err) => {
                    warn!("failed to join MMCSS Pro Audio task for {label}: {err}");
                }
            }

            if let Err(err) = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) {
                warn!("failed to set time-critical thread priority for {label}: {err}");
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn boost_current_audio_thread(_label: &str) {}
}
