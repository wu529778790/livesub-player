//! M2 实时识别管线（技术方案 §4.1，项目灵魂模块）
//!
//! 架构：
//! - 单 worker 线程持有 whisper 上下文（模型只加载一次，0.14s）；全片补全共用同
//!   一份上下文（Arc），不重复加载 ~466MB 模型
//! - ffmpeg 常驻进程管道输出 16k mono f32（方案拍板：常驻管道为主，不逐段 spawn）
//!   `-ss` 置于 `-i` 前 + accurate seek：时间戳以 ffmpeg 输出为基准，不用播放时钟累加
//! - 能量 VAD（M2.0 简化）：整窗静音直接跳过不进 whisper；silero-vad(ort) 留 M2.1 替换
//! - 调度：识别是软实时——领先播放 >30s 就等；seek/换片杀管道重启会话
//! - 幂等：识别结果与「已识别区间」入字幕库（transcript.rs），实时会话跳过已识别
//!   窗——seek 回看、重开视频直接命中；后台补全只跑空洞
//!
//! 全片补全（CompletionState）：后台模式 = 打开视频后从字幕库空洞处补识别，实时
//! 会话优先（让出其识别邻域、CPU 降优先级），目标全片字幕就绪；导出模式 = 同一
//! 条识别通路 + 进度/取消，跑完直接从字幕库构建 SRT（不再单独加载第二份模型）。

use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::transcript::{Seg, TranscriptShared};

const SAMPLE_RATE: usize = 16000;
/// 识别窗口 12s（方案 §4.1：单段上限 ~15s 内）
const WINDOW_SAMPLES: usize = SAMPLE_RATE * 12;
/// 能量 VAD：100ms 帧的 RMS 门限（TTS/人声典型 0.02+，静音 <0.003）
const VAD_RMS_GATE: f32 = 0.008;
/// 静音窗判定：有声帧占比低于 2% 直接跳过 whisper
const VAD_MIN_SPEECH_RATIO: f32 = 0.02;
/// 追帧不追播：识别位置领先播放超过该值就等待
const AHEAD_LIMIT_SEC: f64 = 30.0;
/// 后台补全让出实时识别正在处理的邻域（识别前沿 ± 该值），避免两个 worker 抢同一段
const FRONTIER_CLEARANCE_SEC: f64 = 15.0;
/// 实时会话推理线程数（spike-2 实测 2 线程即够 RTF≤0.15，留余量）
const RT_THREADS: i32 = 4;
/// 后台补全推理线程数（不与实时会话抢满核）
const BG_THREADS: i32 = 2;

// ---------- 全局状态 ----------
pub struct AsrState {
    tx: Mutex<Option<Sender<Msg>>>,
    current_path: Mutex<String>,
    /// 播放位置（毫秒），player 事件线程持续更新
    playback_pos_ms: AtomicU64,
    /// 实时识别推进到的位置（毫秒）：后台补全让出该邻域
    frontier_ms: AtomicU64,
    /// 实时会话是否存活（读到 EOF/出错即结束）：结束后后台补全不再让路
    session_active: AtomicBool,
}

impl Default for AsrState {
    fn default() -> Self {
        Self {
            tx: Mutex::new(None),
            current_path: Mutex::new(String::new()),
            playback_pos_ms: AtomicU64::new(0),
            frontier_ms: AtomicU64::new(0),
            session_active: AtomicBool::new(false),
        }
    }
}

enum Msg {
    Start { path: String, pos: f64 },
    Seek { pos: f64 },
    /// 播放器停止/关片：结束当前识别会话（worker 存活，等下一次 Start）
    StopSession,
    /// 预留：优雅退出 worker（当前进程退出即回收）
    #[allow(dead_code)]
    Shutdown,
}

