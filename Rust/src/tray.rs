use std::{env, thread};

use anyhow::{Context, Result};
use auto_launch::{AutoLaunch, AutoLaunchBuilder, WindowsEnableMode};
use tao::{
    event::Event,
    event_loop::{ControlFlow, EventLoopBuilder},
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use tray_icon::{
    Icon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
};

use crate::app::WEB_ADMIN_ADDR;

#[derive(Debug)]
enum UserEvent {
    Menu(MenuEvent),
}

const OPEN_ADMIN_ID: &str = "open_admin";
const SERVICE_TOGGLE_ID: &str = "service_toggle";
const AUTOSTART_ID: &str = "autostart";
const QUIT_ID: &str = "quit";

pub fn run() -> Result<()> {
    let service = match ServiceHandle::start() {
        Ok(service) => Some(service),
        Err(err) => {
            error!("failed to start Audio Link service: {err:?}");
            None
        }
    };

    run_tray_event_loop(service);
    Ok(())
}

struct ServiceHandle {
    shutdown: CancellationToken,
    thread: thread::JoinHandle<()>,
}

impl ServiceHandle {
    fn start() -> Result<Self> {
        let shutdown = CancellationToken::new();
        let service_shutdown = shutdown.clone();
        let thread = thread::Builder::new()
            .name("lan-audio-service".to_string())
            .spawn(move || run_service(service_shutdown))
            .context("failed to spawn Audio Link service thread")?;

        Ok(Self { shutdown, thread })
    }

    fn stop(self) {
        self.shutdown.cancel();
        if let Err(err) = self.thread.join() {
            error!("Audio Link service thread panicked: {err:?}");
        }
    }

    fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }
}

fn run_service(shutdown: CancellationToken) {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            error!("failed to create Tokio runtime: {err:?}");
            return;
        }
    };

    if let Err(err) = runtime.block_on(crate::app::run(shutdown)) {
        error!("Audio Link service stopped with error: {err:?}");
    }
}

fn run_tray_event_loop(mut service: Option<ServiceHandle>) {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::Menu(event));
    }));

    let auto_launch = match auto_launch() {
        Ok(auto_launch) => Some(auto_launch),
        Err(err) => {
            warn!("failed to initialize autostart integration: {err:?}");
            None
        }
    };

    let tray_menu = Menu::new();
    let service_toggle = MenuItem::with_id(
        MenuId::new(SERVICE_TOGGLE_ID),
        service_toggle_text(service.is_some()),
        true,
        None,
    );
    let service_status = MenuItem::new(service_status_text(service.is_some()), false, None);
    let open_admin = MenuItem::with_id(
        MenuId::new(OPEN_ADMIN_ID),
        "打开管理页面",
        service.is_some(),
        None,
    );
    let autostart_enabled = auto_launch
        .as_ref()
        .and_then(|auto_launch| auto_launch.is_enabled().ok())
        .unwrap_or(false);
    let autostart = CheckMenuItem::with_id(
        MenuId::new(AUTOSTART_ID),
        "开机启动",
        auto_launch.is_some(),
        autostart_enabled,
        None,
    );
    let quit = MenuItem::with_id(MenuId::new(QUIT_ID), "退出", true, None);

    if let Err(err) = tray_menu.append_items(&[
        &service_status,
        &service_toggle,
        &PredefinedMenuItem::separator(),
        &open_admin,
        &autostart,
        &PredefinedMenuItem::separator(),
        &quit,
    ]) {
        warn!("failed to build tray menu: {err:?}");
    }

    let _tray_icon = match TrayIconBuilder::new()
        .with_tooltip("Audio Link")
        .with_menu(Box::new(tray_menu))
        .with_icon(default_icon())
        .build()
    {
        Ok(tray_icon) => Some(tray_icon),
        Err(err) => {
            warn!("failed to create tray icon: {err:?}");
            None
        }
    };

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        if let Event::UserEvent(UserEvent::Menu(event)) = event {
            refresh_finished_service(&mut service, &service_toggle, &service_status, &open_admin);
            let id = event.id().0.as_str();
            match id {
                OPEN_ADMIN_ID => open_admin_page(),
                SERVICE_TOGGLE_ID => {
                    toggle_service(&mut service, &service_toggle, &service_status, &open_admin)
                }
                AUTOSTART_ID => toggle_autostart(auto_launch.as_ref(), &autostart),
                QUIT_ID => {
                    info!("quit requested from tray");
                    if let Some(service) = service.take() {
                        service.stop();
                    }
                    *control_flow = ControlFlow::Exit;
                }
                _ => {}
            }
        }
    });
}

