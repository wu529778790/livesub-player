//! 字幕导出（M5+）：前端把识别结果归并成 SRT 文本，这里只负责落盘。
//!
//! 编码用 UTF-8 带 BOM：Windows 播放器（PotPlayer 等）靠 BOM 识别编码，
//! 无 BOM 时可能被误判为 ANSI 出现乱码。

use std::io::Write;
use std::path::PathBuf;

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
    let mut f = std::fs::File::create(&dest).map_err(|e| format!("无法创建文件: {e}"))?;
    f.write_all(b"\xEF\xBB\xBF").map_err(|e| e.to_string())?;
    f.write_all(content.as_bytes()).map_err(|e| e.to_string())?;
    log::info!("[export] 字幕已导出: {}", dest.display());
    Ok(dest.display().to_string())
}