/// 会话内轮询消息的结果
enum Poll {
    None,
    /// 重启会话（新位置，可选换文件）
    Restart { pos: f64, path: Option<String> },
    /// 结束当前会话但 worker 保留（关片）
    StopSession,
    Stop,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SubtitleEvent {
    start: f64,
    end: f64,
    text: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressEvent {
    start: f64,
    end: f64,
}

// ---------- 对 player 的接口 ----------
pub fn set_playback_pos(app: &AppHandle, t: f64) {
    let st = app.state::<AsrState>();
    st.playback_pos_ms
        .store((t * 1000.0).max(0.0) as u64, Ordering::Relaxed);
}

/// 当前播放位置（秒）——翻译 worker 做就近优先调度用
pub fn playback_pos(app: &AppHandle) -> f64 {
    app.state::<AsrState>()
        .playback_pos_ms
        .load(Ordering::Relaxed) as f64
        / 1000.0
}

fn set_frontier(app: &AppHandle, t: f64) {
    app.state::<AsrState>()
        .frontier_ms
        .store((t * 1000.0).max(0.0) as u64, Ordering::Relaxed);
}

pub fn on_video_start(app: &AppHandle, path: String) {
    {
        let st = app.state::<AsrState>();
        *st.current_path.lock().unwrap() = path.clone();
        st.playback_pos_ms.store(0, Ordering::Relaxed);
        st.frontier_ms.store(0, Ordering::Relaxed);
    }
    // 字幕库随播放会话就位（识别结果入库、已识别区间跳过都靠它）
    crate::transcript::open_current(app, &path);
    send(app, Msg::Start { path, pos: 0.0 });
}

pub fn on_seek(app: &AppHandle, pos: f64) {
    set_frontier(app, pos);
    send(app, Msg::Seek { pos });
}

/// 播放器停止/关片：结束当前识别会话（worker 保留，等下一次 Start）
pub fn on_stop(app: &AppHandle) {
    {
        let st = app.state::<AsrState>();
        *st.current_path.lock().unwrap() = String::new();
        st.playback_pos_ms.store(0, Ordering::Relaxed);
        st.frontier_ms.store(0, Ordering::Relaxed);
    }
    send(app, Msg::StopSession);
}

fn send(app: &AppHandle, msg: Msg) {
    let st = app.state::<AsrState>();
    let mut tx_slot = st.tx.lock().unwrap();
    if tx_slot.is_none() {
        let (tx, rx) = mpsc::channel::<Msg>();
        let app2 = app.clone();
        std::thread::spawn(move || worker(app2, rx));
        *tx_slot = Some(tx);
    }
    let _ = tx_slot.as_ref().unwrap().send(msg);
}

// ---------- 共享 whisper 上下文 ----------
// 实时会话与全片补全共用一份模型（旧实现导出要再加载第二份）。
// 缓存带模型路径：识别档位切换后（cmd_asr_set_model 重置缓存），
// 下一次会话自动加载新档模型，立即生效不重启。
static SHARED_CTX: Mutex<Option<(PathBuf, Arc<WhisperContext>)>> = Mutex::new(None);

fn get_ctx() -> Result<Arc<WhisperContext>, String> {
    let model_path = find_model().ok_or_else(|| {
        "未找到识别模型：请在 设置 → 识别·翻译 中下载语音识别模型".to_string()
    })?;
    {
        let slot = SHARED_CTX.lock().unwrap();
        if let Some((loaded_path, c)) = slot.as_ref() {
            if loaded_path == &model_path {
                return Ok(c.clone());
            }
        }
    }
    let t0 = Instant::now();
    let ctx = WhisperContext::new_with_params(
        &model_path.to_string_lossy(),
        WhisperContextParameters::default(),
    )
    .map_err(|e| format!("whisper 模型加载失败: {e}"))?;
    log::info!(
        "[asr] 模型加载完成 {:.2}s（{}）",
        t0.elapsed().as_secs_f64(),
        model_path.display()
    );
    let arc = Arc::new(ctx);
    *SHARED_CTX.lock().unwrap() = Some((model_path, arc.clone()));
    Ok(arc)
}

// ---------- worker ----------
fn worker(app: AppHandle, rx: Receiver<Msg>) {
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Shutdown => break,
            Msg::StopSession => {
                // 会话运行中会在窗口间隙的 poll 里收到并自行退出；空闲时无事可做
            }
            Msg::Start { path, pos } => match get_ctx() {
                Ok(c) => run_session(&app, &c, &rx, &path, pos),
                Err(e) => handle_model_error(&app, e),
            },
            Msg::Seek { pos } => {
                let path = app.state::<AsrState>().current_path.lock().unwrap().clone();
                if !path.is_empty() {
                    match get_ctx() {
                        Ok(c) => run_session(&app, &c, &rx, &path, pos),
                        Err(e) => handle_model_error(&app, e),
                    }
                }
            }
        }
    }
}

/// 模型不可用：清空 tx 槽位（下载完成后下一次 Start 会重建 worker），并通知前端
fn handle_model_error(app: &AppHandle, e: String) {
    *app.state::<AsrState>().tx.lock().unwrap() = None;
    let _ = app.emit("asr://error", e);
    log::error!("[asr] 模型缺失，识别管线暂停（模型就绪后自动恢复）");
}

/// 会话生命周期包裹：存活标记供后台补全判断「实时是否还在处理」
fn run_session(app: &AppHandle, ctx: &Arc<WhisperContext>, rx: &Receiver<Msg>, path: &str, start_pos: f64) {
    app.state::<AsrState>()
        .session_active
        .store(true, Ordering::Relaxed);
    run_session_inner(app, ctx, rx, path, start_pos);
    app.state::<AsrState>()
        .session_active
        .store(false, Ordering::Relaxed);
}

fn find_model() -> Option<PathBuf> {
    // 识别档位默认快速档（首启后台自动下载的就是它，就绪最快）
    let tier = crate::settings::read("whisper_model").unwrap_or_else(|| "base".into());
    let file = match tier.as_str() {
        "small" => "ggml-small.bin",
        _ => "ggml-base.bin",
    };
    let p = crate::settings::data_dir().join("models").join(file);
    p.is_file().then_some(p)
}

