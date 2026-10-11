//! macOS 菜单栏（HIG 惯例：命令的完整归宿在系统菜单栏，不是窗口内自制按钮）
//!
//! - 文件：打开 ⌘O / 打开文件夹 ⇧⌘O / 打开最近使用（动态重建）/ 关闭 ⌘W / 播放列表 ⇧⌘L / 导出字幕
//! - 播放：播放暂停 / 快退快进 10s / 倍速子菜单 / 截图
//! - 字幕：原文 / 译文 / 双语（对齐 1/2/3 快捷键）
//!
//! 所有自定义菜单项统一发 `menu://action`（payload = 菜单 id）给前端分发——
//! 业务逻辑都长在前端函数里（loadVideo/closeVideo/setMode/setSpeed…），后端只转发。
//!
//! 「打开最近使用」跟本机观看历史走：打开/关闭视频时整体重建菜单（set_menu 可运行时替换）。
//! 不给 播放暂停/字幕模式 绑 accelerator：空格、1/2/3 是视频区视图级快捷键，
//! 菜单 accelerator 会在事件到达 WebView 前拦截，两边都绑会双触发。

use tauri::menu::{Menu, MenuBuilder, MenuItem, Submenu, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Runtime, Wry};

const SPEED_PRESETS: [f64; 6] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

/// 重建并应用整个菜单（打开/关闭视频后调用，刷新「打开最近使用」）
pub fn refresh(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let _ = app.set_menu(build(app));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
    }
}

/// 菜单点击 → 前端统一分发
pub fn on_menu_event<R: Runtime>(app: &AppHandle<R>) {
    app.on_menu_event(move |app, event| {
        let _ = app.emit("menu://action", event.id().0.clone());
    });
}

fn recent_submenu(app: &AppHandle) -> Result<Submenu<Wry>, tauri::Error> {
    let sub = SubmenuBuilder::new(app, "打开最近使用").build()?;
    let recents: Vec<_> = crate::history::list()
        .into_iter()
        .filter(|h| h.exists)
        .take(8)
        .collect();
    if recents.is_empty() {
        let empty = MenuItem::with_id(app, "recents-empty", "（暂无记录）", false, None::<&str>)?;
        sub.append(&empty)?;
    } else {
        for h in recents {
            let item = MenuItem::with_id(app, format!("recent:{}", h.path), h.filename, true, None::<&str>)?;
            sub.append(&item)?;
        }
    }
    Ok(sub)
}

fn speed_submenu(app: &AppHandle) -> Result<Submenu<Wry>, tauri::Error> {
    let sub = SubmenuBuilder::new(app, "倍速").build()?;
    for s in SPEED_PRESETS {
        let item = MenuItem::with_id(app, format!("speed:{s}"), format!("{s}x"), true, None::<&str>)?;
        sub.append(&item)?;
    }
    Ok(sub)
}

#[cfg(target_os = "macos")]
fn build(app: &AppHandle) -> Menu<Wry> {
    build_impl(app).expect("构建菜单失败")
}

#[cfg(target_os = "macos")]
fn build_impl(app: &AppHandle) -> Result<Menu<Wry>, tauri::Error> {
    // 应用菜单（About/隐藏/退出等系统职责，保持原生样板）
    let app_submenu = SubmenuBuilder::new(app, "LiveSub-Player")
        .about(None)
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;
    let edit_submenu = SubmenuBuilder::new(app, "编辑")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;

    // 文件：打开（选视频即播，所在文件夹自动成播放列表）/ 最近使用 / 关闭 / 播放列表 / 导出
    // 目录选择器已弃用：macOS 上「进入文件夹后 Open 置灰」反直觉，文件选择器没有这个问题
    let file_submenu = SubmenuBuilder::new(app, "文件")
        .item(&MenuItem::with_id(app, "open-file", "打开…", true, Some("CmdOrCtrl+O"))?)
        .item(&recent_submenu(app)?)
        .separator()
        .item(&MenuItem::with_id(app, "close-video", "关闭", true, Some("CmdOrCtrl+W"))?)
        .separator()
        .item(&MenuItem::with_id(app, "toggle-playlist", "播放列表", true, Some("CmdOrCtrl+Shift+L"))?)
        .item(&MenuItem::with_id(app, "export-subs", "导出全片字幕…", true, None::<&str>)?)
        .build()?;

    // 播放：暂停 / 快进退 / 倍速 / 截图
    let play_submenu = SubmenuBuilder::new(app, "播放")
        .item(&MenuItem::with_id(app, "toggle-pause", "播放/暂停", true, None::<&str>)?)
        .separator()
        .item(&MenuItem::with_id(app, "seek-back", "快退 10 秒", true, Some("CmdOrCtrl+Shift+Left"))?)
        .item(&MenuItem::with_id(app, "seek-forward", "快进 10 秒", true, Some("CmdOrCtrl+Shift+Right"))?)
        .separator()
        .item(&speed_submenu(app)?)
        .item(&MenuItem::with_id(app, "screenshot", "截图到桌面", true, None::<&str>)?)
        .build()?;

    // 字幕：三模式（对齐视图级 1/2/3 快捷键，故不绑 accelerator）
    let sub_submenu = SubmenuBuilder::new(app, "字幕")
        .item(&MenuItem::with_id(app, "mode:原文", "原文", true, None::<&str>)?)
        .item(&MenuItem::with_id(app, "mode:译文", "译文", true, None::<&str>)?)
        .item(&MenuItem::with_id(app, "mode:双语", "双语", true, None::<&str>)?)
        .build()?;

    // 窗口：最小化/全屏保留原生项；close_window 移除——⌘W 语义让给「关闭视频」
    let window_submenu = SubmenuBuilder::new(app, "窗口")
        .minimize()
        .fullscreen()
        .build()?;

    Ok(MenuBuilder::new(app)
        .item(&app_submenu)
        .item(&file_submenu)
        .item(&edit_submenu)
        .item(&play_submenu)
        .item(&sub_submenu)
        .item(&window_submenu)
        .build()?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn speed_ids_round_trip_through_frontend_parse() {
        for s in super::SPEED_PRESETS {
            let id = format!("speed:{s}");
            let v = id.strip_prefix("speed:").unwrap().parse::<f64>().unwrap();
            assert_eq!(v, s);
        }
    }
}