fn auto_launch() -> Result<AutoLaunch> {
    let exe = env::current_exe().context("failed to resolve current executable path")?;
    let exe = exe
        .to_str()
        .context("current executable path is not valid UTF-8")?;

    let mut builder = AutoLaunchBuilder::new();
    builder
        .set_app_name("Audio Link")
        .set_app_path(exe)
        .set_args(&["--minimized"]);

    #[cfg(windows)]
    builder.set_windows_enable_mode(WindowsEnableMode::CurrentUser);

    builder
        .build()
        .context("failed to create auto-launch entry")
}

fn open_admin_page() {
    let url = format!("http://{WEB_ADMIN_ADDR}");
    if let Err(err) = webbrowser::open(&url) {
        warn!("failed to open web admin page {url}: {err:?}");
    }
}

fn toggle_service(
    service: &mut Option<ServiceHandle>,
    service_toggle: &MenuItem,
    service_status: &MenuItem,
    open_admin: &MenuItem,
) {
    if let Some(service_handle) = service.take() {
        info!("stop requested from tray");
        service_handle.stop();
    } else {
        info!("start requested from tray");
        match ServiceHandle::start() {
            Ok(service_handle) => *service = Some(service_handle),
            Err(err) => error!("failed to start Audio Link service: {err:?}"),
        }
    }

    update_service_menu(service.is_some(), service_toggle, service_status, open_admin);
}

fn refresh_finished_service(
    service: &mut Option<ServiceHandle>,
    service_toggle: &MenuItem,
    service_status: &MenuItem,
    open_admin: &MenuItem,
) {
    let finished = service
        .as_ref()
        .is_some_and(|service_handle| service_handle.is_finished());
    if !finished {
        return;
    }

    if let Some(service_handle) = service.take() {
        service_handle.stop();
    }
    update_service_menu(false, service_toggle, service_status, open_admin);
}

fn update_service_menu(
    running: bool,
    service_toggle: &MenuItem,
    service_status: &MenuItem,
    open_admin: &MenuItem,
) {
    service_status.set_text(service_status_text(running));
    service_toggle.set_text(service_toggle_text(running));
    open_admin.set_enabled(running);
}

fn service_status_text(running: bool) -> &'static str {
    if running {
        "状态：运行中"
    } else {
        "状态：已停止"
    }
}

fn service_toggle_text(running: bool) -> &'static str {
    if running { "停止服务" } else { "启动服务" }
}

fn toggle_autostart(auto_launch: Option<&AutoLaunch>, autostart: &CheckMenuItem) {
    let Some(auto_launch) = auto_launch else {
        autostart.set_checked(false);
        return;
    };

    match auto_launch.is_enabled() {
        Ok(true) => match auto_launch.disable() {
            Ok(()) => autostart.set_checked(false),
            Err(err) => warn!("failed to disable autostart: {err:?}"),
        },
        Ok(false) => match auto_launch.enable() {
            Ok(()) => autostart.set_checked(true),
            Err(err) => warn!("failed to enable autostart: {err:?}"),
        },
        Err(err) => warn!("failed to read autostart state: {err:?}"),
    }
}

fn default_icon() -> Icon {
    const SIZE: u32 = 32;
    let rgba = include_bytes!("../assets/tray_icon_32.rgba").to_vec();
    Icon::from_rgba(rgba, SIZE, SIZE).expect("bundled tray icon should be valid")
}