// ---------- 会话：从 pos 起读 ffmpeg 管道，遇 seek/换片重启 ----------
fn run_session_inner(
    app: &AppHandle,
    ctx: &WhisperContext,
    rx: &Receiver<Msg>,
    path: &str,
    start_pos: f64,
) {
    let mut current_path = path.to_string();
    let mut pos = start_pos;
    // 字幕库：识别结果入库；覆盖表内的窗整窗跳过（seek 回看 / 重开视频直接命中）
    let ts = crate::transcript::current(app);

    'session: loop {
        log::info!("[asr] 会话开始：{current_path} 自 {pos:.1}s");
        let mut child = match spawn_ffmpeg(&current_path, pos) {
            Ok(c) => c,
            Err(e) => {
                log::error!("[asr] {e}");
                let _ = app.emit("asr://error", e);
                return;
            }
        };
        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            return;
        };

        let mut window_idx: u64 = 0;
        let mut byte_buf = vec![0u8; WINDOW_SAMPLES * 4];
        let mut samples = vec![0f32; WINDOW_SAMPLES];
        let mut last_text = String::new();
        let mut repeat_count = 0usize;
        // 语言：设置里强制指定则整段钉住（跳过检测）；
        // 自动模式每窗独立检测——跟随内容切换，混语种片源（如片尾英文歌 + 日文对白）
        // 不再被首窗钉死而把日语硬翻成英文（实测 bug）
        let detected_lang: Option<String> =
            crate::settings::read("asr_language").filter(|l| !l.is_empty() && l != "auto");
        if detected_lang.is_some() {
            log::info!("[asr] 使用指定识别语言: {}", detected_lang.clone().unwrap_or_default());
        }
        let mut last_seen_lang: Option<String> = None;

        loop {
            // 窗口间轮询：seek / 换片 / 关停
            match poll_rx(rx) {
                Poll::Stop => {
                    let _ = child.kill();
                    return;
                }
                Poll::StopSession => {
                    let _ = child.kill();
                    log::info!("[asr] 收到关片 → 会话结束");
                    return;
                }
                Poll::Restart { pos: p, path: np } => {
                    let _ = child.kill();
                    log::info!("[asr] 识别中收到 seek/换片 → 重启会话 @{p:.1}s");
                    pos = p;
                    if let Some(np) = np {
                        current_path = np;
                    }
                    continue 'session;
                }
                Poll::None => {}
            }

            // 读一窗（EOF/区间尾返回实际样本数，尾巴不足一窗也识别）
            let n = match read_exact_or_eof(&mut stdout, &mut byte_buf) {
                ReadOutcome::Samples(n) => n,
                ReadOutcome::Eof => {
                    log::info!("[asr] 音频读到结尾，会话结束（共 {window_idx} 窗）");
                    let _ = child.kill();
                    return;
                }
                ReadOutcome::Error => {
                    let _ = child.kill();
                    return;
                }
            };
            for (i, chunk) in byte_buf[..n * 4].chunks_exact(4).enumerate() {
                samples[i] = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            }

            let window_start =
                pos + (window_idx as f64) * (WINDOW_SAMPLES as f64 / SAMPLE_RATE as f64);
            let window_end = window_start + n as f64 / SAMPLE_RATE as f64;

            // 已识别区间：字幕已在库里（打开视频时已载入上屏），不重复识别
            if let Some(t) = &ts {
                let covered = t.t.lock().unwrap().coverage(window_start, window_end) >= 1.0;
                if covered {
                    window_idx += 1;
                    continue;
                }
            }

            // 追帧不追播：领先播放太多就等（等的同时持续轮询 seek）
            loop {
                let play = app.state::<AsrState>().playback_pos_ms.load(Ordering::Relaxed) as f64
                    / 1000.0;
                if window_start <= play + AHEAD_LIMIT_SEC {
                    break;
                }
                match poll_rx(rx) {
                    Poll::Stop => {
                        let _ = child.kill();
                        return;
                    }
                    Poll::StopSession => {
                        let _ = child.kill();
                        log::info!("[asr] 收到关片 → 会话结束");
                        return;
                    }
                    Poll::Restart { pos: p, path: np } => {
                        let _ = child.kill();
                        log::info!("[asr] 等待中收到 seek/换片 → 重启会话 @{p:.1}s");
                        pos = p;
                        if let Some(np) = np {
                            current_path = np;
                        }
                        continue 'session;
                    }
                    Poll::None => {}
                }
                std::thread::sleep(Duration::from_millis(200));
            }

            // 能量 VAD：静音窗直接跳过（省 30–50% 算力的第一道闸）
            let speech_ratio = energy_vad(&samples);
            if speech_ratio < VAD_MIN_SPEECH_RATIO {
                // 静音窗也标「已识别」——否则全片覆盖永远缺这一块，导出会重跑
                mark_processed(&ts, window_start, window_end);
                set_frontier(app, window_end);
                window_idx += 1;
                continue;
            }
            // 进度事件只对「确有人声、正在识别」的窗发——静音/对白间隙不发，
            // 否则前端指示条在没台词的时候也常驻（体验问题：影响看画面）
            let _ = app.emit(
                "asr://progress",
                ProgressEvent { start: window_start, end: window_end },
            );

            // whisper 推理
            let t_infer = Instant::now();
            let mut state = match ctx.create_state() {
                Ok(s) => s,
                Err(e) => {
                    log::error!("[asr] create_state 失败: {e}");
                    let _ = child.kill();
                    return;
                }
            };
            if let Err(e) = state.full(make_params(detected_lang.as_deref(), RT_THREADS), &samples[..n]) {
                log::error!("[asr] 推理失败: {e}");
                window_idx += 1;
                continue;
            }
            // 自动模式：每窗独立检测语言（whisper 窗口内自判），仅在语言切换时记日志；
            // 字幕库 lang 字段记首次检测值，仅作元数据
            if detected_lang.is_none() {
                if let Ok(id) = state.full_lang_id_from_state() {
                    if let Some(code) = whisper_rs::get_lang_str(id) {
                        if last_seen_lang.as_deref() != Some(code) {
                            log::info!("[asr] 检测语言: {code}（窗 {window_idx}）");
                            last_seen_lang = Some(code.to_string());
                            if let Some(t) = &ts {
                                let mut t = t.t.lock().unwrap();
                                if t.lang.is_none() {
                                    t.lang = Some(code.to_string());
                                }
                            }
                        }
                    }
                }
            }
            log::debug!(
                "[asr] 窗 {window_idx} [{window_start:.1}-{window_end:.1}] 推理 {:.2}s（语音占比 {:.0}%)",
                t_infer.elapsed().as_secs_f64(),
                speech_ratio * 100.0
            );

            // 段落事件 + 幻觉治理
            let n_seg = state.full_n_segments().unwrap_or(0);
            let mut raw_segs: Vec<(f64, f64, String)> = Vec::new();
            for i in 0..n_seg {
                let text = state
                    .full_get_segment_text(i)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if text.is_empty()
                    || crate::transcript::is_sound_marker(&text)
                    || crate::transcript::is_hallucination(&text)
                {
                    continue;
                }
                // TODO(M2.1): whisper-rs 0.14 未暴露段落级 no_speech_prob；
                // 静音段已被 VAD 硬跳过，重复幻觉由下方去重兜底
                // 连续重复行去重（spike-2 发现的幻觉形态）
                if text == last_text {
                    repeat_count += 1;
                    if repeat_count >= 3 {
                        continue;
                    }
                } else {
                    last_text = text.clone();
                    repeat_count = 0;
                }
                let seg_start =
                    window_start + state.full_get_segment_t0(i).unwrap_or(0) as f64 / 100.0;
                let seg_end =
                    window_start + state.full_get_segment_t1(i).unwrap_or(0) as f64 / 100.0;
                log::info!("[asr] 字幕 [{seg_start:.1}-{seg_end:.1}] {text}");
                raw_segs.push((seg_start, seg_end, text));
            }
            // 入库去重（后台补全可能已识别过同一段），只把新增段上屏 + 进翻译队列
            let new_segs = append_to_transcript(&ts, window_start, window_end, &raw_segs);
            for s in &new_segs {
                let _ = app.emit(
                    "asr://subtitle",
                    SubtitleEvent { start: s.start, end: s.end, text: s.text.clone() },
                );
                if !crate::mt::is_mostly_chinese(&s.text) {
                    crate::mt::enqueue(app, s.start, s.end, &s.text);
                }
            }
            set_frontier(app, window_end);
            window_idx += 1;
        }
    }
}

