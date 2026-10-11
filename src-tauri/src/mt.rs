//! M3 翻译管线（技术方案 §4.2）
//!
//! - llama-server sidecar 常驻（OpenAI 兼容 /v1/chat/completions），懒启动、健康检查、退出收割
//! - 攒批翻译：4 句一批（方案：3–5 句带上下文，译文连贯且省 token），2s 超时触发；
//!   就近优先——离播放位置最近的先译，seek 后译文跟上的永远是正在看的部分
//! - 翻译缓存：(源文本, 源语言, 目标语言) 键——M3 固定 zh 目标，键取源文本；内存 + 磁盘 JSON
//!   （原子写 + 节流落盘，不再每批全量重写）
//! - 降级：模型/binary 缺失时标记不可用，播放照常（「看剧永不停」原则），前端隐藏译文行
//! - 字幕库联动：打开视频时把库里未译的句全部入队（缓存命中直接回放），全片补全
//!   灌进来的句子会一直译到完

use std::collections::HashMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

/// 每批最大句数（方案 §4.2：3–5 句）
const BATCH_LINES: usize = 4;
/// 攒批超时：首句入队后等这么久没凑满就翻译
const BATCH_TIMEOUT_MS: u64 = 2000;
/// 等待池上限（全片补全会一次性灌入整部电影的句数）
const POOL_CAP: usize = 10000;
/// 单句引擎失败最大重试次数（重开视频会重新入队再试）
const MAX_ATTEMPTS: u32 = 3;
/// 翻译缓存落盘节流
const CACHE_SAVE_INTERVAL_MS: u128 = 5000;
const BASE_PORT: u16 = 18434;
const ONLINE_TIMEOUT_SECS: u64 = 10;

// ---------- 翻译引擎选择（产品决策：在线优先，本地兜底） ----------
#[derive(Clone, Copy, PartialEq)]
enum Engine {
    Auto,
    OnlineOnly,
    LocalOnly,
}

impl Engine {
    #[allow(dead_code)]
    fn from_str(s: &str) -> Self {
        match s {
            "online" => Engine::OnlineOnly,
            "local" => Engine::LocalOnly,
            _ => Engine::Auto,
        }
    }
}

fn engine_setting() -> Engine {
    match crate::settings::read("mt_engine").as_deref() {
        Some("online") => Engine::OnlineOnly,
        Some("local") => Engine::LocalOnly,
        _ => Engine::Auto,
    }
}

/// 从环境变量构造代理（ureq 2.12 的系统代理 API 未公开，自行实现）
pub fn env_proxy() -> Option<ureq::Proxy> {
    for k in ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        if let Ok(v) = std::env::var(k) {
            if let Ok(p) = ureq::Proxy::new(v) {
                return Some(p);
            }
        }
    }
    None
}

// ---------- 在线免费翻译（Google gtx 端点；需网络可达，尊重代理环境变量） ----------
fn online_translate(lines: &[&str]) -> Result<Vec<String>, String> {
    let mut builder = ureq::AgentBuilder::new().timeout(Duration::from_secs(ONLINE_TIMEOUT_SECS));
    if let Some(p) = env_proxy() {
        builder = builder.proxy(p);
    }
    let agent = builder.build();

    let mut outs = Vec::with_capacity(lines.len());
    for line in lines {
        let resp = agent
            .get("https://translate.googleapis.com/translate_a/single")
            .query("client", "gtx")
            .query("sl", "auto")
            .query("tl", "zh-CN")
            .query("dt", "t")
            .query("q", line)
            .call()
            .map_err(|e| format!("在线翻译请求失败: {e}"))?;
        let text = resp.into_string().map_err(|e| e.to_string())?;
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        // 响应结构 v[0] = [[trans, orig, ...], ...]，拼接所有分段的译文
        let segs = v[0]
            .as_array()
            .ok_or("在线翻译响应格式异常")?;
        let mut out = String::new();
        for seg in segs {
            if let Some(t) = seg[0].as_str() {
                out.push_str(t);
            }
        }
        if out.trim().is_empty() {
            return Err("在线翻译返回空结果（可能被过滤）".into());
        }
        outs.push(out);
    }
    Ok(outs)
}

pub struct MtState {
    tx: Mutex<Option<Sender<TransJob>>>,
    child: Mutex<Option<std::process::Child>>,
    cache: Mutex<HashMap<String, String>>,
    /// 翻译服务可用性（false 后前端隐藏译文行）
    pub unavailable: AtomicBool,
    /// 收割中（⌘Q/信号退出）：在途请求被杀出的失败不再记 ERROR/发事件
    shutting_down: AtomicBool,
}

