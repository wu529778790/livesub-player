//! 字幕库：识别结果按视频落盘（~/.livesub-player/transcripts/<key>.json）。
//!
//! key = 路径 + 大小 + mtime 的 FNV 哈希——文件内容变过即视为新视频，不沿用旧字幕。
//!
//! 两类写入方共用当前视频的 Transcript：
//! - 实时会话（跟随播放）：识别出的段入库，seek 回看 / 重开视频直接命中
//! - 全片补全（后台填洞 / 导出）：从头到尾补齐
//!
//! 覆盖表 windows 记录「已过识别」的区间——包括 VAD 判静音的窗（它们合法地没有
//! 字幕），所以覆盖比例只看识别是否跑过，不看有没有字幕段。用途：
//! 实时会话跳过已识别窗、后台补全只跑空洞、导出判断是否可以直接出 SRT。
//!
//! 去重规则：新段与邻域已有段重叠超过较短段的 50% → 丢弃（两个 worker 抢同一段
//! 音频时的兜底，正常路径靠覆盖表就不会重复识别）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

const FLUSH_INTERVAL_MS: u128 = 3000;

#[derive(Clone, Serialize, Deserialize)]
pub struct Seg {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Serialize, Deserialize)]
struct TranscriptFile {
    version: u32,
    path: String,
    lang: Option<String>,
    segments: Vec<Seg>,
    windows: Vec<(f64, f64)>,
}

pub struct Transcript {
    pub path: String,
    pub key: String,
    pub lang: Option<String>,
    /// 字幕段，start 升序
    pub segments: Vec<Seg>,
    /// 已识别区间（合并后、按序）
    windows: Vec<(f64, f64)>,
    dirty: bool,
    last_flush: Instant,
}

fn transcripts_dir() -> PathBuf {
    let dir = crate::settings::data_dir().join("transcripts");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

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

/// 视频身份键：FNV-1a 混入大小与 mtime
pub fn key_for(path: &str) -> String {
    let (size, mtime) = identity(path);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1_0000_0001_b3);
    }
    h ^= size.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    h = h.wrapping_mul(0x1_0000_0001_b3);
    h ^= mtime.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h = h.wrapping_mul(0x1_0000_0001_b3);
    format!("{h:016x}")
}

impl Transcript {
    pub fn load(path: &str) -> Transcript {
        let key = key_for(path);
        let file = transcripts_dir().join(format!("{key}.json"));
        if let Ok(bytes) = std::fs::read(&file) {
            if let Ok(f) = serde_json::from_slice::<TranscriptFile>(&bytes) {
                if f.path == path {
                    log::info!("[transcript] 字幕库命中：{}（{} 段）", path, f.segments.len());
                    return Transcript {
                        path: path.to_string(),
                        key,
                        lang: f.lang,
                        segments: f.segments,
                        windows: f.windows,
                        dirty: false,
                        last_flush: Instant::now(),
                    };
                }
            }
        }
        Transcript {
            path: path.to_string(),
            key,
            lang: None,
            segments: vec![],
            windows: vec![],
            dirty: false,
            last_flush: Instant::now(),
        }
    }

    /// 入库一批段；返回真正新增的（调用方决定是否上屏）
    pub fn append(&mut self, segs: &[Seg]) -> Vec<Seg> {
        let mut added = Vec::new();
        for s in segs {
            let text = s.text.trim();
            if text.is_empty() || s.end <= s.start {
                continue;
            }
            let seg = Seg { start: s.start.max(0.0), end: s.end, text: text.to_string() };
            if self.is_dup(&seg) {
                continue;
            }
            let pos = self
                .segments
                .binary_search_by(|x| x.start.total_cmp(&seg.start))
                .unwrap_or_else(|p| p);
            self.segments.insert(pos, seg.clone());
            added.push(seg);
            self.dirty = true;
        }
        added
    }

    /// 与邻域已有段重叠超过较短段一半视为重复
    fn is_dup(&self, seg: &Seg) -> bool {
        let idx = self
            .segments
            .binary_search_by(|x| x.start.total_cmp(&seg.start))
            .unwrap_or_else(|p| p);
        let from = idx.saturating_sub(8);
        let to = (idx + 8).min(self.segments.len());
        for e in &self.segments[from..to] {
            let ov = seg.end.min(e.end) - seg.start.max(e.start);
            if ov > 0.0 {
                let shorter = (seg.end - seg.start).min(e.end - e.start).max(0.001);
                if ov / shorter > 0.5 {
                    return true;
                }
            }
        }
        false
    }

