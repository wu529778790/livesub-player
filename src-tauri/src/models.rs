//! M4 模型仓库：按需下载器
//!
//! 产品决策（2026-10-12 修订）：本地翻译默认推轻量档（Hy-MT1.5-1.8B，~1.1GB），
//! 高质量档（Hunyuan-MT-7B，~4.4GB）可选；识别档位默认快速档。
//! 下载支持：hf-mirror 主源 + 官方源自动切换、断点续传（Range）、暂停/继续/取消、
//! 失败自动重试、流式写盘、进度事件。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

pub struct ModelsState {
    downloading: AtomicBool,
    /// 暂停当前下载（.part 保留，可继续）
    pause: AtomicBool,
    /// 取消当前下载（.part 删除）
    cancel: AtomicBool,
}

impl Default for ModelsState {
    fn default() -> Self {
        Self {
            downloading: AtomicBool::new(false),
            pause: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
        }
    }
}

struct ModelSpec {
    /// 按国内速度排序的下载源：首个为主源，失败自动切下一个（每个源再重试 2 次）
    sources: Vec<String>,
    /// 落盘文件名（asr/mt 按此查找；引擎为临时压缩包，解压后删除）
    dest: &'static str,
    /// 展示描述
    desc: &'static str,
    /// 非空 = 下载后从 tar.gz 中解出该成员到 bin/ 并删包（翻译引擎用）
    member: Option<&'static str>,
}

/// llama.cpp 预编译版固定 tag：含 llama-server 的 macOS arm64 构建。
const LLAMA_CPP_RELEASE: &str =
    "https://github.com/ggml-org/llama.cpp/releases/download/b11552/llama-b11552-bin-macos-arm64.tar.gz";
#[cfg(target_os = "windows")]
const LLAMA_CPP_RELEASE: &str =
    "https://github.com/ggml-org/llama.cpp/releases/download/b11552/llama-b11552-bin-win-cpu-x64.zip";

/// 下载源：魔搭（阿里云 CDN，国内实测 ~18MB/s，7 倍于 hf-mirror）优先，
/// hf-mirror 次之，HuggingFace 官方兜底
const MS: &str = "https://modelscope.cn";
const HF: &str = "https://hf-mirror.com";
const HF_ORIGIN: &str = "https://huggingface.co";

/// HF 仓库文件的双源（hf-mirror + 官方）
fn hf_sources(repo_file: &str) -> Vec<String> {
    vec![format!("{HF}/{repo_file}"), format!("{HF_ORIGIN}/{repo_file}")]
}

/// 魔搭有官方镜像的模型：魔搭主源 + HF 双源
fn ms_then_hf(ms_repo_file: &str, hf_repo_file: &str) -> Vec<String> {
    vec![
        format!("{MS}/models/{ms_repo_file}"),
        format!("{HF}/{hf_repo_file}"),
        format!("{HF_ORIGIN}/{hf_repo_file}"),
    ]
}

/// 翻译引擎落盘路径（models.rs 负责下载解压，mt.rs 按此拉起）
pub fn engine_bin() -> PathBuf {
    #[cfg(target_os = "windows")]
    return crate::settings::data_dir().join("bin").join("llama-server.exe");
    #[cfg(not(target_os = "windows"))]
    return crate::settings::data_dir().join("bin").join("llama-server");
}