impl Default for MtState {
    fn default() -> Self {
        Self {
            tx: Mutex::new(None),
            child: Mutex::new(None),
            cache: Mutex::new(HashMap::new()),
            unavailable: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
        }
    }
}

#[derive(Debug)]
struct TransJob {
    start: f64,
    end: f64,
    text: String,
    attempts: u32,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TranslationEvent {
    start: f64,
    end: f64,
    src: String,
    dst: String,
}

// ---------- 对 asr 的接口 ----------
/// 懒启动 worker 并拿到发送端
fn worker_tx(app: &AppHandle) -> Sender<TransJob> {
    let st = app.state::<MtState>();
    let mut tx_slot = st.tx.lock().unwrap();
    if tx_slot.is_none() {
        let (tx, rx) = mpsc::channel::<TransJob>();
        let app2 = app.clone();
        std::thread::spawn(move || worker(app2, rx));
        *tx_slot = Some(tx);
    }
    tx_slot.as_ref().unwrap().clone()
}

pub fn enqueue(app: &AppHandle, start: f64, end: f64, text: &str) {
    if app.state::<MtState>().unavailable.load(Ordering::Relaxed) {
        return;
    }
    let _ = worker_tx(app).send(TransJob {
        start,
        end,
        text: text.to_string(),
        attempts: 0,
    });
}

/// 字幕库载入后：把库里所有非中文句交给翻译管线。缓存命中的直接回放上屏，
/// 未命中的排队翻译——配合全片补全，译文会一直补到全片完成。
pub fn enqueue_transcript(app: &AppHandle) {
    if app.state::<MtState>().unavailable.load(Ordering::Relaxed) {
        return;
    }
    let Some(ts) = crate::transcript::current(app) else {
        return;
    };
    let segs = ts.t.lock().unwrap().segments.clone();
    if segs.is_empty() {
        return;
    }
    let tx = worker_tx(app);
    let mut n = 0usize;
    for seg in segs {
        if is_mostly_chinese(&seg.text) {
            continue;
        }
        let _ = tx.send(TransJob {
            start: seg.start,
            end: seg.end,
            text: seg.text,
            attempts: 0,
        });
        n += 1;
    }
    if n > 0 {
        log::info!("[mt] 字幕库 {n} 句入翻译队列（缓存命中直接回放）");
    }
}

/// 主进程退出时收割 llama-server（方案 §4.4：退出即干净）+ 缓存兜底落盘
pub fn shutdown(app: &AppHandle) {
    let st = app.state::<MtState>();
    st.shutting_down.store(true, Ordering::Relaxed);
    {
        let mut guard = st.child.lock().unwrap();
        if let Some(mut child) = guard.take() {
            log::info!("[mt] 收割 llama-server (pid {:?})", child.id());
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    save_cache(&st.cache.lock().unwrap(), &cache_file());
}

// ---------- 引擎设置命令 ----------
#[tauri::command]
pub fn cmd_mt_set_engine(app: AppHandle, engine: String) -> Result<(), String> {
    if !matches!(engine.as_str(), "auto" | "online" | "local") {
        return Err(format!("未知引擎: {engine}"));
    }
    // 重置不可用标记：切引擎给一次重新尝试的机会
    crate::settings::write("mt_engine", &engine)?;
    reset_unavailable(&app);
    log::info!("[mt] 翻译引擎 → {engine}");
    Ok(())
}

/// 恢复翻译可用性并通知前端（模型/引擎下载完成后、切引擎时调用）
pub fn reset_unavailable(app: &AppHandle) {
    app.state::<MtState>().unavailable.store(false, Ordering::Relaxed);
    let _ = app.emit("mt://available", ());
    // 引擎恢复/装好：字幕库里还没译的句子继续补齐
    enqueue_transcript(app);
}

#[tauri::command]
pub fn cmd_mt_get_engine() -> String {
    match engine_setting() {
        Engine::OnlineOnly => "online".into(),
        Engine::LocalOnly => "local".into(),
        Engine::Auto => "auto".into(),
    }
}

// ---------- worker：攒批 + 就近优先 ----------
struct Worker {
    pool: Vec<TransJob>,
    last_save: Instant,
    cache_path: PathBuf,
}

fn worker(app: AppHandle, rx: Receiver<TransJob>) {
    // 启动时加载磁盘缓存
    let cache_path = cache_file();
    {
        let st = app.state::<MtState>();
        if let Ok(bytes) = std::fs::read(&cache_path) {
            // 注意与 save_cache 的 CacheFile{version, entries} 结构一致——
            // 旧代码按裸 HashMap 解析，永远失败，跨启动缓存形同虚设
            if let Ok(file) = serde_json::from_slice::<CacheFile>(&bytes) {
                log::info!("[mt] 磁盘缓存载入 {} 条", file.entries.len());
                *st.cache.lock().unwrap() = file.entries;
            }
        }
    }

    let mut w = Worker {
        pool: Vec::new(),
        // 刻意设为「已过期」：首条译文落盘不等 5s 节流——单句短视频被杀时也不丢缓存
        last_save: Instant::now() - Duration::from_millis(CACHE_SAVE_INTERVAL_MS as u64 + 1000),
        cache_path,
    };
    loop {
        // 凑满一批立即翻译；没凑满则等攒批超时
        if w.pool.len() >= BATCH_LINES {
            w.flush(&app);
            continue;
        }
        match rx.recv_timeout(Duration::from_millis(if w.pool.is_empty() {
            u64::MAX / 2 // 池空：长等
        } else {
            BATCH_TIMEOUT_MS
        })) {
            Ok(job) => {
                if w.pool.len() < POOL_CAP {
                    w.pool.push(job);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !w.pool.is_empty() {
                    w.flush(&app);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

impl Worker {
    /// 就近优先：离播放位置最近的先译——seek 之后译文跟上的永远是正在看的部分
    fn flush(&mut self, app: &AppHandle) {
        if app.state::<MtState>().shutting_down.load(Ordering::Relaxed) {
            // 收割中：排空待翻队列即可，不再发请求
            self.pool.clear();
            return;
        }
        let st = app.state::<MtState>();
        let play = crate::asr::playback_pos(app);
        self.pool
            .sort_by(|a, b| (a.start - play).abs().total_cmp(&(b.start - play).abs()));
        let take = self.pool.len().min(BATCH_LINES);
        let batch: Vec<TransJob> = self.pool.drain(..take).collect();

        // 1. 缓存命中直接出
        let mut uncached: Vec<TransJob> = Vec::new();
        {
            let cache = st.cache.lock().unwrap();
            for job in batch {
                match cache.get(&job.text) {
                    Some(dst) => emit_translation(app, &job, dst),
                    None => uncached.push(job),
                }
            }
        }
        if uncached.is_empty() {
            return;
        }

        let lines: Vec<&str> = uncached.iter().map(|j| j.text.as_str()).collect();

        // 2. 按引擎路由（产品决策：自动=在线优先→本地兜底；失败时提示下载本地模型）
        let engine = engine_setting();
        log::info!(
            "[mt] 引擎={}，批 {} 句（池 {}）",
            match engine {
                Engine::Auto => "auto",
                Engine::OnlineOnly => "online",
                Engine::LocalOnly => "local",
            },
            lines.len(),
            self.pool.len()
        );
        // 收割中的失败 = 在途请求被杀，静默处理（不刷 ERROR、不发引导事件）
        let shutting_down = || app.state::<MtState>().shutting_down.load(Ordering::Relaxed);
        let result = match engine {
            Engine::OnlineOnly => online_translate(&lines).map_err(|e| {
                if !shutting_down() {
                    log::error!("[mt] 在线翻译失败: {e}");
                    let _ = app.emit("mt://need-local-model", "在线翻译不可用");
                }
                e
            }),
            Engine::LocalOnly => local_translate(app, &lines).map_err(|e| {
                if !shutting_down() {
                    log::error!("[mt] 本地翻译失败: {e}");
                    app.state::<MtState>()
                        .unavailable
                        .store(true, Ordering::Relaxed);
                    let _ = app.emit("mt://unavailable", e.clone());
                }
                e
            }),
            Engine::Auto => match online_translate(&lines) {
                Ok(o) => Ok(o),
                Err(e) => {
                    if !shutting_down() {
                        log::warn!("[mt] 在线翻译失败（{e}），尝试本地兜底");
                    }
                    match local_translate(app, &lines) {
                        Ok(o) => Ok(o),
                        Err(e2) => {
                            if !shutting_down() {
                                log::error!("[mt] 本地兜底失败: {e2}");
                                let _ = app.emit(
                                    "mt://need-local-model",
                                    "在线翻译不可用，可下载本地模型兜底",
                                );
                            }
                            Err(e2)
                        }
                    }
                }
            },
        };

        // 3. 成功：入库 + 上屏 + 节流落盘；失败：重新入池（退避重试）
        match result {
            Ok(outs) if outs.len() == lines.len() => {
                {
                    let mut cache = st.cache.lock().unwrap();
                    for (job, dst) in uncached.into_iter().zip(outs.into_iter()) {
                        let dst = dst.trim().to_string();
                        if !dst.is_empty() {
                            cache.insert(job.text.clone(), dst.clone());
                        }
                        emit_translation(app, &job, &dst);
                    }
                }
                if self.last_save.elapsed().as_millis() > CACHE_SAVE_INTERVAL_MS {
                    save_cache(&st.cache.lock().unwrap(), &self.cache_path);
                    self.last_save = Instant::now();
                }
            }
            Ok(_) | Err(_) => {
                if !shutting_down() {
                    self.requeue(uncached);
                }
            }
        }
    }

    /// 失败重试：重新入池（带次数上限），并退避一拍——别把故障的引擎往死里打
    fn requeue(&mut self, jobs: Vec<TransJob>) {
        let mut dropped = 0usize;
        for mut job in jobs {
            job.attempts += 1;
            if job.attempts <= MAX_ATTEMPTS {
                self.pool.push(job);
            } else {
                dropped += 1;
            }
        }
        if dropped > 0 {
            log::warn!(
                "[mt] {dropped} 句重试 {MAX_ATTEMPTS} 次仍失败，本轮放弃（重开视频会再试）"
            );
        }
        std::thread::sleep(Duration::from_millis(BATCH_TIMEOUT_MS));
    }
}

/// 本地 sidecar 翻译（llama-server）
fn local_translate(app: &AppHandle, lines: &[&str]) -> Result<Vec<String>, String> {
    let port = ensure_server(app)?;
    let outs = translate_batch(port, lines)?;
    if outs.len() != lines.len() {
        return Err(format!("批量输出行数不齐（{}/{}）", outs.len(), lines.len()));
    }
    Ok(outs)
}

fn emit_translation(app: &AppHandle, job: &TransJob, dst: &str) {
    if dst.is_empty() {
        return;
    }
    let _ = app.emit(
        "asr://translation",
        TranslationEvent {
            start: job.start,
            end: job.end,
            src: job.text.clone(),
            dst: dst.to_string(),
        },
    );
    log::debug!("[mt] 译文: {}", dst);
}

// ---------- llama-server sidecar ----------
fn find_model() -> Option<PathBuf> {
    let dir = crate::settings::data_dir().join("models");
    for name in ["translation.gguf", "translation-tiny.gguf"] {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// sidecar 端口（进程内单实例，拉起时定档）
static SERVER_PORT: Mutex<Option<u16>> = Mutex::new(None);

fn ensure_server(app: &AppHandle) -> Result<u16, String> {
    let st = app.state::<MtState>();
    if st.child.lock().unwrap().is_some() {
        return Ok(SERVER_PORT.lock().unwrap().unwrap_or(BASE_PORT));
    }

    let Some(model) = find_model() else {
        return Err("未找到本地翻译模型：请在 设置 → 识别·翻译 中下载「本地翻译·混元 MT」（约 4.4GB）".into());
    };
    let Some(engine) = llama_server_path() else {
        return Err("未找到本地翻译引擎：请在 设置 → 识别·翻译 中下载「本地翻译·引擎」（约 12MB）".into());
    };

    let port = pick_port().ok_or("找不到可用端口")?;
    log::info!("[mt] 拉起 llama-server :{port}（{}）", model.display());
    let child = std::process::Command::new(&engine)
        .args([
            "-m",
            model.to_str().ok_or("模型路径非法")?,
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "-c",
            "4096",
            "-t",
            "4",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("llama-server 启动失败: {e}"))?;
    *st.child.lock().unwrap() = Some(child);
    *SERVER_PORT.lock().unwrap() = Some(port);

    // 健康检查：最多 60s（模型首次加载可能较久）
    let health = format!("http://127.0.0.1:{port}/health");
    for _ in 0..200 {
        if let Ok(resp) = ureq::get(&health).timeout(Duration::from_millis(1500)).call() {
            if resp.status() == 200 {
                log::info!("[mt] llama-server 就绪 :{port}");
                return Ok(port);
            }
        }
        // 若进程已死，快速失败
        if let Some(c) = st.child.lock().unwrap().as_mut() {
            if let Ok(Some(_)) = c.try_wait() {
                *st.child.lock().unwrap() = None;
                return Err("llama-server 启动后退出（模型或参数问题）".into());
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Err("llama-server 健康检查超时".into())
}

/// llama-server 查找链：应用内下载的引擎优先，其次 Homebrew（开发环境）
fn llama_server_path() -> Option<PathBuf> {
    let candidates = [
        crate::models::engine_bin(),
        PathBuf::from("/opt/homebrew/bin/llama-server"),
        PathBuf::from("/usr/local/bin/llama-server"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

fn pick_port() -> Option<u16> {
    for p in BASE_PORT..BASE_PORT + 10 {
        if TcpListener::bind(("127.0.0.1", p)).is_ok() {
            return Some(p);
        }
    }
    None
}

// ---------- 翻译请求 ----------
/// 按模型族构建 prompt：Hunyuan-MT 用官方模板（MT 专用模型），通用小模型用编号系统提示
fn build_messages(model_name: &str, lines: &[&str]) -> serde_json::Value {
    let numbered: Vec<String> =
        lines.iter().enumerate().map(|(i, l)| format!("{}. {}", i + 1, l)).collect();
    let user_text = numbered.join("\n");
    if model_name.to_lowercase().contains("hunyuan") {
        // Hunyuan-MT 官方模板（无 system 角色）
        serde_json::json!({
            "messages": [
                { "role": "user", "content": format!("把下面的文本翻译成简体中文，不要额外解释。\n{user_text}") }
            ],
            "temperature": 0.2,
            "max_tokens": 1024,
            "stream": false
        })
    } else {
        serde_json::json!({
            "messages": [
                {
                    "role": "system",
                    "content": "你是专业的字幕翻译引擎。用户会给出若干条带编号的字幕行。把每行字幕翻译成简体中文，输出时保留每行开头的编号数字并替换 N 为实际数字（例：输入「1. hello」输出「1. 你好」）。一行一条，不要解释、不要引号。"
                },
                { "role": "user", "content": user_text }
            ],
            "temperature": 0.2,
            "max_tokens": 1024,
            "stream": false
        })
    }
}

fn translate_batch(port: u16, lines: &[&str]) -> Result<Vec<String>, String> {
    let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
    let model_name = find_model()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_default();
    let body = build_messages(&model_name, lines);
    let resp = ureq::post(&url)
        .timeout(Duration::from_secs(180))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(|e| format!("HTTP 请求失败: {e}"))?;
    let text = resp.into_string().map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let content = v["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("响应缺少 content")?
        .to_string();
    // 编号解析回原顺序；未编号行按顺序填空位（小模型兼容）
    let mut out: Vec<Option<String>> = vec![None; lines.len()];
    let mut unnumbered: Vec<String> = Vec::new();
    for l in content.lines() {
        let l = l.trim();
        if l.is_empty() {
            continue;
        }
        // 识别 "12. " / "12、" / "12)" 前缀
        let mut split = l.splitn(2, |c: char| c == '.' || c == '、' || c == ')');
        let head = split.next().unwrap_or("").trim();
        let rest = split.next().unwrap_or("").trim();
        match head.parse::<usize>() {
            Ok(idx) if idx >= 1 && idx <= lines.len() && !rest.is_empty() => {
                out[idx - 1] = Some(rest.to_string());
            }
            _ => unnumbered.push(l.to_string()),
        }
    }
    for slot in out.iter_mut() {
        if slot.is_none() {
            *slot = unnumbered.pop();
        }
    }
    // 缺失的编号用原文兜底（宁出原文不出错行）
    let outs: Vec<String> = out
        .into_iter()
        .zip(lines.iter())
        .map(|(o, l)| o.unwrap_or_else(|| l.to_string()))
        .collect();
    Ok(outs)
}

/// 源文本已是目标语言（简体中文）时跳过翻译，省 sidecar 算力
pub fn is_mostly_chinese(text: &str) -> bool {
    let cjk = text.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count();
    let total = text.chars().count().max(1);
    cjk * 100 / total > 30
}

// ---------- 缓存持久化 ----------
#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    entries: HashMap<String, String>,
}

fn cache_file() -> PathBuf {
    // 方案：磁盘缓存跨会话有效——放 ~/.livesub-player/，不随系统临时目录清理
    let dir = crate::settings::data_dir();
    if std::fs::create_dir_all(&dir).is_ok() {
        return dir.join("translation-cache.json");
    }
    let mut p = std::env::temp_dir();
    p.push("livesub-translation-cache.json");
    p
}

fn save_cache(map: &HashMap<String, String>, path: &PathBuf) {
    let data = CacheFile {
        version: 1,
        entries: map.clone(),
    };
    if let Ok(s) = serde_json::to_string(&data) {
        let _ = crate::settings::write_atomic(path, s.as_bytes());
    }
}