    /// 标记一个区间已过识别（VAD 静音窗也要标，否则导出永远认为没识别完）
    pub fn mark_window(&mut self, start: f64, end: f64) {
        if end <= start {
            return;
        }
        // 插入并合并相邻/重叠区间
        let mut lo = 0usize;
        let mut hi = 0usize;
        let mut s = start;
        let mut e = end;
        for (i, w) in self.windows.iter().enumerate() {
            if w.1 < s {
                lo = i + 1;
            } else if w.0 > e {
                hi = i;
                break;
            } else {
                s = s.min(w.0);
                e = e.max(w.1);
                hi = i + 1;
            }
        }
        if hi < lo {
            hi = lo; // 新区间落在所有已有区间之后：hi 停在 0 会变成非法 range
        }
        self.windows.splice(lo..hi, [(s, e)]);
        self.dirty = true;
    }

    /// [start, end) 已识别比例（0~1）
    pub fn coverage(&self, start: f64, end: f64) -> f64 {
        let len = end - start;
        if len <= 0.0 {
            return 1.0;
        }
        let mut inter = 0.0;
        for (s, e) in &self.windows {
            let ov = e.min(end) - s.max(start);
            if ov > 0.0 {
                inter += ov;
                if inter >= len {
                    break;
                }
            }
        }
        (inter / len).min(1.0)
    }

    /// 全片是否已识别完（覆盖连续到片尾附近）
    pub fn is_fully_covered(&self, duration: f64) -> bool {
        if duration <= 0.0 {
            return false;
        }
        self.coverage(0.0, duration) >= 0.995
    }

    /// [0, duration) 内尚未识别的区间（后台补全判断是否还需要再跑一轮）。
    /// 尾部 <0.75s 的缝隙不算——AAC 等编码的可解码长度比容器时长略短，
    /// 死磕这几毫秒会让补全永远差一点点、每 30s 重跑一轮。
    pub fn gaps(&self, duration: f64) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        let mut cursor = 0.0;
        for (s, e) in &self.windows {
            if *s > cursor {
                out.push((cursor, s.min(duration)));
            }
            cursor = cursor.max(*e);
            if cursor >= duration {
                return out;
            }
        }
        if cursor < duration {
            out.push((cursor, duration));
        }
        out.retain(|&(s, e)| {
            let len = e - s;
            if len <= 0.05 {
                return false;
            }
            // 尾部小缝隙：已覆盖到实际音频结尾，只是容器时长略长
            !(e >= duration - 0.01 && len < 0.75)
        });
        out
    }

    pub fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let data = TranscriptFile {
            version: 1,
            path: self.path.clone(),
            lang: self.lang.clone(),
            segments: self.segments.clone(),
            windows: self.windows.clone(),
        };
        if let Ok(bytes) = serde_json::to_vec(&data) {
            let path = transcripts_dir().join(format!("{}.json", self.key));
            if crate::settings::write_atomic(&path, &bytes).is_ok() {
                self.dirty = false;
                self.last_flush = Instant::now();
            }
        }
    }

    /// 节流落盘：识别循环里高频调用，实际写盘最多每 3s 一次
    pub fn maybe_flush(&mut self) {
        if self.dirty && self.last_flush.elapsed().as_millis() > FLUSH_INTERVAL_MS {
            self.flush();
        }
    }
}

// ---------- 进程内当前视频的字幕库 ----------

pub struct TranscriptShared {
    pub t: Mutex<Transcript>,
    /// 视频切换后旧库作废：后台补全 worker 轮询此标记退出
    pub cancel: AtomicBool,
}

#[derive(Default)]
pub struct TranscriptState {
    current: Mutex<Option<Arc<TranscriptShared>>>,
    generation: AtomicU64,
}

/// 打开（或取已打开的）当前视频字幕库；换片时旧库标记 cancel
pub fn open_current(app: &AppHandle, path: &str) -> Arc<TranscriptShared> {
    let st = app.state::<TranscriptState>();
    {
        let cur = st.current.lock().unwrap();
        if let Some(ts) = cur.as_ref() {
            if ts.t.lock().unwrap().path == path {
                return ts.clone();
            }
        }
    }
    let shared = Arc::new(TranscriptShared {
        t: Mutex::new(Transcript::load(path)),
        cancel: AtomicBool::new(false),
    });
    let mut cur = st.current.lock().unwrap();
    // 双检：两个 open 竞争同一路径时复用先到者
    if let Some(ts) = cur.as_ref() {
        if ts.t.lock().unwrap().path == path {
            return ts.clone();
        }
    }
    if let Some(old) = cur.take() {
        old.cancel.store(true, Ordering::SeqCst);
    }
    st.generation.fetch_add(1, Ordering::SeqCst);
    *cur = Some(shared.clone());
    shared
}

pub fn current(app: &AppHandle) -> Option<Arc<TranscriptShared>> {
    app.state::<TranscriptState>().current.lock().unwrap().clone()
}

