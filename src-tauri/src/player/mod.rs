//! M1 播放内核（平台分发）
//!
//! - macOS：libmpv 进程内 + NSView 垫底（spike-1 已验证）
//! - Windows：libmpv 动态加载（libloading）+ 子 HWND 置底嵌入，WebView 透明透出
//! - Linux：X11 subwindow 接入中，当前提供 stub 命令保证跨平台可编译

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
pub use mac::*;

#[cfg(target_os = "windows")]
mod win;
#[cfg(target_os = "windows")]
pub use win::*;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod stub;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub use stub::*;

/// 支持的视频扩展名（与前端 VIDEO_EXTS 保持一致）
pub const VIDEO_EXTS: [&str; 10] =
    ["mp4", "mkv", "avi", "mov", "webm", "m4v", "ts", "flv", "wmv", "rmvb"];

/// 扫描文件夹里的视频文件（文件名排序），供播放列表用；只扫一层不递归
#[tauri::command]
pub fn cmd_scan_folder(path: String) -> Result<Vec<String>, String> {
    let dir = std::path::PathBuf::from(&path);
    if !dir.is_dir() {
        return Err(format!("不是文件夹: {path}"));
    }
    let mut out: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| format!("读取文件夹失败: {e}"))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|x| x.to_str())
                    .map(|x| VIDEO_EXTS.contains(&x.to_ascii_lowercase().as_str()))
                    .unwrap_or(false)
        })
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_folder_lists_only_videos_sorted() {
        let dir = std::env::temp_dir().join(format!("livesub-scan-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["b.mp4", "a.MKV", "notes.txt", "c.avi"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        std::fs::create_dir_all(dir.join("subdir")).unwrap();
        let vids = cmd_scan_folder(dir.to_str().unwrap().to_string()).unwrap();
        assert_eq!(
            vids,
            vec![
                dir.join("a.MKV").to_string_lossy().into_owned(),
                dir.join("b.mp4").to_string_lossy().into_owned(),
                dir.join("c.avi").to_string_lossy().into_owned(),
            ],
            "只收视频扩展名（大小写不敏感）、按文件名排序、不进子目录"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_folder_rejects_non_dir() {
        assert!(cmd_scan_folder("/nonexistent/dir/for/sure".into()).is_err());
    }
}