/// 识别结果入库（覆盖标记 + 去重合并），返回新增段；无字幕库时原样返回
fn append_to_transcript(
    ts: &Option<Arc<TranscriptShared>>,
    ws: f64,
    we: f64,
    raw: &[(f64, f64, String)],
) -> Vec<Seg> {
    let segs: Vec<Seg> = raw
        .iter()
        .map(|(s, e, x)| Seg { start: *s, end: *e, text: x.clone() })
        .collect();
    let Some(t) = ts else { return segs };
    let mut t = t.t.lock().unwrap();
    let added = t.append(&segs);
    t.mark_window(ws, we);
    t.maybe_flush();
    added
}

/// 标记区间已过识别（VAD 静音窗等没有字幕段的情形）
fn mark_processed(ts: &Option<Arc<TranscriptShared>>, ws: f64, we: f64) {
    if let Some(t) = ts {
        let mut t = t.t.lock().unwrap();
        t.mark_window(ws, we);
        t.maybe_flush();
    }
}

fn make_params<'a>(lang: Option<&'a str>, n_threads: i32) -> FullParams<'a, 'a> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(n_threads);
    match lang {
        Some(l) => {
            params.set_language(Some(l));
            // 简体引导只在确认是中文时加（日/英内容加中文提示词会把输出带歪）
            if l == "zh" {
                params.set_initial_prompt("以下是普通话的句子。");
            }
        }
        None => {
            // language 置空 = whisper.cpp 每窗自动检测语言并按检测语言转写（跟随内容切换）。
            // 千万别设 detect_language(true)：那是「只测语言」模式，whisper.cpp 检测完
            // 直接 return 0，整窗零转写输出（whisper.cpp src/whisper.cpp:5356-5370）
            params.set_language(None);
        }
    }
    params.set_no_speech_thold(0.6);
    params.set_suppress_blank(true);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_print_special(false);
    params
}