/// 退出前兜底落盘
pub fn flush_current(app: &AppHandle) {
    if let Some(ts) = current(app) {
        ts.t.lock().unwrap().flush();
    }
}

/// 前端打开视频后调用：返回已识别段（直接上屏），并启动后台补全 + 译文补齐
#[tauri::command]
pub fn cmd_transcript_get(app: AppHandle, path: String, duration: f64) -> Result<Vec<Seg>, String> {
    let shared = open_current(&app, &path);
    let segments = {
        let mut t = shared.t.lock().unwrap();
        t.maybe_flush();
        t.segments.clone()
    };
    log::info!(
        "[transcript] 打开 {path}：{} 段字幕（时长 {duration:.0}s）",
        segments.len()
    );
    crate::mt::enqueue_transcript(&app);
    crate::asr::start_background_completion(&app, path, duration);
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: f64, end: f64, text: &str) -> Seg {
        Seg { start, end, text: text.to_string() }
    }

    #[test]
    fn append_dedups_overlapping_segments() {
        let mut t = Transcript::load("/nonexistent/a.mp4");
        let added = t.append(&[seg(0.0, 3.0, "hello"), seg(3.0, 6.0, "world")]);
        assert_eq!(added.len(), 2);
        // 与已有段重叠 >50%：丢弃
        let added = t.append(&[seg(0.5, 3.0, "hello"), seg(6.0, 9.0, "new line")]);
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].text, "new line");
        assert_eq!(t.segments.len(), 3);
        // 保持 start 升序
        assert!(t.segments.windows(2).all(|w| w[0].start <= w[1].start));
    }

    #[test]
    fn mark_window_merges_intervals() {
        let mut t = Transcript::load("/nonexistent/a.mp4");
        t.mark_window(12.0, 24.0);
        t.mark_window(24.0, 36.0);
        t.mark_window(48.0, 60.0);
        assert_eq!(t.windows, vec![(12.0, 36.0), (48.0, 60.0)]);
        // 覆盖比例
        assert_eq!(t.coverage(12.0, 24.0), 1.0);
        assert!((t.coverage(0.0, 36.0) - 24.0 / 36.0).abs() < 1e-9);
        assert_eq!(t.coverage(36.0, 48.0), 0.0);
        // 新区间在所有已有区间之后（触发 lo>hi 边界）
        t.mark_window(70.0, 80.0);
        assert_eq!(t.windows, vec![(12.0, 36.0), (48.0, 60.0), (70.0, 80.0)]);
        // 新区间吞并两侧
        t.mark_window(0.0, 65.0);
        assert_eq!(t.windows, vec![(0.0, 65.0), (70.0, 80.0)]);
    }

    #[test]
    fn gaps_returns_unrecognized_ranges() {
        let mut t = Transcript::load("/nonexistent/a.mp4");
        t.mark_window(12.0, 36.0);
        let gaps = t.gaps(60.0);
        assert_eq!(gaps, vec![(0.0, 12.0), (36.0, 60.0)]);
        t.mark_window(0.0, 12.0);
        t.mark_window(36.0, 60.0);
        assert!(t.gaps(60.0).is_empty());
    }

    #[test]
    fn tail_sliver_is_not_a_gap() {
        let mut t = Transcript::load("/nonexistent/a.mp4");
        // 覆盖到 20.16s，容器时长 20.166s：AAC 可解码长度比容器略短的典型场景
        t.mark_window(0.0, 20.16);
        assert!(t.gaps(20.166666666666664).is_empty(), "尾部几十毫秒不应算空洞");
        // 但片尾真实空洞（>0.75s）必须补
        let mut t2 = Transcript::load("/nonexistent/b.mp4");
        t2.mark_window(0.0, 10.0);
        let gaps = t2.gaps(20.166666666666664);
        assert_eq!(gaps, vec![(10.0, 20.166666666666664)]);
    }

    #[test]
    fn fully_covered_requires_contiguous_tail() {
        let mut t = Transcript::load("/nonexistent/a.mp4");
        t.mark_window(0.0, 100.0);
        // 覆盖了大部分但没到片尾：不算完成
        assert!(!t.is_fully_covered(120.0));
        t.mark_window(100.0, 120.0);
        assert!(t.is_fully_covered(120.0));
    }

    #[test]
    fn key_changes_when_file_changes() {
        let dir = std::env::temp_dir().join(format!("livesub-transcript-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("v.mp4");
        std::fs::write(&p, b"aaa").unwrap();
        let path = p.to_str().unwrap();
        let k1 = key_for(path);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&p, b"bbbbbb").unwrap();
        let k2 = key_for(path);
        assert_ne!(k1, k2, "文件内容变化后 key 必须变，避免沿用旧字幕");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
