//! 设置持久化（~/.livesub-player/settings.json）：简单 KV，各模块按 key 读取
//!
//! 数据目录统一走 `data_dir()`：首次调用时把旧版 `~/.dualsub` 整体迁移为
//! `~/.livesub-player`（模型/设置/翻译缓存一次搬完，避免重复下载）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

/// 应用数据目录：~/.livesub-player（首次调用时从旧 ~/.dualsub 迁移）
pub fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    let new_dir = PathBuf::from(&home).join(".livesub-player");
    let old_dir = PathBuf::from(&home).join(".dualsub");
    if !new_dir.exists() && old_dir.is_dir() {
        // 旧目录整体改名迁移；失败不影响后续（缺目录按未下载处理）
        let _ = std::fs::rename(&old_dir, &new_dir);
    }
    new_dir
}

/// 日志目录：~/.livesub-player/logs。
/// 特意不用 app_log_dir()（~/Library/Logs/com.livesubplayer.app）——
/// 目录名以 .app 结尾时 macOS 会把它当应用包：open/Finder 双击都报“已损坏或不完整”，根本进不去。
pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

pub fn settings_file() -> PathBuf {
    data_dir().join("settings.json")
}

fn read_all() -> Value {
    std::fs::read(settings_file())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

pub fn read(key: &str) -> Option<String> {
    read_all()[key].as_str().map(|s| s.to_string())
}

pub fn write(key: &str, value: &str) -> Result<(), String> {
    let mut v = read_all();
    v[key] = Value::String(value.to_string());
    let path = settings_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    write_atomic(&path, serde_json::to_string(&v).unwrap().as_bytes())
}

/// 原子写盘：先写同目录临时文件再 rename。直写一半崩溃会损坏 JSON（设置/缓存/字幕库共用）。
/// 并发写同一目标时 rename 可能撞车，此时目标已是完整文件，按成功处理。
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
    let n = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp{n}"));
    std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            if path.exists() && !tmp.exists() {
                Ok(())
            } else {
                Err(e.to_string())
            }
        }
    }
}

// ---------- Tauri 命令 ----------
#[tauri::command]
pub fn cmd_get_setting(key: String) -> Option<String> {
    read(&key)
}

#[tauri::command]
pub fn cmd_set_setting(key: String, value: String) -> Result<(), String> {
    write(&key, &value)
}

/// 打开日志目录（设置页「打开日志文件夹」入口，排查 bug 用）。
/// 旧版日志在 app_log_dir（目录名以 .app 结尾，Finder 里无法进入），点按钮时顺手迁移到新目录。
#[tauri::command]
pub fn cmd_open_log_dir(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let dir = log_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if let Ok(old) = app.path().app_log_dir() {
        if let Ok(entries) = std::fs::read_dir(&old) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "log") {
                    let dest = dir.join(e.file_name());
                    if !dest.exists() {
                        let _ = std::fs::rename(&p, &dest);
                    }
                }
            }
        }
    }
    reveal(&dir.display().to_string(), false)?;
    log::info!("[settings] 打开日志目录: {}", dir.display());
    Ok(())
}

/// 用系统浏览器打开链接（设置页「官网 / GitHub」入口）
/// 白名单校验：前端被注入也不允许打开任意地址
#[tauri::command]
pub fn cmd_open_url(url: String) -> Result<(), String> {
    const ALLOWED: [&str; 2] = [
        "https://github.com/wu529778790/livesub-player",
        "https://livesub-player.shenzjd.com/",
    ];
    if !ALLOWED.contains(&url.as_str()) {
        return Err(format!("不允许打开链接: {url}"));
    }
    reveal(&url, true)?;
    log::info!("[settings] 打开链接: {url}");
    Ok(())
}

/// 跨平台调系统默认方式打开目录/URL，等待退出并检查状态（失败返回真实错误，不静默）。
/// 注意：macOS 上 open 的目标路径末段不能以 .app 结尾，否则会被当作应用包启动（见 log_dir 注释）。
#[cfg_attr(target_os = "macos", allow(unused_variables))]
fn reveal(target: &str, is_url: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("/usr/bin/open");
        c.arg(target);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = if is_url {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start"]).arg(target);
        c
    } else {
        let mut c = std::process::Command::new("explorer");
        c.arg(target);
        c
    };
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(target);
        c
    };
    let st = cmd.status().map_err(|e| e.to_string())?;
    if !st.success() {
        return Err(format!(
            "系统打开失败（exit {}）：{target}",
            st.code().unwrap_or(-1)
        ));
    }
    Ok(())
}