/// 排空消息队列，最后一条生效
fn poll_rx(rx: &Receiver<Msg>) -> Poll {
    let mut result = Poll::None;
    loop {
        match rx.try_recv() {
            Ok(Msg::Shutdown) => return Poll::Stop,
            Ok(Msg::StopSession) => {
                result = Poll::StopSession;
            }
            Ok(Msg::Seek { pos }) => {
                result = Poll::Restart { pos, path: None };
            }
            Ok(Msg::Start { path, pos }) => {
                result = Poll::Restart { pos, path: Some(path) };
            }
            Err(TryRecvError::Empty) => return result,
            Err(TryRecvError::Disconnected) => return Poll::Stop,
        }
    }
}

fn spawn_ffmpeg(path: &str, pos: f64) -> Result<std::process::Child, String> {
    std::process::Command::new(ffmpeg_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-ss",
            &format!("{pos:.3}"),
            "-i",
            path,
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-f",
            "f32le",
            "pipe:1",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "ffmpeg 启动失败：本机未安装 ffmpeg。请在终端执行 brew install ffmpeg 后重启应用".into())
}

/// 能量 VAD：返回有声帧占比
fn energy_vad(samples: &[f32]) -> f32 {
    const FRAME: usize = SAMPLE_RATE / 10; // 100ms
    let mut speech_frames = 0usize;
    let mut total = 0usize;
    for frame in samples.chunks(FRAME) {
        let rms = (frame.iter().map(|x| x * x).sum::<f32>() / frame.len() as f32).sqrt();
        if rms > VAD_RMS_GATE {
            speech_frames += 1;
        }
        total += 1;
    }
    if total == 0 { 0.0 } else { speech_frames as f32 / total as f32 }
}

enum ReadOutcome {
    /// 读到 n 个样本（可能不足整窗：文件尾/区间尾）
    Samples(usize),
    Eof,
    Error,
}

/// 读满一窗；不足整窗时返回实际样本数（>0 时仍值得跑一次识别，尾部台词不丢）
fn read_exact_or_eof(r: &mut impl Read, byte_buf: &mut [u8]) -> ReadOutcome {
    let mut filled = 0usize;
    while filled < byte_buf.len() {
        match r.read(&mut byte_buf[filled..]) {
            Ok(0) => {
                return if filled >= 4 {
                    ReadOutcome::Samples(filled / 4)
                } else {
                    ReadOutcome::Eof
                };
            }
            Ok(n) => filled += n,
            Err(_) => return ReadOutcome::Error,
        }
    }
    ReadOutcome::Samples(byte_buf.len() / 4)
}

fn ffmpeg_path() -> &'static str {
    if PathBuf::from("/opt/homebrew/bin/ffmpeg").is_file() {
        "/opt/homebrew/bin/ffmpeg"
    } else if PathBuf::from("/usr/local/bin/ffmpeg").is_file() {
        "/usr/local/bin/ffmpeg"
    } else {
        "ffmpeg"
    }
}

/// ffmpeg 可用性探测（GUI 从 Finder 启动 PATH 极小，直接 spawn 试一把最可靠）
pub fn ffmpeg_ok() -> bool {
    if PathBuf::from("/opt/homebrew/bin/ffmpeg").is_file() || PathBuf::from("/usr/local/bin/ffmpeg").is_file() {
        return true;
    }
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok()
}

/// 切换识别档位并立即生效：写设置 + 丢弃共享上下文 + 当前会话从播放位置用新模型接续
#[tauri::command]
pub fn cmd_asr_set_model(app: AppHandle, tier: String) -> Result<(), String> {
    if !matches!(tier.as_str(), "base" | "small") {
        return Err(format!("未知识别档位: {tier}"));
    }
    crate::settings::write("whisper_model", &tier)?;
    // 所选档位模型必须已下载（前端负责先触发下载，完成后才调用本命令）
    if find_model().is_none() {
        return Err("该档位模型尚未下载完成".into());
    }
    // 丢弃旧上下文：下一次会话（含下方的接续 seek）自动加载新档模型
    *SHARED_CTX.lock().unwrap() = None;
    let st = app.state::<AsrState>();
    if !st.current_path.lock().unwrap().is_empty() {
        // 有视频在播：从当前播放位置用新模型接续识别（已识别区间走字幕库秒过）
        on_seek(&app, playback_pos(&app));
    }
    log::info!("[asr] 识别档位切换为 {tier}，立即生效");
    Ok(())
}

#[tauri::command]
pub fn cmd_check_ffmpeg() -> bool {
    ffmpeg_ok()
}

// ---------- 全片补全：后台填洞 + 导出，共用一条识别通路 ----------
//
// 后台模式：打开视频后按字幕库空洞补识别，直到全片字幕就绪。
//   - 实时会话优先：让出实时识别前沿 ±15s 的窗（正在看的区域实时管），CPU 降优先级
//   - 换片（字幕库被替换）即退出；有空洞时等 30s 再补一轮
// 导出模式：同一条通路跑全片（跳过已识别窗），有进度/可取消，跑完直接从字幕库
// 构建 SRT——不再像旧实现那样单独加载第二份 whisper 模型。

pub struct CompletionState {
    running: AtomicBool,
    cancel: AtomicBool,
    /// true = 导出模式（前端有进度条/取消按钮）；false = 后台补全（静默）
    export: AtomicBool,
}

