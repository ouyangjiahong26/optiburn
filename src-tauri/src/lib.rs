//! OptiBurn 图形前端的 Rust 侧：Tauri 命令层，复用核心 crate（ADR-0008）。
//!
//! 平台差异不在这里出现：设备枚举与打开都走 `optiburn-transport`。

mod clipboard;
mod cmd;
mod i18n;
mod job;
mod updater;

use tauri::{Emitter, Manager};

pub fn run() {
    tauri::Builder::default()
        // 单实例插件必须第一个注册：重复启动时把已有主窗口带到前台。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(job::JobState::default())
        .invoke_handler(tauri::generate_handler![
            cmd::probe_devices,
            cmd::start_build_image,
            cmd::start_burn,
            cmd::start_append,
            cmd::disc_volume_id,
            cmd::copy::list_disc,
            cmd::copy::copy_disc_files,
            cmd::copy::paste_files,
            cmd::verify::start_verify,
            cmd::cancel_job,
            cmd::confirm_close,
            updater::can_self_update,
        ])
        .on_window_event(|window, event| {
            // 有任务在跑时拦下关窗：由前端弹确认框，确认后走 confirm_close。
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.app_handle().state::<job::JobState>();
                if state.0.lock().expect("job state mutex").is_some() {
                    api.prevent_close();
                    let _ = window.app_handle().emit("close-blocked", ());
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running optiburn gui");
}