/// 内置模型清单（方案 §4.3：JSON 配置内置，可随版本更新）
fn spec(name: &str) -> Option<ModelSpec> {
    match name {
        "whisper-base" => Some(ModelSpec {
            sources: hf_sources("ggerganov/whisper.cpp/resolve/main/ggml-base.bin"),
            dest: "ggml-base.bin",
            desc: "语音识别·快速档（~142MB，默认）",
            member: None,
        }),
        "whisper-small" => Some(ModelSpec {
            sources: hf_sources("ggerganov/whisper.cpp/resolve/main/ggml-small.bin"),
            dest: "ggml-small.bin",
            desc: "语音识别·均衡档（~466MB，更准）",
            member: None,
        }),
        // 引擎按平台下载预编译二进制（llama.cpp release，包内扁平：主程序 + 依赖库）
        "translation-engine" => Some(ModelSpec {
            sources: vec![LLAMA_CPP_RELEASE.to_string()],
            dest: if cfg!(target_os = "windows") { "llama-server.zip" } else { "llama-server.tar.gz" },
            desc: "本地翻译·引擎（~12MB，一次性下载）",
            member: Some("llama-server"),
        }),
        // 播放内核（仅 Windows）：libmpv-2.dll，播放器子窗口渲染用。
        // mpv-dev 无固定文件名（日期+commit），下载时动态解析最新 release
        #[cfg(target_os = "windows")]
        "player-mpv" => Some(ModelSpec {
            sources: vec!["https://api.github.com/repos/zhongfly/mpv-winbuild/releases/latest".into()],
            dest: "libmpv-2.7z",
            desc: "播放内核 libmpv（~31MB，一次性下载）",
            member: Some("libmpv-2.dll"),
        }),
        // 腾讯官方 GGUF，魔搭有镜像（tencent/HY-MT1.5-1.8B-GGUF），Q4_K_M ~1.1GB
        "translation-light" => Some(ModelSpec {
            sources: ms_then_hf(
                "Tencent-Hunyuan/HY-MT1.5-1.8B-GGUF/resolve/master/HY-MT1.5-1.8B-Q4_K_M.gguf",
                "tencent/HY-MT1.5-1.8B-GGUF/resolve/main/HY-MT1.5-1.8B-Q4_K_M.gguf",
            ),
            dest: "hunyuan-mt-light.gguf",
            desc: "本地翻译·轻量（~1.1GB，推荐）",
            member: None,
        }),
        "translation" => Some(ModelSpec {
            sources: hf_sources(
                "imikeliu/Hunyuan-MT-7B-Q4_K_M-GGUF/resolve/main/hunyuan-mt-7b-q4_k_m.gguf",
            ),
            dest: "translation.gguf",
            desc: "本地翻译·高质量（~4.4GB，更准）",
            member: None,
        }),
        _ => None,
    }
}

/// 全部已知模型（状态页展示顺序；引擎在模型之后——先有引擎模型才能用）
#[cfg(target_os = "windows")]
const KNOWN_MODELS: [&str; 6] = [
    "whisper-base",
    "whisper-small",
    "translation-light",
    "translation",
    "translation-engine",
    "player-mpv",
];
#[cfg(not(target_os = "windows"))]
const KNOWN_MODELS: [&str; 5] =
    ["whisper-base", "whisper-small", "translation-light", "translation", "translation-engine"];

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
        // 引擎的「已装」看有效性（主程序 + 依赖库齐全；压缩包下载完即删）；
        // 播放内核看解压出的 libmpv-2.dll
        let (installed_path, installed) = if name == "translation-engine" {
            (engine_bin(), engine_valid())
        } else if name == "player-mpv" {
            let dll = crate::mt::mpv_dll_path();
            (dll.clone(), dll.is_file())
        } else {
            let p = dir.join(spec.dest);
            (p.clone(), p.is_file())
        };
        let size_mb = installed_path
            .metadata()
            .map(|m| m.len() as f64 / 1048576.0)
            .unwrap_or(0.0);
        out.push(ModelStatus {
            name: name.to_string(),
            desc: spec.desc.to_string(),
            installed,
            size_mb: (size_mb * 10.0).round() / 10.0,
        });
    }
    Ok(out)
}

