use anyhow::Result;

#[derive(Clone, Debug)]
pub struct AudioEnginePeriod {
    pub sample_rate: u32,
    pub default_frames: u32,
    pub fundamental_frames: u32,
    pub min_frames: u32,
    pub max_frames: u32,
}

impl AudioEnginePeriod {
    pub fn default_ms(&self) -> f64 {
        frames_to_ms(self.default_frames, self.sample_rate)
    }

    pub fn fundamental_ms(&self) -> f64 {
        frames_to_ms(self.fundamental_frames, self.sample_rate)
    }

    pub fn min_ms(&self) -> f64 {
        frames_to_ms(self.min_frames, self.sample_rate)
    }

    pub fn max_ms(&self) -> f64 {
        frames_to_ms(self.max_frames, self.sample_rate)
    }
}

pub fn query_default_render_engine_period() -> Result<AudioEnginePeriod> {
    platform::query_default_render_engine_period()
}

fn frames_to_ms(frames: u32, sample_rate: u32) -> f64 {
    if sample_rate == 0 {
        return 0.0;
    }
    frames as f64 * 1000.0 / sample_rate as f64
}

#[cfg(windows)]
mod platform {
    use super::AudioEnginePeriod;
    use anyhow::{Context, Result};
    use windows::Win32::{
        Media::Audio::{
            ERole, IAudioClient3, IMMDeviceEnumerator, MMDeviceEnumerator, eMultimedia, eRender,
        },
        System::Com::{
            CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
            CoUninitialize,
        },
    };

    pub fn query_default_render_engine_period() -> Result<AudioEnginePeriod> {
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
        let audio_client: IAudioClient3 = unsafe {
            device
                .Activate(CLSCTX_ALL, None)
                .context("failed to activate IAudioClient3")?
        };
        let mix_format = unsafe {
            audio_client
                .GetMixFormat()
                .context("failed to get mix format")?
        };
        let sample_rate = unsafe { (*mix_format).nSamplesPerSec };

        let mut default_frames = 0_u32;
        let mut fundamental_frames = 0_u32;
        let mut min_frames = 0_u32;
        let mut max_frames = 0_u32;
        let result = unsafe {
            audio_client.GetSharedModeEnginePeriod(
                mix_format,
                &mut default_frames,
                &mut fundamental_frames,
                &mut min_frames,
                &mut max_frames,
            )
        };
        unsafe {
            CoTaskMemFree(Some(mix_format.cast()));
        }
        result.context("failed to query shared mode engine period")?;

        Ok(AudioEnginePeriod {
            sample_rate,
            default_frames,
            fundamental_frames,
            min_frames,
            max_frames,
        })
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
    use super::AudioEnginePeriod;
    use anyhow::{Result, bail};

    pub fn query_default_render_engine_period() -> Result<AudioEnginePeriod> {
        bail!("IAudioClient3 engine period query is only supported on Windows")
    }
}
