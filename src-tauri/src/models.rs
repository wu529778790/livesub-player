//! M4 模型仓库（第一片）：按需下载器
//!
//! 产品决策（用户拍板）：安装包不内置模型（~30MB），模型按需下载——
//! 在线翻译默认可用；被过滤/断网时引导用户点「下载本地模型」落回离线兜底。
//! 支持：hf-mirror 镜像源、流式写盘、断点续传（Range）、进度事件。

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

pub struct ModelsState {
    downloading: AtomicBool,
}

impl Default for ModelsState {
    fn default() -> Self {
        Self {
            downloading: AtomicBool::new(false),
        }
    }
}

struct ModelSpec {
    /// 下载直链（模型走 hf-mirror 镜像，国内可达优先；引擎走 llama.cpp release）
    url: &'static str,
    /// 落盘文件名（asr/mt 按此查找；引擎为临时压缩包，解压后删除）
    dest: &'static str,
    /// 展示描述
    desc: &'static str,
    /// 非空 = 下载后从 tar.gz 中解出该成员到 bin/ 并删包（翻译引擎用）
    member: Option<&'static str>,
}

/// llama.cpp 预编译版固定 tag：含 llama-server 的 macOS arm64 构建。
/// 固定而非追新：本地翻译走 OpenAI 兼容 HTTP，引擎版本无需跟模型同步升。
const LLAMA_CPP_RELEASE: &str =
    "https://github.com/ggml-org/llama.cpp/releases/download/b11552/llama-b11552-bin-macos-arm64.tar.gz";

/// 翻译引擎落盘路径（models.rs 负责下载解压，mt.rs 按此拉起）
pub fn engine_bin() -> PathBuf {
    crate::settings::data_dir().join("bin").join("llama-server")
}

/// 内置模型清单（方案 §4.3：JSON 配置内置，可随版本更新）
fn spec(name: &str) -> Option<ModelSpec> {
    match name {
        "whisper-base" => Some(ModelSpec {
            url: "https://hf-mirror.com/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
            dest: "ggml-base.bin",
            desc: "语音识别·快速档（~142MB，低配机推荐）",
            member: None,
        }),
        "whisper-small" => Some(ModelSpec {
            url: "https://hf-mirror.com/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
            dest: "ggml-small.bin",
            desc: "语音识别·均衡档（~466MB，默认）",
            member: None,
        }),
        // 引擎是 macOS 预编译二进制，其余平台发布物暂不可用，不列出
        #[cfg(target_os = "macos")]
        "translation-engine" => Some(ModelSpec {
            url: LLAMA_CPP_RELEASE,
            dest: "llama-server.tar.gz",
            desc: "本地翻译·引擎 llama-server（~12MB，一次性下载）",
            member: Some("llama-server"),
        }),
        "translation" => Some(ModelSpec {
            url: "https://hf-mirror.com/imikeliu/Hunyuan-MT-7B-Q4_K_M-GGUF/resolve/main/hunyuan-mt-7b-q4_k_m.gguf",
            dest: "translation.gguf",
            desc: "本地翻译·混元 MT 7B（~4.4GB，全离线无屏蔽）",
            member: None,
        }),
        _ => None,
    }
}

/// 全部已知模型（状态页展示顺序；引擎在模型之前——先有引擎模型才能用）
const KNOWN_MODELS: [&str; 4] = ["whisper-small", "whisper-base", "translation-engine", "translation"];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    name: String,
    desc: String,
    installed: bool,
    size_mb: f64,
}

#[tauri::command]
pub fn cmd_model_status() -> Result<Vec<ModelStatus>, String> {
    let dir = models_dir()?;
    let mut out = Vec::new();
    for name in KNOWN_MODELS {
        let Some(spec) = spec(name) else { continue };
        // 引擎的「已装」看解压后的二进制（压缩包下载完即删）
        let installed_path = if spec.member.is_some() { engine_bin() } else { dir.join(spec.dest) };
        let size_mb = installed_path
            .metadata()
            .map(|m| m.len() as f64 / 1048576.0)
            .unwrap_or(0.0);
        out.push(ModelStatus {
            name: name.to_string(),
            desc: spec.desc.to_string(),
            installed: installed_path.is_file(),
            size_mb: (size_mb * 10.0).round() / 10.0,
        });
    }
    Ok(out)
}