#[tauri::command]
pub fn cmd_model_delete(name: String) -> Result<(), String> {
    let dir = models_dir()?;
    let spec = spec(&name).ok_or(format!("未知模型: {name}"))?;
    if name == "player-mpv" {
        let dll = crate::mt::mpv_dll_path();
        if dll.is_file() {
            std::fs::remove_file(&dll).map_err(|e| e.to_string())?;
            log::info!("[models] 已删除 {}", dll.display());
        }
        let pack = dir.join(spec.dest);
        let _ = std::fs::remove_file(&pack);
        return Ok(());
    }
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
    // 引擎的「已存在」看解压后的二进制，而不是临时压缩包；播放内核看 dll
    let already = if name == "translation-engine" {
        engine_bin()
    } else if name == "player-mpv" {
        crate::mt::mpv_dll_path()
    } else {
        dest.clone()
    };
    if already.exists() {
        log::info!("[models] {} 已存在，跳过下载", already.display());
        state.downloading.store(false, Ordering::SeqCst);
        let _ = app.emit("models://done", DoneEvent { name: name.clone() });
        return Ok(());
    }
    state.pause.store(false, Ordering::SeqCst);
    state.cancel.store(false, Ordering::SeqCst);

    let app2 = app.clone();
    let name2 = name.clone();
    std::thread::spawn(move || {
        // 按源顺序下载，每源 2 次重试；暂停/取消立即返回（.part 保留供续传）
        // player-mpv：mpv-dev 无固定文件名（日期+commit），先从最新 release 动态解析
        let resolved: Vec<String> = if name2 == "player-mpv" {
            match resolve_latest_mpv_dev() {
                Ok(u) => vec![u],
                Err(e) => {
                    log::error!("[models] 解析 mpv-dev 地址失败: {e}");
                    app2.state::<ModelsState>().downloading.store(false, Ordering::SeqCst);
                    let _ = app2.emit("models://error", e);
                    return;
                }
            }
        } else {
            spec.sources.clone()
        };
        let mut last_err = String::new();
        let mut outcome: Option<DlOutcome> = None;
        'sources: for (si, src) in resolved.iter().enumerate() {
            for attempt in 0..2 {
                if si + attempt > 0 {
                    log::warn!("[models] {} 第 {} 次尝试 {}（上次：{}）", name2, si * 2 + attempt + 1, src, last_err);
                }
                match download(&app2, &name2, src, &dest) {
                    Ok(DlOutcome::Done) => {
                        outcome = Some(DlOutcome::Done);
                        break 'sources;
                    }
                    Ok(DlOutcome::Paused) => {
                        outcome = Some(DlOutcome::Paused);
                        break 'sources;
                    }
                    Ok(DlOutcome::Canceled) => {
                        outcome = Some(DlOutcome::Canceled);
                        break 'sources;
                    }
                    Err(e) => last_err = e,
                }
            }
        }

        match outcome {
            Some(DlOutcome::Done) => {
                // member 包：按格式分派——tar.gz=mac 引擎（含软链）/ zip=win 引擎（扁平）/
                // 7z=mpv 内核（抽取 libmpv-2.dll 单文件）
                if spec.member.is_some() {
                    let bin = engine_bin();
                    let extract = (|| -> Result<(), String> {
                        if let Some(parent) = bin.parent() {
                            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                        }
                        let out_dir = bin.parent().expect("bin 必有父目录");
                        match dest.extension().and_then(|e| e.to_str()) {
                            Some("gz") => extract_engine_pack(&dest, out_dir)?,
                            #[cfg(target_os = "windows")]
                            Some("zip") => extract_zip_pack(&dest, out_dir)?,
                            #[cfg(target_os = "windows")]
                            Some("7z") => extract_7z_member(&dest, spec.member.as_deref().unwrap_or("libmpv-2.dll"), out_dir)?,
                            _ => return Err(format!("未知的引擎包格式: {}", dest.display())),
                        }
                        let _ = std::fs::remove_file(&dest);
                        log::info!("[models] 引擎已解压到 {}（含全部依赖库）", bin.display());
                        Ok(())
                    })();
                    if let Err(e) = extract {
                        // 解压失败删掉残包，下次重试重新走下载分支
                        let _ = std::fs::remove_file(&dest);
                        log::error!("[models] 下载失败: {e}");
                        app2.state::<ModelsState>().downloading.store(false, Ordering::SeqCst);
                        let _ = app2.emit("models://error", e);
                        return;
                    }
                }
                log::info!("[models] {} 下载完成", name2);
                // 翻译模型/引擎就位后，恢复可能已置死的翻译可用性
                if name2.starts_with("translation") {
                    crate::mt::reset_unavailable(&app2);
                }
                let _ = app2.emit("models://done", DoneEvent { name: name2 });
            }
            Some(DlOutcome::Paused) => {
                log::info!("[models] {} 已暂停（.part 保留，可继续）", name2);
                let _ = app2.emit("models://paused", DoneEvent { name: name2 });
            }
            Some(DlOutcome::Canceled) => {
                log::info!("[models] {} 已取消", name2);
                let _ = app2.emit("models://canceled", DoneEvent { name: name2 });
            }
            None => {
                log::error!("[models] 下载失败: {last_err}");
                if spec.member.is_some() {
                    let _ = std::fs::remove_file(&dest);
                }
                let _ = app2.emit("models://error", last_err);
            }
        }
        app2.state::<ModelsState>().downloading.store(false, Ordering::SeqCst);
    });
    Ok(())
}