impl Default for CompletionState {
    fn default() -> Self {
        Self {
            running: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            export: AtomicBool::new(false),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportProgress {
    percent: f64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportDone {
    srt: String,
}

fn srt_from_transcript(ts: &Arc<TranscriptShared>) -> String {
    let t = ts.t.lock().unwrap();
    build_srt(
        &t.segments
            .iter()
            .map(|s| (s.start, s.end, s.text.clone()))
            .collect::<Vec<_>>(),
    )
}

/// 后台补全：视频打开即启动，把字幕库从当前状态补到全片覆盖
pub fn start_background_completion(app: &AppHandle, path: String, duration: f64) {
    if duration <= 0.0 {
        return;
    }
    let st = app.state::<CompletionState>();
    if st.running.load(Ordering::SeqCst) {
        return; // 已有补全在跑（可能是本视频的导出）
    }
    st.export.store(false, Ordering::SeqCst);
    st.cancel.store(false, Ordering::SeqCst);
    st.running.store(true, Ordering::SeqCst);
    let app2 = app.clone();
    let _ = std::thread::Builder::new()
        .name("asr-complete".into())
        .spawn(move || {
            set_bg_priority();
            loop {
                match run_completion(&app2, &path.clone(), duration, false) {
                    Ok(true) => {
                        log::info!("[asr] 后台补全完成：全片字幕已就绪");
                        let _ = app2.emit("asr://complete", &path);
                        break;
                    }
                    Ok(false) => {
                        // 有空洞（多半是实时会话正在处理的邻域）：等一会再补一轮
                        if bg_should_stop(&app2, &path) {
                            break;
                        }
                        for _ in 0..60 {
                            if bg_should_stop(&app2, &path) {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(500));
                        }
                        if bg_should_stop(&app2, &path) {
                            break;
                        }
                    }
                    Err(e) => {
                        // 「已取消」多半是换片/导出抢占，属正常收尾，不当失败刷日志
                        if e != "已取消" {
                            log::error!("[asr] 后台补全失败: {e}");
                        }
                        break;
                    }
                }
            }
            app2.state::<CompletionState>().running.store(false, Ordering::SeqCst);
        });
}

fn bg_should_stop(app: &AppHandle, path: &str) -> bool {
    app.state::<CompletionState>().cancel.load(Ordering::SeqCst)
        || crate::transcript::current(app)
            .map(|ts| ts.t.lock().unwrap().path != path)
            .unwrap_or(true)
}

/// 后台补全降优先级（macOS QoS utility），不与播放/实时识别抢调度
fn set_bg_priority() {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(qos_class: u32, priority: i32) -> i32;
        }
        // QOS_CLASS_UTILITY
        unsafe {
            pthread_set_qos_class_self_np(0x09, 0);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        // 其他平台暂不降级（后续接 nice / Job Object 时统一处理）
    }
}

#[tauri::command]
pub fn cmd_export_full_srt(app: AppHandle, path: String, duration: f64) -> Result<(), String> {
    let st = app.state::<CompletionState>();
    if !PathBuf::from(&path).is_file() {
        return Err(format!("文件不存在: {path}"));
    }
    // 后台补全在跑 → 让路（取消并等它退出）；导出已在跑 → 拒绝
    if st.running.load(Ordering::SeqCst) {
        if st.export.load(Ordering::SeqCst) {
            return Err("已有导出任务进行中".into());
        }
        st.cancel.store(true, Ordering::SeqCst);
        for _ in 0..100 {
            if !st.running.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if st.running.load(Ordering::SeqCst) {
            return Err("后台补全正在收尾，请稍后再试".into());
        }
    }
    st.cancel.store(false, Ordering::SeqCst);
    st.export.store(true, Ordering::SeqCst);
    st.running.store(true, Ordering::SeqCst);

    // 字幕库已覆盖全片 → 不识别直接出 SRT（重看/补全完成后的视频秒出）
    let ts = crate::transcript::open_current(&app, &path);
    if ts.t.lock().unwrap().is_fully_covered(duration) {
        let srt = srt_from_transcript(&ts);
        st.running.store(false, Ordering::SeqCst);
        log::info!("[export] 字幕库已覆盖全片，直接导出（跳过识别）");
        let _ = app.emit("export://done", ExportDone { srt });
        return Ok(());
    }
    log::info!("[export] 全片字幕补全开始: {path}（时长 {duration:.0}s）");
    let app2 = app.clone();
    std::thread::spawn(move || {
        let r = run_completion(&app2, &path, duration, true);
        app2.state::<CompletionState>().running.store(false, Ordering::SeqCst);
        match r {
            Ok(_) => {} // done 事件已在 run_completion 内发出
            Err(e) if e == "已取消" => {
                log::info!("[export] 全片识别已取消");
                let _ = app2.emit("export://error", e);
            }
            Err(e) => {
                log::error!("[export] 全片识别失败: {e}");
                let _ = app2.emit("export://error", e);
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub fn cmd_cancel_export(app: AppHandle) -> Result<(), String> {
    app.state::<CompletionState>().cancel.store(true, Ordering::SeqCst);
    Ok(())
}

/// 跑一轮补全：按字幕库空洞逐段处理（空洞间用 ffmpeg -ss 跳转，不空读已识别区间）。
/// 返回 Ok(true) = 全片已覆盖；Ok(false) = 还有空洞（后台模式稍后重跑）。
/// 导出模式跑完直接发 export://done。
fn run_completion(
    app: &AppHandle,
    path: &str,
    duration: f64,
    export_mode: bool,
) -> Result<bool, String> {
    let ctx = get_ctx()?;
    let ts = crate::transcript::open_current(app, path);
    let ranges: Vec<(f64, f64)> = if export_mode {
        vec![(0.0, duration)]
    } else {
        ts.t.lock().unwrap().gaps(duration)
    };
    if ranges.is_empty() {
        ts.t.lock().unwrap().flush();
        if export_mode {
            let _ = app.emit("export://done", ExportDone { srt: srt_from_transcript(&ts) });
        }
        return Ok(true);
    }
    for (rs, re) in ranges {
        process_range(app, &ctx, &ts, path, rs, re.min(duration), duration, export_mode)?;
    }
    ts.t.lock().unwrap().flush();
    let complete = ts.t.lock().unwrap().gaps(duration).is_empty();
    if export_mode {
        let _ = app.emit("export://done", ExportDone { srt: srt_from_transcript(&ts) });
    }
    Ok(complete)
}

/// 处理一个连续区间 [start, end)：逐窗读音频 → 覆盖跳过 → VAD → whisper → 入库
fn process_range(
    app: &AppHandle,
    ctx: &Arc<WhisperContext>,
    ts: &Arc<TranscriptShared>,
    path: &str,
    start: f64,
    end: f64,
    duration: f64,
    export_mode: bool,
) -> Result<(), String> {
    let threads = if export_mode { RT_THREADS } else { BG_THREADS };
    let mode_label = if export_mode { "导出" } else { "后台" };
    let mut child = spawn_ffmpeg(path, start)?;
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        return Err("ffmpeg 管道建立失败".into());
    };
    let cancelled = || {
        app.state::<CompletionState>().cancel.load(Ordering::SeqCst)
            || ts.cancel.load(Ordering::SeqCst)
    };
    let mut byte_buf = vec![0u8; WINDOW_SAMPLES * 4];
    let mut samples = vec![0f32; WINDOW_SAMPLES];
    let mut idx: u64 = 0;
    // 已读到音频的位置：EOF 时把 [audio_end, end) 标记为已处理——
    // 音频流可能短于容器时长（-shortest 拼接 / 封装 padding），
    // 这段「没有音频」的区间不标记的话，补全永远差一口气
    let mut audio_end = start;
    let mut last_text = String::new();
    let mut repeat_count = 0usize;
    let detected_lang: Option<String> =
        crate::settings::read("asr_language").filter(|l| !l.is_empty() && l != "auto");
    // 自动模式每窗独立检测（与实时会话一致），仅在语言切换时记日志
    let mut last_seen_lang: Option<String> = None;
    let t0 = Instant::now();
    log::info!("[asr] 补全区间 [{start:.1} – {end:.1}]s（{mode_label}）");

    loop {
        if cancelled() {
            let _ = child.kill();
            return Err("已取消".into());
        }
        let n = match read_exact_or_eof(&mut stdout, &mut byte_buf) {
            ReadOutcome::Samples(n) => n,
            ReadOutcome::Eof => {
                let _ = child.kill();
                // 音频流结束：剩余区间按「已识别（无音频）」处理
                if audio_end < end {
                    let mut t = ts.t.lock().unwrap();
                    t.mark_window(audio_end, end);
                    t.maybe_flush();
                }
                break;
            }
            ReadOutcome::Error => {
                let _ = child.kill();
                return Err("ffmpeg 音频读取失败".into());
            }
        };
        for (i, chunk) in byte_buf[..n * 4].chunks_exact(4).enumerate() {
            samples[i] = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        let window_start = start + (idx as f64) * (WINDOW_SAMPLES as f64 / SAMPLE_RATE as f64);
        let window_end = window_start + n as f64 / SAMPLE_RATE as f64;
        audio_end = window_end;
        // 本窗已触及区间尾：处理完这一窗就收工（ffmpeg 管道不用读完整个文件）
        let range_done = window_end >= end;

        // 已识别窗跳过（实时会话可能刚补过这段）
        if ts.t.lock().unwrap().coverage(window_start, window_end) >= 1.0 {
            if range_done {
                let _ = child.kill();
                break;
            }
            idx += 1;
            continue;
        }
        // 后台模式：实时识别前沿邻域让给实时会话（不标记，实时会话会标记）。
        // 实时会话已结束（EOF/出错）就不再让路——否则会话留下的缝隙没人补。
        if !export_mode
            && app
                .state::<AsrState>()
                .session_active
                .load(Ordering::Relaxed)
        {
            let frontier =
                app.state::<AsrState>().frontier_ms.load(Ordering::Relaxed) as f64 / 1000.0;
            if (window_start - frontier).abs() <= FRONTIER_CLEARANCE_SEC {
                if range_done {
                    let _ = child.kill();
                    break;
                }
                idx += 1;
                continue;
            }
        }
        if export_mode && duration > 0.0 {
            let percent = (window_end / duration * 100.0).min(99.0);
            let _ = app.emit("export://progress", ExportProgress { percent });
        }

        let speech_ratio = energy_vad(&samples[..n]);
        if speech_ratio < VAD_MIN_SPEECH_RATIO {
            {
                let mut t = ts.t.lock().unwrap();
                t.mark_window(window_start, window_end);
                t.maybe_flush();
            }
            if range_done {
                let _ = child.kill();
                break;
            }
            idx += 1;
            continue;
        }

        let t_infer = Instant::now();
        let mut state = match ctx.create_state() {
            Ok(s) => s,
            Err(e) => {
                let _ = child.kill();
                return Err(format!("create_state 失败: {e}"));
            }
        };
        if state.full(make_params(detected_lang.as_deref(), threads), &samples[..n]).is_err() {
            if range_done {
                let _ = child.kill();
                break;
            }
            idx += 1;
            continue;
        }
        // 自动模式：每窗独立检测语言（跟随内容切换）
        if detected_lang.is_none() {
            if let Ok(id) = state.full_lang_id_from_state() {
                if let Some(code) = whisper_rs::get_lang_str(id) {
                    if last_seen_lang.as_deref() != Some(code) {
                        log::info!("[asr] 补全检测语言: {code}（窗 {idx}）");
                        last_seen_lang = Some(code.to_string());
                        let mut t = ts.t.lock().unwrap();
                        if t.lang.is_none() {
                            t.lang = Some(code.to_string());
                        }
                    }
                }
            }
        }
        let n_seg = state.full_n_segments().unwrap_or(0);
        let mut raw_segs: Vec<(f64, f64, String)> = Vec::new();
        for i in 0..n_seg {
            let text = state
                .full_get_segment_text(i)
                .unwrap_or_default()
                .trim()
                .to_string();
            if text.is_empty()
                || crate::transcript::is_sound_marker(&text)
                || crate::transcript::is_hallucination(&text)
            {
                continue;
            }
            // 连续重复行去重（与实时管线同一套幻觉治理）
            if text == last_text {
                repeat_count += 1;
                if repeat_count >= 3 {
                    continue;
                }
            } else {
                last_text = text.clone();
                repeat_count = 0;
            }
            let seg_start =
                window_start + state.full_get_segment_t0(i).unwrap_or(0) as f64 / 100.0;
            let seg_end = window_start + state.full_get_segment_t1(i).unwrap_or(0) as f64 / 100.0;
            raw_segs.push((seg_start, seg_end, text));
        }
        // 入库；后台模式把新增段上屏（用户可能正好看到这块），导出模式不上屏
        {
            let mut t = ts.t.lock().unwrap();
            let added = t.append(
                &raw_segs
                    .iter()
                    .map(|(s, e, x)| Seg { start: *s, end: *e, text: x.clone() })
                    .collect::<Vec<_>>(),
            );
            t.mark_window(window_start, window_end);
            t.maybe_flush();
            if !export_mode {
                for s in &added {
                    log::info!("[asr] 补全字幕 [{:.1}-{:.1}] {}", s.start, s.end, s.text);
                    let _ = app.emit(
                        "asr://subtitle",
                        SubtitleEvent { start: s.start, end: s.end, text: s.text.clone() },
                    );
                    if !crate::mt::is_mostly_chinese(&s.text) {
                        crate::mt::enqueue(app, s.start, s.end, &s.text);
                    }
                }
            }
        }
        log::debug!(
            "[asr] 补全窗 {idx} [{window_start:.1}-{window_end:.1}] 推理 {:.2}s（{mode_label}）",
            t_infer.elapsed().as_secs_f64()
        );
        idx += 1;
        if range_done {
            let _ = child.kill();
            break;
        }
    }
    log::info!(
        "[asr] 补全区间 [{start:.1} – {end:.1}]s 完成，耗时 {:.1}s（{mode_label}）",
        t0.elapsed().as_secs_f64()
    );
    Ok(())
}

fn build_srt(segs: &[(f64, f64, String)]) -> String {
    let mut out = String::new();
    for (i, (start, end, text)) in segs.iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            srt_time(*start),
            srt_time(*end),
            text
        ));
    }
    out
}

fn srt_time(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3600000,
        ms % 3600000 / 60000,
        ms % 60000 / 1000,
        ms % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srt_time_formats_timestamps() {
        assert_eq!(srt_time(0.0), "00:00:00,000");
        assert_eq!(srt_time(12.5), "00:00:12,500");
        assert_eq!(srt_time(83.456), "00:01:23,456");
        assert_eq!(srt_time(3661.2), "01:01:01,200");
        assert_eq!(srt_time(-1.0), "00:00:00,000"); // 负时间钳到 0
    }

    #[test]
    fn build_srt_numbers_blocks_and_joins_with_blank_lines() {
        let segs = vec![
            (0.0, 2.0, "こんにちは".to_string()),
            (120.4, 123.2, "All I need".to_string()),
        ];
        assert_eq!(
            build_srt(&segs),
            "1\n00:00:00,000 --> 00:00:02,000\nこんにちは\n\n\
             2\n00:02:00,400 --> 00:02:03,200\nAll I need\n\n"
        );
    }
}
