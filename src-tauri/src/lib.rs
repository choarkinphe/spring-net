mod commands;

use aonet::service::{AppService, ServiceConfig};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tauri::Manager;
use tokio::sync::RwLock;

pub struct DesktopState {
    service: RwLock<Result<Arc<AppService>, String>>,
    quitting: AtomicBool,
    cleaned: AtomicBool,
    _lock: std::fs::File,
}
impl DesktopState {
    async fn service(&self) -> Result<Arc<AppService>, String> {
        if self.quitting.load(Ordering::SeqCst) {
            return Err("应用正在退出".into());
        }
        self.service.read().await.clone()
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data)?;
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(data.join("data.lock"))?;
            lock.try_lock()
                .map_err(|e| format!("数据目录已由另一实例使用：{e}"))?;
            let resources = app.path().resource_dir()?;
            let auth = data.join("authorized-targets.json");
            if !auth.exists() {
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&auth)?;
                f.write_all(b"{\"version\":1,\"targets\":[]}")?;
                f.sync_all()?;
            }
            let handle = app.handle().clone();
            app.manage(DesktopState {
                service: RwLock::new(Err("初始化中，请稍后重新连接".into())),
                quitting: AtomicBool::new(false),
                cleaned: AtomicBool::new(false),
                _lock: lock,
            });
            // Initialize validated local configuration before exposing IPC; endpoint
            // health checks start explicitly only after the service has an Arc owner.
            let initialized = tauri::async_runtime::block_on(AppService::initialize(
                ServiceConfig {
                    root: resources,
                    data: data.join("data"),
                    auth_file: auth,
                    control_port: None,
                },
                tokio_util::sync::CancellationToken::new(),
            ))
            .map(Arc::new);
            if let Ok(service) = &initialized {
                tauri::async_runtime::block_on(service.start_background());
            }
            *handle.state::<DesktopState>().service.blocking_write() = initialized;
            #[cfg(target_os = "macos")]
            {
                use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
                // A custom item routes Cmd+Q through ExitRequested, unlike native terminate:.
                let quit = MenuItemBuilder::with_id("quit-aonet", "退出 AoNet")
                    .accelerator("CmdOrCtrl+Q")
                    .build(app)?;
                let application = SubmenuBuilder::new(app, "AoNet")
                    .about(None)
                    .separator()
                    .hide()
                    .hide_others()
                    .show_all()
                    .separator()
                    .item(&quit)
                    .build()?;
                let edit = SubmenuBuilder::new(app, "编辑")
                    .undo()
                    .redo()
                    .separator()
                    .cut()
                    .copy()
                    .paste()
                    .select_all()
                    .build()?;
                let window = SubmenuBuilder::new(app, "窗口")
                    .minimize()
                    .fullscreen()
                    .build()?;
                app.set_menu(
                    MenuBuilder::new(app)
                        .items(&[&application, &edit, &window])
                        .build()?,
                )?;
                app.on_menu_event(|app, event| {
                    if event.id().as_ref() == "quit-aonet" {
                        app.exit(0);
                    }
                });
            }
            let window = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("AoNet · 网络测试工作台")
            .inner_size(1280.0, 840.0)
            .min_inner_size(900.0, 620.0)
            .on_navigation(|url| {
                (url.scheme() == "tauri" && url.host_str() == Some("localhost"))
                    || (url.scheme() == "http"
                        && url.host_str() == Some("tauri.localhost")
                        && url.port().is_none())
            })
            .build()?;
            window.on_window_event({
                let app = app.handle().clone();
                move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        app.exit(0);
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::proxy_health,
            commands::proxy_settings,
            commands::save_proxy_settings,
            commands::site_catalog,
            commands::save_site_catalog,
            commands::network_info,
            commands::network_state,
            commands::network_start,
            commands::network_stop,
            commands::bootstrap,
            commands::state,
            commands::start_run,
            commands::stop_run,
            commands::monitor_state,
            commands::monitor_save,
            commands::monitor_start,
            commands::monitor_heartbeat,
            commands::monitor_stop,
            commands::monitor_pause,
            commands::catalog_batch_info,
            commands::catalog_batch_start,
            commands::catalog_batch_state,
            commands::catalog_batch_stop,
            commands::batch_state,
            commands::batch_start,
            commands::batch_stop,
            commands::diagnostic_state,
            commands::start_diagnostic,
            commands::stop_diagnostic,
            commands::save_lists,
            commands::workspace,
            commands::import_lists_dialog,
            commands::export_dialog
        ])
        .build(tauri::generate_context!())
        .expect("无法创建 AoNet 桌面窗口")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                let state = app.state::<DesktopState>();
                if state.cleaned.load(Ordering::SeqCst) {
                    return;
                }
                api.prevent_exit();
                if !state.quitting.swap(true, Ordering::SeqCst) {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        let state = app.state::<DesktopState>();
                        if let Ok(service) = state.service.read().await.clone() {
                            service.shutdown_and_wait().await;
                        }
                        state.cleaned.store(true, Ordering::SeqCst);
                        app.exit(0);
                    });
                }
            }
        });
}