/// 单源单次下载的结果
enum DlOutcome {
    Done,
    /// 用户暂停：.part 保留，再次 cmd_model_download 即续传
    Paused,
    Canceled,
}

#[tauri::command]
pub fn cmd_model_pause(app: AppHandle) -> Result<(), String> {
    let st = app.state::<ModelsState>();
    if st.downloading.load(Ordering::SeqCst) {
        st.pause.store(true, Ordering::SeqCst);
    }
    Ok(())
}

#[tauri::command]
pub fn cmd_model_cancel(app: AppHandle) -> Result<(), String> {
    let st = app.state::<ModelsState>();
    if st.downloading.load(Ordering::SeqCst) {
        st.cancel.store(true, Ordering::SeqCst);
    }
    Ok(())
}

/// 从 tar.gz 解出指定文件名的成员（llama.cpp release 包内路径带版本前缀，按 basename 匹配）
/// 引擎包（llama.cpp macOS release）是扁平目录：llama-server 主程序 + 全部依赖 dylib。
/// 新版 llama.cpp 的 llama-server 是动态链接的薄启动器（~49KB），
/// 只解主程序会缺 libllama-server-impl.dylib，启动即被 dyld 杀死（2026-10-12 实锤）。
/// 因此必须把包内所有文件都解到 bin/ 下（扁平结构，rpath 按 @loader_path 解析）。
fn extract_engine_pack(archive: &Path, out_dir: &Path) -> Result<(), String> {
    let f = std::fs::File::open(archive).map_err(|e| format!("打开压缩包失败: {e}"))?;
    let gz = flate2::read::GzDecoder::new(f);
    let mut tar = tar::Archive::new(gz);
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let mut extracted = 0usize;
    for entry in tar.entries().map_err(|e| format!("读取压缩包失败: {e}"))? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let name = entry.path().map_err(|e| e.to_string())?;
        let Some(fname) = name.file_name() else { continue };
        let out = out_dir.join(fname);
        let et = entry.header().entry_type();
        // dylib 包用「无版本号软链 → 版本号真文件」布局（libllama-common.0.dylib →
        // libllama-common.0.6.0.dylib），软链漏解 = 主程序 dyld 找不到库启动即死
        if et.is_symlink() {
            let Some(target) = entry.link_name().map_err(|e| e.to_string())? else { continue };
            let _ = std::fs::remove_file(&out);
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &out).map_err(|e| format!("创建软链失败: {e}"))?;
            continue;
        }
        if !et.is_file() {
            continue;
        }
        let mut out_f = std::fs::File::create(&out).map_err(|e| format!("解压失败: {e}"))?;
        std::io::copy(&mut entry, &mut out_f).map_err(|e| format!("解压失败: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = entry.header().mode().unwrap_or(0o755);
            std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode | 0o755))
                .map_err(|e| e.to_string())?;
        }
        extracted += 1;
    }
    if extracted == 0 {
        return Err("压缩包内没有文件".into());
    }
    engine_valid_or_err(out_dir)
}

