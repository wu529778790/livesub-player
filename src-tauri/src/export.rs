//! 字幕导出（M5+）：前端把识别结果归并成 SRT 文本，这里只负责落盘。
//!
//! 编码用 UTF-8 带 BOM：Windows 播放器（PotPlayer 等）靠 BOM 识别编码，
//! 无 BOM 时可能被误判为 ANSI 出现乱码。

use std::io::Write;
use std::path::{Path, PathBuf};

#[tauri::command]
pub fn cmd_export_srt(path: String, content: String) -> Result<String, String> {
    let mut dest = PathBuf::from(&path);
    let ext = dest
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    if ext.as_deref() != Some("srt") {
        dest.set_extension("srt");
    }
    write_srt(&dest, &content)
}

/// 保存对话框被取消时的兜底：自动落到「桌面」，避免几十分钟的识别成果一次误点归零
#[tauri::command]
pub fn cmd_export_srt_auto(content: String, video_path: String) -> Result<String, String> {
    let stem = Path::new(&video_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("subtitles")
        .to_string();
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| crate::settings::data_dir());
    let mut dir = home.join("Desktop");
    if !dir.is_dir() {
        dir = home;
    }
    let mut dest = dir.join(format!("{stem}.srt"));
    let mut n = 1;
    while dest.exists() {
        dest = dir.join(format!("{stem}-{n}.srt"));
        n += 1;
    }
    let shown = write_srt(&dest, &content)?;
    log::info!("[export] 用户取消保存对话框，已自动落盘 {}", dest.display());
    Ok(shown)
}

fn write_srt(dest: &PathBuf, content: &str) -> Result<String, String> {
    let mut f = std::fs::File::create(dest).map_err(|e| format!("无法创建文件: {e}"))?;
    f.write_all(b"\xEF\xBB\xBF").map_err(|e| e.to_string())?;
    f.write_all(content.as_bytes()).map_err(|e| e.to_string())?;
    log::info!("[export] 字幕已导出: {}", dest.display());
    Ok(dest.display().to_string())
}
