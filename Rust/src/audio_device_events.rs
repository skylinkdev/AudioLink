use anyhow::Result;
use tokio::sync::watch;

#[cfg(windows)]
mod platform {
    use super::*;
    use anyhow::Context;
    use std::thread;
    use windows::{
        Win32::{
            Foundation::PROPERTYKEY,
            Media::Audio::{
                DEVICE_STATE, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
                IMMNotificationClient_Impl, MMDeviceEnumerator, eConsole, eMultimedia, eRender,
            },
            System::Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
            },
        },
        core::{PCWSTR, implement},
    };

    pub fn spawn_default_output_change_watcher(tx: watch::Sender<u64>) -> Result<()> {
        thread::Builder::new()
            .name("audio-device-events".to_string())
            .spawn(move || {
                if let Err(err) = run_watcher(tx) {
                    tracing::warn!("audio device event watcher stopped: {err:?}");
                }
            })
            .context("failed to spawn audio device event watcher")?;
        Ok(())
    }

    fn run_watcher(tx: watch::Sender<u64>) -> Result<()> {
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
        let client: IMMNotificationClient = AudioNotificationClient { tx }.into();

        unsafe {
            enumerator
                .RegisterEndpointNotificationCallback(&client)
                .context("failed to register endpoint notification callback")?;
        }

        tracing::info!("Windows default output device watcher started");

        loop {
            thread::park();
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

    #[implement(IMMNotificationClient)]
    struct AudioNotificationClient {
        tx: watch::Sender<u64>,
    }

    impl IMMNotificationClient_Impl for AudioNotificationClient_Impl {
        fn OnDeviceStateChanged(
            &self,
            _pwstrdeviceid: &PCWSTR,
            _dwnewstate: DEVICE_STATE,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn OnDeviceAdded(&self, _pwstrdeviceid: &PCWSTR) -> windows::core::Result<()> {
            Ok(())
        }

        fn OnDeviceRemoved(&self, _pwstrdeviceid: &PCWSTR) -> windows::core::Result<()> {
            Ok(())
        }

        fn OnDefaultDeviceChanged(
            &self,
            flow: EDataFlow,
            role: ERole,
            _pwstrdefaultdeviceid: &PCWSTR,
        ) -> windows::core::Result<()> {
            if flow == eRender && (role == eConsole || role == eMultimedia) {
                let current = *self.tx.borrow();
                let _ = self.tx.send(current.wrapping_add(1));
                tracing::info!("Windows default output device changed");
            }
            Ok(())
        }

        fn OnPropertyValueChanged(
            &self,
            _pwstrdeviceid: &PCWSTR,
            _key: &PROPERTYKEY,
        ) -> windows::core::Result<()> {
            Ok(())
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub fn spawn_default_output_change_watcher(_tx: watch::Sender<u64>) -> Result<()> {
        Ok(())
    }
}

pub use platform::spawn_default_output_change_watcher;