/// llama.cpp Windows 包：扁平目录（llama-server.exe + 全部依赖 dll），全部解到 bin/
#[cfg(target_os = "windows")]
fn extract_zip_pack(archive: &Path, out_dir: &Path) -> Result<(), String> {
    let f = std::fs::File::open(archive).map_err(|e| format!("打开压缩包失败: {e}"))?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| format!("读取 zip 失败: {e}"))?;
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    for i in 0..z.len() {
        let mut entry = z.by_index(i).map_err(|e| format!("读取 zip 条目失败: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let Some(fname) = entry.enclosed_name().and_then(|p| p.file_name().map(|x| x.to_owned())) else { continue };
        let out = out_dir.join(fname);
        let mut out_f = std::fs::File::create(&out).map_err(|e| format!("解压失败: {e}"))?;
        std::io::copy(&mut entry, &mut out_f).map_err(|e| format!("解压失败: {e}"))?;
    }
    Ok(())
}

/// mpv-dev 7z：整包解到临时目录后只取 libmpv-2.dll（包内还有头文件等，不落 bin/）
#[cfg(target_os = "windows")]
fn extract_7z_member(archive: &Path, member: &str, out_dir: &Path) -> Result<(), String> {
    let tmp = out_dir.join(".mpv-dev-tmp");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    sevenz_rust::decompress_file(archive, &tmp).map_err(|e| format!("解压 7z 失败: {e}"))?;
    let src = tmp.join(member);
    if !src.is_file() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!("压缩包内未找到 {member}"));
    }
    let dest = out_dir.join(member);
    std::fs::rename(&src, &dest)
        .or_else(|_| std::fs::copy(&src, &dest).map(|_| ()))
        .map_err(|e| format!("移动 {member} 失败: {e}"))?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

/// 解析 zhongfly/mpv-winbuild 最新 release 的 mpv-dev-x86_64-*.7z（排除 v3，兼容老 CPU）。
/// 包名带日期+commit 没有固定 URL，只能动态解析；每次下载查一次 API（限流 60 次/时够用）
fn resolve_latest_mpv_dev() -> Result<String, String> {
    let resp = ureq::get("https://api.github.com/repos/zhongfly/mpv-winbuild/releases/latest")
        .timeout(std::time::Duration::from_secs(30))
        .call()
        .map_err(|e| format!("查询 mpv-dev release 失败: {e}"))?;
    let text = resp.into_string().map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let assets = v["assets"].as_array().ok_or("release 响应缺 assets")?;
    for a in assets {
        let name = a["name"].as_str().unwrap_or("");
        if name.starts_with("mpv-dev-x86_64-2") && name.ends_with(".7z") {
            return Ok(a["browser_download_url"]
                .as_str()
                .ok_or("资产缺下载地址")?
                .to_string());
        }
    }
    Err("最新 release 里没有 mpv-dev-x86_64 包".into())
}

/// 引擎可用的判定：主程序 + 其依赖 dylib 都在（旧版只解主程序的安装视为未装，触发重下）
pub fn engine_valid() -> bool {
    let bin = engine_bin();
    #[cfg(target_os = "macos")]
    return bin.is_file() && bin.with_file_name("libllama-server-impl.dylib").is_file();
    #[cfg(target_os = "windows")]
    return bin.is_file() && bin.with_file_name("llama-server-impl.dll").is_file();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    return false;
}

fn engine_valid_or_err(dir: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let bin = dir.join("llama-server");
        if bin.is_file() && bin.with_file_name("libllama-server-impl.dylib").is_file() {
            return Ok(());
        }
        Err("引擎包不完整（缺少 llama-server 或其依赖 dylib）".into())
    }
    #[cfg(target_os = "windows")]
    {
        let bin = dir.join("llama-server.exe");
        if bin.is_file() && dir.join("llama-server-impl.dll").is_file() {
            return Ok(());
        }
        Err("引擎包不完整（缺少 llama-server.exe 或其依赖 dll）".into())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    Err("该平台暂无引擎".into())
}

fn download(app: &AppHandle, name: &str, url: &str, dest: &PathBuf) -> Result<DlOutcome, String> {
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
    // 进度事件间隔 2MB（前端据此计算速度/剩余时间）
    let emit_step: u64 = 2 * 1048576;
    let t0 = Instant::now();
    loop {
        // 暂停/取消：.part 保留（取消时删除），下次 Range 续传
        let st = app.state::<ModelsState>();
        if st.cancel.load(Ordering::SeqCst) {
            let _ = std::fs::remove_file(&part);
            return Ok(DlOutcome::Canceled);
        }
        if st.pause.load(Ordering::SeqCst) {
            return Ok(DlOutcome::Paused);
        }
        drop(st);
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
                    let _ = t0; // 速度由前端按事件时间差计算
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
    Ok(DlOutcome::Done)
}