#[tauri::command]
pub fn cmd_model_delete(name: String) -> Result<(), String> {
    let dir = models_dir()?;
    let spec = spec(&name).ok_or(format!("未知模型: {name}"))?;
    if spec.member.is_some() {
        let bin = engine_bin();
        if bin.is_file() {
            std::fs::remove_file(&bin).map_err(|e| e.to_string())?;
            log::info!("[models] 已删除 {}", bin.display());
        }
        let tarball = dir.join(spec.dest);
        if tarball.is_file() {
            let _ = std::fs::remove_file(&tarball);
        }
        return Ok(());
    }
    let p = dir.join(spec.dest);
    if p.is_file() {
        std::fs::remove_file(&p).map_err(|e| e.to_string())?;
        log::info!("[models] 已删除 {}", p.display());
    }
    Ok(())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressEvent {
    name: String,
    downloaded_mb: f64,
    total_mb: f64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoneEvent {
    name: String,
}

fn models_dir() -> Result<PathBuf, String> {
    let dir = crate::settings::data_dir().join("models");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

#[tauri::command]
pub fn cmd_model_download(app: AppHandle, name: String) -> Result<(), String> {
    let state = app.state::<ModelsState>();
    if state.downloading.swap(true, Ordering::SeqCst) {
        return Err("已有下载任务进行中".into());
    }

    let Some(spec) = spec(&name) else {
        state.downloading.store(false, Ordering::SeqCst);
        return Err(format!("未知模型: {name}"));
    };
    let dir = models_dir()?;
    let dest = dir.join(spec.dest);
    // 引擎的「已存在」看解压后的二进制，而不是临时压缩包
    let already = if spec.member.is_some() { engine_bin() } else { dest.clone() };
    if already.exists() {
        log::info!("[models] {} 已存在，跳过下载", already.display());
        state.downloading.store(false, Ordering::SeqCst);
        let _ = app.emit("models://done", DoneEvent { name: name.clone() });
        return Ok(());
    }

    let app2 = app.clone();
    let name2 = name.clone();
    let member = spec.member;
    std::thread::spawn(move || {
        let r = download(&app2, &name2, spec.url, &dest).and_then(|()| {
            // 引擎：从 tar.gz 解出 llama-server 到 bin/，随后删包
            if let Some(member) = member {
                let bin = engine_bin();
                if let Some(parent) = bin.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                extract_member_tar_gz(&dest, member, &bin)?;
                let _ = std::fs::remove_file(&dest);
                log::info!("[models] 引擎已解压到 {}", bin.display());
            }
            Ok(())
        });
        app2.state::<ModelsState>().downloading.store(false, Ordering::SeqCst);
        match r {
            Ok(()) => {
                log::info!("[models] {} 下载完成", name2);
                // 本地翻译二件套（引擎/模型）就位后，恢复可能已置死的翻译可用性
                if name2 == "translation" || name2 == "translation-engine" {
                    crate::mt::reset_unavailable(&app2);
                }
                let _ = app2.emit("models://done", DoneEvent { name: name2 });
            }
            Err(e) => {
                log::error!("[models] 下载失败: {e}");
                // 解压失败时删掉残包，下次重试才能重新走下载分支
                if member.is_some() {
                    let _ = std::fs::remove_file(&dest);
                }
                let _ = app2.emit("models://error", e);
            }
        }
    });
    Ok(())
}

/// 从 tar.gz 解出指定文件名的成员（llama.cpp release 包内路径带版本前缀，按 basename 匹配）
fn extract_member_tar_gz(archive: &PathBuf, member: &str, out: &PathBuf) -> Result<(), String> {
    let f = std::fs::File::open(archive).map_err(|e| format!("打开压缩包失败: {e}"))?;
    let gz = flate2::read::GzDecoder::new(f);
    let mut tar = tar::Archive::new(gz);
    for entry in tar.entries().map_err(|e| format!("读取压缩包失败: {e}"))? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = entry.path().map_err(|e| e.to_string())?;
        if name.file_name().and_then(|s| s.to_str()) == Some(member) {
            let mut out_f = std::fs::File::create(out).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out_f).map_err(|e| format!("解压失败: {e}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o755))
                    .map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
    }
    Err(format!("压缩包内未找到 {member}"))
}

fn download(app: &AppHandle, name: &str, url: &str, dest: &PathBuf) -> Result<(), String> {
    let mut builder = ureq::AgentBuilder::new().timeout_read(Duration::from_secs(60));
    if let Some(p) = crate::mt::env_proxy() {
        builder = builder.proxy(p);
    }
    let agent = builder.build();

    // 断点续传：.part 已有内容则 Range 续传
    let part = dest.with_extension("part");
    let mut start: u64 = part.metadata().map(|m| m.len()).unwrap_or(0);
    if start > 0 {
        log::info!("[models] 断点续传自 {:.1}MB", start as f64 / 1048576.0);
    }

    let mut req = agent.get(url);
    if start > 0 {
        req = req.set("Range", &format!("bytes={start}-"));
    }
    let resp = req.call().map_err(|e| format!("下载请求失败: {e}"))?;
    let total: u64 = resp
        .header("content-length")
        .and_then(|h| h.parse::<u64>().ok())
        .map(|len| len + start)
        .unwrap_or(0);
    let mut reader = resp.into_reader();

    let mut file = if start > 0 {
        std::fs::OpenOptions::new()
            .append(true)
            .open(&part)
            .map_err(|e| e.to_string())?
    } else {
        std::fs::File::create(&part).map_err(|e| e.to_string())?
    };

    let mut buf = [0u8; 256 * 1024];
    let mut last_emit = 0u64;
    // 大文件每 ~16MB 发一次进度；小文件（引擎 ~12MB）每 1MB 发一次，否则全程 0%
    let emit_step: u64 = if total > 0 && total < 64 * 1048576 { 1048576 } else { 16 * 1048576 };
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                start += n as u64;
                if total > 0 && start - last_emit > emit_step {
                    last_emit = start;
                    let _ = app.emit(
                        "models://progress",
                        ProgressEvent {
                            name: name.to_string(),
                            downloaded_mb: start as f64 / 1048576.0,
                            total_mb: total as f64 / 1048576.0,
                        },
                    );
                }
            }
            Err(e) => return Err(format!("下载中断: {e}")),
        }
    }

    // sha256 校验留 M4 完整版（清单带 hash）；此处以文件大小兜底
    if total > 0 && start < total {
        return Err(format!("下载不完整（{}/{}）", start, total));
    }
    std::fs::rename(&part, dest).map_err(|e| e.to_string())?;
    Ok(())
}
