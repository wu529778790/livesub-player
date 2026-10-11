//! 观看历史（~/.livesub-player/history.json）：按文件记忆播放进度 + 最近播放列表。
//!
//! 与旧版「全局单槽 last_video / last_video_pos」的区别：
//! - 进度跟文件走——看了 A 十分钟再开 B，A 的进度不丢，重开 A 续播
//! - 文件身份 = 路径 + 大小 + mtime：内容变过（重新压制/换版本）不沿用旧进度
//! - 启动不再自动打开上次文件；续播只发生在「用户打开某个文件」时（前端决定）
//!
//! 容量上限 50 条，超出按 updated_at 淘汰最旧的。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const MAX_ENTRIES: usize = 50;

#[derive(Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub path: String,
    pub size: u64,
    pub mtime: u64,
    pub position: f64,
    pub duration: f64,
    pub updated_at: u64,
}

#[derive(Serialize)]
pub struct HistoryItem {
    pub path: String,
    pub filename: String,
    pub position: f64,
    pub duration: f64,
    pub updated_at: u64,
    pub exists: bool,
}

#[derive(Serialize, Deserialize)]
struct HistoryFile {
    version: u32,
    entries: Vec<HistoryEntry>,
}

fn history_file() -> PathBuf {
    crate::settings::data_dir().join("history.json")
}

fn load() -> HistoryFile {
    std::fs::read(history_file())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| HistoryFile { version: 1, entries: vec![] })
}

fn save(f: &HistoryFile) {
    let path = history_file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec(f) {
        let _ = crate::settings::write_atomic(&path, &bytes);
    }
}

/// 文件身份（大小, mtime 秒）；元数据读不到时为 (0, 0)（身份退化为纯路径）
fn identity(path: &str) -> (u64, u64) {
    match std::fs::metadata(path) {
        Ok(m) => {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (m.len(), mtime)
        }
        Err(_) => (0, 0),
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 记录/更新一条播放进度（前端播放中每 3s 调一次）
pub fn record(path: &str, position: f64, duration: f64) {
    if path.is_empty() {
        return;
    }
    let (size, mtime) = identity(path);
    let mut f = load();
    let entry = match f.entries.iter_mut().find(|e| e.path == path) {
        Some(e) => {
            // 文件内容变过：进度作废，重新计
            if e.size != size || e.mtime != mtime {
                e.size = size;
                e.mtime = mtime;
                e.duration = 0.0;
            }
            e
        }
        None => {
            f.entries.push(HistoryEntry {
                path: path.to_string(),
                size,
                mtime,
                position: 0.0,
                duration: 0.0,
                updated_at: 0,
            });
            f.entries.last_mut().unwrap()
        }
    };
    entry.position = position.max(0.0);
    if duration > 0.0 {
        entry.duration = duration;
    }
    entry.updated_at = now_secs();
    f.entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    f.entries.truncate(MAX_ENTRIES);
    save(&f);
}

/// 查某文件的续播记录；身份不符（文件变过）视为无记录
pub fn get(path: &str) -> Option<HistoryEntry> {
    let (size, mtime) = identity(path);
    load()
        .entries
        .into_iter()
        .find(|e| e.path == path && e.size == size && e.mtime == mtime)
}

/// 最近播放列表（updated_at 倒序，供欢迎页展示）
pub fn list() -> Vec<HistoryItem> {
    load()
        .entries
        .into_iter()
        .map(|e| {
            let filename = std::path::Path::new(&e.path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| e.path.clone());
            let exists = std::path::Path::new(&e.path).is_file();
            HistoryItem {
                path: e.path,
                filename,
                position: e.position,
                duration: e.duration,
                updated_at: e.updated_at,
                exists,
            }
        })
        .collect()
}

/// 旧版单槽（settings 里的 last_video / last_video_pos）迁移成 history.json 首条记录
pub fn migrate_from_legacy_settings() {
    if history_file().exists() {
        return;
    }
    let Some(last) = crate::settings::read("last_video") else {
        return;
    };
    if last.is_empty() {
        return;
    }
    let pos: f64 = crate::settings::read("last_video_pos")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    log::info!("[history] 迁移旧版上次播放记录: {last} @ {pos:.0}s");
    record(&last, pos, 0.0);
}

// ---------- Tauri 命令 ----------
#[tauri::command]
pub fn cmd_history_get(path: String) -> Option<HistoryEntry> {
    get(&path)
}

#[tauri::command]
pub fn cmd_history_record(path: String, position: f64, duration: f64) -> Result<(), String> {
    record(&path, position, duration);
    Ok(())
}

#[tauri::command]
pub fn cmd_history_list() -> Vec<HistoryItem> {
    list()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_matches_only_when_file_unchanged() {
        // 只对真实文件做元数据断言，不碰 history.json 本身
        let dir = std::env::temp_dir().join(format!("livesub-history-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("v.mp4");
        std::fs::write(&p, b"hello").unwrap();
        let path = p.to_str().unwrap();
        let (size, mtime) = identity(path);
        assert_eq!(size, 5);
        assert!(mtime > 0);
        assert_eq!(identity(path), (size, mtime), "同一文件两次读取身份一致");
        // 文件内容变了 → mtime/大小至少一项变
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&p, b"hello world").unwrap();
        assert_ne!(identity(path), (size, mtime), "文件被替换后身份必须变化");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_has_neutral_identity() {
        assert_eq!(identity("/nonexistent/no/such/file.mp4"), (0, 0));
    }
}
