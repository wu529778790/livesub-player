mod asr;
mod export;
mod history;
mod menu;
mod models;
mod mt;
mod player;
mod settings;
mod transcript;
mod update;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 日志级别：默认 Info，LIVESUB_LOG=debug/trace 可调详（排查 bug 用）
    let log_level = std::env::var("LIVESUB_LOG")
        .map(|v| match v.to_ascii_lowercase().as_str() {
            "debug" => log::LevelFilter::Debug,
            "trace" => log::LevelFilter::Trace,
            "warn" => log::LevelFilter::Warn,
            "error" => log::LevelFilter::Error,
            _ => log::LevelFilter::Info,
        })
        .unwrap_or(log::LevelFilter::Info);
    tauri::Builder::default()
        // 日志插件必须最先注册（先于 dialog/updater），才能接到它们的日志
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log_level)
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepAll)
                .timezone_strategy(tauri_plugin_log::TimezoneStrategy::UseLocal)
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    // 日志放 ~/.livesub-player/logs：不用 app_log_dir（目录名以 .app 结尾，
                    // macOS 会把它当应用包，Finder 里双击/打开都报“已损坏或不完整”）
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Folder {
                        path: settings::log_dir(),
                        file_name: Some("livesub-player".into()),
                    }),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Webview),
                ])
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(player::PlayerState::default())
        .manage(asr::AsrState::default())
        .manage(asr::CompletionState::default())
        .manage(mt::MtState::default())
        .manage(models::ModelsState::default())
        .manage(transcript::TranscriptState::default())
        .manage(update::UpdateState::default())
        .invoke_handler(tauri::generate_handler![
            player::cmd_player_load,
            player::cmd_player_toggle_pause,
            player::cmd_player_seek,
            player::cmd_player_stop,
            player::cmd_scan_folder,
            player::cmd_player_set_volume,
            player::cmd_player_set_speed,
            player::cmd_player_toggle_fullscreen,
            player::cmd_player_get_state,
            player::cmd_player_screenshot,
            mt::cmd_mt_set_engine,
            mt::cmd_mt_get_engine,
            models::cmd_model_download,
            models::cmd_model_pause,
            models::cmd_model_cancel,
            models::cmd_model_status,
            models::cmd_model_delete,
            settings::cmd_get_setting,
            settings::cmd_set_setting,
            settings::cmd_open_log_dir,
            settings::cmd_open_url,
            history::cmd_history_get,
            history::cmd_history_record,
            history::cmd_history_list,
            transcript::cmd_transcript_get,
            export::cmd_export_srt,
            export::cmd_export_srt_auto,
            asr::cmd_export_full_srt,
            asr::cmd_cancel_export,
            asr::cmd_check_ffmpeg,
            asr::cmd_asr_set_model,
            update::cmd_check_update,
            update::cmd_install_update,
        ])
        .setup(|app| {
            // 任意线程 panic 落日志（尤其 mt 翻译 worker：静默死会让「译文生成中」永远挂起）
            std::panic::set_hook(Box::new(|info| {
                log::error!("[panic] {info}");
            }));

            // 每份日志开头自述：版本 + 日志目录（用户把日志发来即可定位）
            log::info!(
                "LiveSub-Player v{} 启动（日志目录：{}）",
                app.package_info().version,
                settings::log_dir().display(),
            );

            // macOS 菜单栏 = 命令中心（文件/播放/字幕），点击统一转发前端分发；
            // 「打开最近使用」随观看历史动态重建。菜单结构见 menu.rs。
            #[cfg(target_os = "macos")]
            {
                menu::refresh(app.handle());
                menu::on_menu_event(app.handle());
            }
            // 非 macOS：最小菜单保证 Ctrl+Q 可退出
            #[cfg(not(target_os = "macos"))]
            {
                use tauri::menu::{MenuBuilder, MenuItem};
                let quit = MenuItem::with_id(app, "app-quit", "退出", true, Some("Ctrl+Q"))?;
                let menu = MenuBuilder::new(app).items(&[&quit]).build()?;
                app.set_menu(menu)?;
            }

            // Windows 播放子窗口跟随窗口尺寸（mpv --wid 渲染在 WebView 之下）
            #[cfg(target_os = "windows")]
            {
                let handle = app.handle().clone();
                app.on_window_event(move |_window, event| {
                    if matches!(event, tauri::WindowEvent::Resized(_)) {
                        player::on_parent_resized(&handle);
                    }
                });
            }

            // 旧版「全局单槽上次播放」迁移为按文件的历史记录
            history::migrate_from_legacy_settings();

            // SIGTERM/SIGINT 收割：⌘Q 走 RunEvent::Exit（有收割），但强杀/watcher 重启/
            // 活动监视器走信号路径——不接住的话 llama-server（4.4GB）会变孤儿
            //（日志实锤：端口 18434→18437 递增 = 旧实例未被收割）
            #[cfg(unix)]
            {
                use signal_hook::consts::{SIGINT, SIGTERM};
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    if let Ok(mut signals) = signal_hook::iterator::Signals::new([SIGTERM, SIGINT]) {
                        for sig in signals.forever() {
                            log::info!("[app] 收到信号 {sig}，收割子进程并落盘后退出");
                            mt::shutdown(&handle);
                            transcript::flush_current(&handle);
                            std::process::exit(0);
                        }
                    }
                });
            }

            // 自动化验证钩子：LIVESUB_AUTO_PLAY=/path/to.mp4 启动即加载（仅 macOS 已接播放内核）
            #[cfg(target_os = "macos")]
            if let Ok(path) = std::env::var("LIVESUB_AUTO_PLAY") {
                if !path.is_empty() {
                    let handle = app.handle().clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(800));
                        if let Err(e) = player::player_load(&handle, path) {
                            log::error!("[player] 自动加载失败: {e}");
                        }
                        // 自动化验证钩子：LIVESUB_AUTO_SEEK=秒，LIVESUB_AUTO_SEEK_DELAY_MS=延迟（默认 4000）
                        if let Ok(t) = std::env::var("LIVESUB_AUTO_SEEK") {
                            if let Ok(t) = t.parse::<f64>() {
                                let delay: u64 = std::env::var("LIVESUB_AUTO_SEEK_DELAY_MS")
                                    .ok()
                                    .and_then(|s| s.parse().ok())
                                    .unwrap_or(4000);
                                std::thread::sleep(std::time::Duration::from_millis(delay));
                                log::info!("[player] 自动 seek 到 {t}s");
                                if let Err(e) = player::player_seek(&handle, t, true) {
                                    log::error!("[player] 自动 seek 失败: {e}");
                                }
                            }
                        }
                    });
                }
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("LiveSub-Player 构建失败")
        .run(|app_handle, event| {
            // 方案 §4.4：退出即干净——收割 llama-server sidecar，字幕/翻译缓存兜底落盘
            if let tauri::RunEvent::Exit = event {
                mt::shutdown(app_handle);
                transcript::flush_current(app_handle);
            }
        });
}
