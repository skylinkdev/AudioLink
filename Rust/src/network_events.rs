use anyhow::Result;
use tokio::sync::watch;

#[cfg(windows)]
mod platform {
    use super::*;
    use anyhow::bail;
    use std::ffi::c_void;
    use windows::Win32::{
        Foundation::{HANDLE, NO_ERROR},
        NetworkManagement::IpHelper::{
            CancelMibChangeNotify2, MIB_IPINTERFACE_ROW, MIB_NOTIFICATION_TYPE,
            MIB_UNICASTIPADDRESS_ROW, NotifyIpInterfaceChange, NotifyUnicastIpAddressChange,
        },
        Networking::WinSock::AF_UNSPEC,
    };

    pub struct NetworkChangeWatcher {
        interface_handle: HANDLE,
        address_handle: HANDLE,
        context: *mut CallbackContext,
    }

    // The watcher owns cancellation handles and a boxed callback context. The
    // Windows callbacks access the context until both notifications are canceled
    // in Drop, so moving this owner between Tokio worker threads is OK.
    unsafe impl Send for NetworkChangeWatcher {}

    impl Drop for NetworkChangeWatcher {
        fn drop(&mut self) {
            for handle in [self.interface_handle, self.address_handle] {
                let result = unsafe { CancelMibChangeNotify2(handle) };
                if result != NO_ERROR {
                    tracing::warn!("failed to cancel network change watcher: {result:?}");
                }
            }

            unsafe {
                drop(Box::from_raw(self.context));
            }
        }
    }

    pub fn watch_network_changes(tx: watch::Sender<u64>) -> Result<NetworkChangeWatcher> {
        let context = Box::new(CallbackContext { tx });
        let context = Box::into_raw(context);
        let mut interface_handle = HANDLE::default();
        let mut address_handle = HANDLE::default();

        let interface_result = unsafe {
            NotifyIpInterfaceChange(
                AF_UNSPEC,
                Some(on_ip_interface_changed),
                Some(context.cast::<c_void>()),
                false,
                &mut interface_handle,
            )
        };

        if interface_result != NO_ERROR {
            unsafe {
                drop(Box::from_raw(context));
            }
            bail!("NotifyIpInterfaceChange failed: {interface_result:?}");
        }

        let address_result = unsafe {
            NotifyUnicastIpAddressChange(
                AF_UNSPEC,
                Some(on_unicast_ip_address_changed),
                Some(context.cast::<c_void>()),
                false,
                &mut address_handle,
            )
        };

        if address_result != NO_ERROR {
            let _ = unsafe { CancelMibChangeNotify2(interface_handle) };
            unsafe {
                drop(Box::from_raw(context));
            }
            bail!("NotifyUnicastIpAddressChange failed: {address_result:?}");
        }

        tracing::info!("Windows network change watcher started");
        Ok(NetworkChangeWatcher {
            interface_handle,
            address_handle,
            context,
        })
    }

    struct CallbackContext {
        tx: watch::Sender<u64>,
    }

    unsafe extern "system" fn on_ip_interface_changed(
        callercontext: *const c_void,
        _row: *const MIB_IPINTERFACE_ROW,
        notificationtype: MIB_NOTIFICATION_TYPE,
    ) {
        if callercontext.is_null() {
            return;
        }

        let context = unsafe { &*(callercontext.cast::<CallbackContext>()) };
        notify_network_change(context);
        tracing::info!("Windows network interface changed: {notificationtype:?}");
    }

    unsafe extern "system" fn on_unicast_ip_address_changed(
        callercontext: *const c_void,
        _row: *const MIB_UNICASTIPADDRESS_ROW,
        notificationtype: MIB_NOTIFICATION_TYPE,
    ) {
        if callercontext.is_null() {
            return;
        }

        let context = unsafe { &*(callercontext.cast::<CallbackContext>()) };
        notify_network_change(context);
        tracing::info!("Windows unicast IP address changed: {notificationtype:?}");
    }

    fn notify_network_change(context: &CallbackContext) {
        let current = *context.tx.borrow();
        let _ = context.tx.send(current.wrapping_add(1));
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub struct NetworkChangeWatcher;

    pub fn watch_network_changes(_tx: watch::Sender<u64>) -> Result<NetworkChangeWatcher> {
        Ok(NetworkChangeWatcher)
    }
}

pub use platform::watch_network_changes;
