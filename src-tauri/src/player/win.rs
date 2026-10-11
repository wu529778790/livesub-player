//! Windows 播放内核：libmpv 动态加载 + 子 HWND 嵌入
//!
//! 与 mac.rs 的差异（架构决策）：
//! - 不链接 libmpv（Windows CI 没有 mpv-dev 导入库）：libloading 运行时加载
//!   `~/.livesub-player/bin/libmpv-2.dll`（应用内下载，见 models.rs "player-mpv"）
//! - 不用 render API 自绘：mpv `--wid` 渲到主窗口的子 HWND，z 序压到最底，
//!   透明的 WebView 盖在上面（tauri.conf transparent: true）——点击全部落在 WebView，
//!   mpv 只负责出画面；这与 macOS「NSView 垫底 + WebView 透出」同构
//! - 子窗口在主线程创建/改位（窗口消息线程亲和，跨线程 SetWindowPos 会死锁），
//!   尺寸变化由 lib.rs 的 WindowEvent::Resized 驱动 on_parent_resized
//!
//! 命令面与 mac.rs 完全同构（前端零感知）：load/pause/seek/stop/volume/speed/
//! get_state/screenshot/toggle_fullscreen + player://state|loaded|error|ended 事件。
//! 外挂字幕走 mpv sub-auto 自动加载（macOS 的嗅探转码是针对 iconv 的，Windows 暂缺）。

use libloading::Library;
use serde::Serialize;
use std::ffi::{c_char, c_void, CString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};

use crate::mt::mpv_dll_path;

// ---------- mpv client API（动态加载子集，签名见 mpv/client.h） ----------
type MpvCreate = unsafe extern "C" fn() -> *mut c_void;
type MpvInitialize = unsafe extern "C" fn(*mut c_void) -> i32;
type MpvTerminateDestroy = unsafe extern "C" fn(*mut c_void);
type MpvSetOptionString = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> i32;
type MpvCommand = unsafe extern "C" fn(*mut c_void, *const *const c_char) -> i32;
type MpvSetPropertyString = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> i32;
type MpvGetPropertyString = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char;
type MpvObserveProperty = unsafe extern "C" fn(*mut c_void, u64, *const c_char, u32) -> i32;
type MpvRequestLogMessages = unsafe extern "C" fn(*mut c_void, *const c_char);
type MpvWaitEvent = unsafe extern "C" fn(*mut c_void, f64) -> *mut c_void;
type MpvWakeup = unsafe extern "C" fn(*mut c_void);
type MpvFree = unsafe extern "C" fn(*mut c_void);

const MPV_FORMAT_STRING: u32 = 1;
const MPV_FORMAT_FLAG: u32 = 3;
const MPV_EVENT_LOG_MESSAGE: i32 = 2;
const MPV_EVENT_END_FILE: i32 = 7;
const MPV_EVENT_PROPERTY_UPDATE: i32 = 22;
const END_FILE_REASON_EOF: i32 = 0;
const END_FILE_REASON_ERROR: i32 = 4;

#[repr(C)]
struct MpvEvent {
    event_id: i32,
    error: i32,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct MpvEventProperty {
    name: *const c_char,
    format: u32,
    data: *mut c_void,
}

#[repr(C)]
struct MpvEventEndFile {
    reason: i32,
    error: i32,
    reason_unused: u64,
}

pub struct MpvLib {
    _lib: Library, // 持有：dll 卸载后函数指针全部失效
    create: MpvCreate,
    initialize: MpvInitialize,
    terminate_destroy: MpvTerminateDestroy,
    set_option_string: MpvSetOptionString,
    command: MpvCommand,
    set_property_string: MpvSetPropertyString,
    get_property_string: MpvGetPropertyString,
    observe_property: MpvObserveProperty,
    request_log_messages: MpvRequestLogMessages,
    wait_event: MpvWaitEvent,
    wakeup: MpvWakeup,
    free: MpvFree,
}

unsafe impl Send for MpvLib {}
unsafe impl Sync for MpvLib {}

impl MpvLib {
    unsafe fn load(dll: &PathBuf) -> Result<Self, String> {
        let lib = Library::new(dll).map_err(|e| {
            format!(
                "加载播放内核失败: {e}（可在设置 → 识别·翻译 检查 libmpv 是否完整）"
            )
        })?;
        macro_rules! sym {
            ($name:literal, $t:ty) => {
                *lib.get::<$t>($name.as_bytes())
                    .map_err(|_| format!("libmpv 缺少符号 {}", $name))?
            };
        }
        let (create, initialize, terminate_destroy, set_option_string, command, set_property_string, get_property_string, observe_property, request_log_messages, wait_event, wakeup, free) = (
            sym!("mpv_create", MpvCreate),
            sym!("mpv_initialize", MpvInitialize),
            sym!("mpv_terminate_destroy", MpvTerminateDestroy),
            sym!("mpv_set_option_string", MpvSetOptionString),
            sym!("mpv_command", MpvCommand),
            sym!("mpv_set_property_string", MpvSetPropertyString),
            sym!("mpv_get_property_string", MpvGetPropertyString),
            sym!("mpv_observe_property", MpvObserveProperty),
            sym!("mpv_request_log_messages", MpvRequestLogMessages),
            sym!("mpv_wait_event", MpvWaitEvent),
            sym!("mpv_wakeup", MpvWakeup),
            sym!("mpv_free", MpvFree),
        );
        Ok(Self {
            _lib: lib,
            create,
            initialize,
            terminate_destroy,
            set_option_string,
            command,
            set_property_string,
            get_property_string,
            observe_property,
            request_log_messages,
            wait_event,
            wakeup,
            free,
        })
    }
}

/// 裸指针线程安全包装（mpv 句柄的命令/属性调用线程安全，事件由专用线程 drain）
struct SendPtr(*mut c_void);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {} // mpv 命令/属性调用线程安全，句柄可跨线程共享

// ---------- 播放会话 ----------

pub struct PlayerState {
    session: Mutex<Option<Arc<Session>>>,
    /// 子 HWND 镜像（isize）：resize 事件免锁读取；0 = 无会话
    child_hwnd: AtomicIsize,
    shutting_down: AtomicBool,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            session: Mutex::new(None),
            child_hwnd: AtomicIsize::new(0),
            shutting_down: AtomicBool::new(false),
        }
    }
}

struct Session {
    lib: Arc<MpvLib>,
    mpv: SendPtr,
    event_shutdown: Arc<AtomicBool>,
}

/// 推给前端的播放状态（与 mac.rs 同构）
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerEvent {
    pub time_pos: Option<f64>,
    pub duration: Option<f64>,
    pub paused: Option<bool>,
}

const UNSUPPORTED: &str = "该平台播放内核尚未接入";

// ---------- 内部工具 ----------

fn child_hwnd_of(app: &tauri::AppHandle) -> Result<isize, String> {
    let v = app.state::<PlayerState>().child_hwnd.load(Ordering::Relaxed);
    if v == 0 {
        Err("播放器未初始化".into())
    } else {
        Ok(v)
    }
}

/// 在主线程创建子窗口 + 初始化 mpv（必须主线程：窗口消息线程亲和）
fn create_on_main(
    app: &tauri::AppHandle,
    lib: Arc<MpvLib>,
) -> Result<(SendPtr, isize), String> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<(SendPtr, isize), String>>();
    let app_h = app.clone();
    app.run_on_main_thread(move || {
        let _ = tx.send(create_session_on_main(&app_h, lib));
    })
    .map_err(|e| format!("主线程调度失败: {e}"))?;
    Ok(rx.recv().map_err(|_| "主线程闭包提前退出".to_string())??)
}

fn create_session_on_main(
    app: &tauri::AppHandle,
    lib: Arc<MpvLib>,
) -> Result<(SendPtr, isize), String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, GetClientRect, SetWindowPos, HWND_BOTTOM, SWP_NOACTIVATE, SWP_NOMOVE,
        WINDOW_EX_STYLE, WS_CHILD, WS_CLIPSIBLINGS, WS_VISIBLE,
    };
    let window = app.get_webview_window("main").ok_or("找不到主窗口")?;
    let main: HWND = window.hwnd().map_err(|e| format!("拿不到主窗口句柄: {e}"))?;

    // 子窗口铺满客户区，z 序压到最底（WebView 之下）
    let mut rect = windows::Win32::Foundation::RECT::default();
    unsafe { GetClientRect(main, &mut rect).map_err(|e| format!("GetClientRect 失败: {e}"))? };
    let child = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            windows::core::w!("STATIC"),
            windows::core::w!("LiveSubMpv"),
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
            0,
            0,
            rect.right.max(1),
            rect.bottom.max(1),
            Some(main),
            None,
            None,
            None,
        )
        .map_err(|e| format!("创建视频子窗口失败: {e}"))?
    };
    unsafe {
        SetWindowPos(
            child,
            Some(HWND_BOTTOM),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOACTIVATE,
        )
        .map_err(|e| format!("子窗口置底失败: {e}"))?;
    }
    let child_isize = child.0 as isize;

    // mpv 实例：wid 指向子窗口，画面渲染进 WebView 底下
    unsafe {
        let mpv = (lib.create)();
        if mpv.is_null() {
            return Err("mpv_create 失败".into());
        }
        let wid = format!("{}", child.0 as isize);
        let opt = |k: &str, v: &str| {
            let (k, v) = (CString::new(k).unwrap(), CString::new(v).unwrap());
            (lib.set_option_string)(mpv, k.as_ptr(), v.as_ptr())
        };
        if opt("wid", &wid) != 0 {
            (lib.terminate_destroy)(mpv);
            return Err("设置 wid 失败".into());
        }
        opt("hwdec", "auto"); // D3D11 硬解
        opt("keep-open", "yes"); // 播完停在末帧，由前端控制换片/回欢迎页
        opt("force-window", "yes");
        opt("osc", "no");
        opt("input-default-bindings", "no"); // 键盘鼠标全部留在 WebView
        opt("input-vo-keyboard", "no");
        opt("sub-auto", "fuzzy"); // 自动加载同名外挂字幕
        if (lib.initialize)(mpv) != 0 {
            (lib.terminate_destroy)(mpv);
            return Err("mpv_initialize 失败".into());
        }
        (lib.request_log_messages)(mpv, c"warn".as_ptr());

        // 属性观察：reply_userdata 1=time-pos 2=duration 3=pause
        let obs = |id: u64, name: &str, fmt: u32| {
            let n = CString::new(name).unwrap();
            (lib.observe_property)(mpv, id, n.as_ptr(), fmt)
        };
        obs(1, "time-pos", MPV_FORMAT_STRING);
        obs(2, "duration", MPV_FORMAT_STRING);
        obs(3, "pause", MPV_FORMAT_FLAG);

        Ok((SendPtr(mpv), child_isize))
    }
}

// ---------- 事件线程 ----------

fn spawn_event_thread(app: &tauri::AppHandle, session: Arc<Session>) {
    let app = app.clone();
    std::thread::Builder::new()
        .name("mpv-events".into())
        .spawn(move || {
            let mpv = session.mpv.0;
            let lib = &session.lib;
            let mut last_duration: Option<f64> = None;
            let mut last_paused: Option<bool> = None;
            let mut last_emit: std::time::Instant = std::time::Instant::now() - std::time::Duration::from_secs(1);
            loop {
                if session.event_shutdown.load(Ordering::Relaxed) {
                    break;
                }
                let ev = unsafe { (lib.wait_event)(mpv, 0.25) };
                if ev.is_null() {
                    continue;
                }
                let e = unsafe { &*(ev as *const MpvEvent) };
                match e.event_id {
                    MPV_EVENT_PROPERTY_UPDATE => {
                        let prop =
                            unsafe { &*(e.data as *const MpvEventProperty) };
                        let name = unsafe { std::ffi::CStr::from_ptr(prop.name) }
                            .to_string_lossy()
                            .into_owned();
                        match (name.as_str(), prop.format) {
                            ("time-pos", MPV_FORMAT_STRING) => {
                                let t = unsafe { std::ffi::CStr::from_ptr(prop.data as *const c_char) }
                                    .to_string_lossy()
                                    .parse::<f64>()
                                    .ok();
                                if let Some(t) = t {
                                    // time-pos 每帧都变，节流到 250ms；暂停/时长变化立即发
                                    if last_emit.elapsed() >= std::time::Duration::from_millis(250) {
                                        last_emit = std::time::Instant::now();
                                        let _ = app.emit(
                                            "player://state",
                                            PlayerEvent {
                                                time_pos: Some(t),
                                                duration: last_duration,
                                                paused: last_paused,
                                            },
                                        );
                                    }
                                }
                            }
                            ("duration", MPV_FORMAT_STRING) => {
                                last_duration = unsafe { std::ffi::CStr::from_ptr(prop.data as *const c_char) }
                                    .to_string_lossy()
                                    .parse::<f64>()
                                    .ok();
                            }
                            ("pause", MPV_FORMAT_FLAG) => {
                                let raw = unsafe { *(prop.data as *const i32) };
                                last_paused = Some(raw != 0);
                                let _ = app.emit(
                                    "player://state",
                                    PlayerEvent {
                                        time_pos: None,
                                        duration: last_duration,
                                        paused: last_paused,
                                    },
                                );
                            }
                            _ => {}
                        }
                    }
                    MPV_EVENT_END_FILE => {
                        let ef = unsafe { &*(e.data as *const MpvEventEndFile) };
                        if ef.reason == END_FILE_REASON_EOF {
                            // 自然播完 → 前端自动连播/回欢迎页（手动 stop 不触发）
                            let _ = app.emit("player://ended", ());
                        } else if ef.reason == END_FILE_REASON_ERROR {
                            let _ = app.emit(
                                "player://error",
                                format!("播放错误（mpv 代码 {}）", ef.error),
                            );
                        }
                    }
                    MPV_EVENT_LOG_MESSAGE => {
                        // 结构 {prefix, level, text}，只转 info 以上由 log crate 收集
                        #[repr(C)]
                        struct MpvEventLog {
                            prefix: *const c_char,
                            level: *const c_char,
                            text: *const c_char,
                        }
                        let m = unsafe { &*(e.data as *const MpvEventLog) };
                        let text = unsafe { std::ffi::CStr::from_ptr(m.text) }.to_string_lossy();
                        log::info!("[mpv:{}] {}", unsafe { std::ffi::CStr::from_ptr(m.level) }.to_string_lossy(), text.trim_end());
                    }
                    _ => {}
                }
            }
        })
        .expect("mpv 事件线程创建失败");
}

// ---------- 会话管理 ----------

/// 取当前会话（没有则初始化：加载 dll + 建子窗口 + mpv 实例 + 事件线程）
fn ensure_session(app: &tauri::AppHandle) -> Result<Arc<Session>, String> {
    let st = app.state::<PlayerState>();
    {
        let g = st.session.lock().unwrap();
        if let Some(s) = g.as_ref() {
            return Ok(s.clone());
        }
    }
    if st.shutting_down.load(Ordering::Relaxed) {
        return Err(UNSUPPORTED.into());
    }
    let dll = mpv_dll_path();
    if !dll.is_file() {
        return Err(
            "播放内核（libmpv-2.dll）未下载，正在自动下载（约 31MB），完成后将自动打开"
                .into(),
        );
    }
    let lib = Arc::new(unsafe { MpvLib::load(&dll) }?);
    let (mpv, child) = create_on_main(app, lib.clone())?;

    let event_shutdown = Arc::new(AtomicBool::new(false));
    let session = Arc::new(Session {
        lib,
        mpv,
        event_shutdown,
    });
    st.child_hwnd.store(child, Ordering::Relaxed);
    *st.session.lock().unwrap() = Some(session.clone());
    spawn_event_thread(app, session.clone());
    Ok(session)
}

// ---------- 命令实现（与 mac.rs 同构） ----------

pub fn player_load(app: &tauri::AppHandle, path: String) -> Result<(), String> {
    let video = PathBuf::from(&path);
    if !video.is_file() {
        return Err(format!("文件不存在: {path}"));
    }
    let session = ensure_session(app)?;
    unsafe {
        let cpath = CString::new(path.as_str()).unwrap();
        let args = [c"loadfile", cpath.as_c_str(), c"replace"];
        let argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).chain([std::ptr::null()]).collect();
        if (session.lib.command)(session.mpv.0, argv.as_ptr()) != 0 {
            return Err("mpv loadfile 失败".into());
        }
    }
    let _ = app.emit(
        "player://loaded",
        PlayerLoaded {
            path: path.clone(),
        },
    );
    Ok(())
}

#[derive(Clone, Serialize)]
pub struct PlayerLoaded {
    pub path: String,
}

pub fn player_toggle_pause(app: &tauri::AppHandle) -> Result<bool, String> {
    let session = ensure_session(app)?;
    unsafe {
        let cur = query_string(&session, "pause").unwrap_or_default();
        let next = if cur == "yes" { "no" } else { "yes" };
        set_prop(&session, "pause", next)?;
        Ok(next == "no") // 返回切换后的暂停态
    }
}

pub fn player_seek(app: &tauri::AppHandle, target: f64, absolute: bool) -> Result<(), String> {
    let session = ensure_session(app)?;
    unsafe {
        let t = CString::new(format!("{target}")).unwrap();
        let mode = if absolute { c"absolute+exact" } else { c"relative" };
        let args = [c"seek", t.as_c_str(), mode];
        let argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).chain([std::ptr::null()]).collect();
        if (session.lib.command)(session.mpv.0, argv.as_ptr()) != 0 {
            return Err("mpv seek 失败".into());
        }
    }
    Ok(())
}

pub fn player_stop(app: &tauri::AppHandle) -> Result<(), String> {
    let session = ensure_session(app)?;
    unsafe {
        let args = [c"stop"];
        let argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).chain([std::ptr::null()]).collect();
        let _ = (session.lib.command)(session.mpv.0, argv.as_ptr());
    }
    Ok(())
}

pub fn player_set_volume(app: &tauri::AppHandle, volume: f64) -> Result<(), String> {
    let session = ensure_session(app)?;
    unsafe { set_prop(&session, "volume", &format!("{volume}")) }
}

pub fn player_set_speed(app: &tauri::AppHandle, speed: f64) -> Result<(), String> {
    let session = ensure_session(app)?;
    unsafe { set_prop(&session, "speed", &format!("{speed}")) }
}

pub fn player_get_state(app: &tauri::AppHandle) -> Result<PlayerEvent, String> {
    let session = ensure_session(app)?;
    unsafe {
        Ok(PlayerEvent {
            time_pos: query_string(&session, "time-pos").and_then(|s| s.parse().ok()),
            duration: query_string(&session, "duration").and_then(|s| s.parse().ok()),
            paused: query_string(&session, "pause").map(|s| s == "yes"),
        })
    }
}

pub fn player_screenshot(app: &tauri::AppHandle) -> Result<String, String> {
    let session = ensure_session(app)?;
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|_| "拿不到用户目录")?;
    let dir = PathBuf::from(home).join("Desktop");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis();
    let path = dir.join(format!("LiveSub-截图-{ts}.png"));
    unsafe {
        let cpath = CString::new(path.to_string_lossy().as_bytes()).unwrap();
        let args = [c"screenshot-to-file", cpath.as_c_str(), c"subtitles"];
        let argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).chain([std::ptr::null()]).collect();
        if (session.lib.command)(session.mpv.0, argv.as_ptr()) != 0 {
            return Err("mpv 截图失败".into());
        }
    }
    Ok(path.to_string_lossy().to_string())
}

pub fn player_toggle_fullscreen(app: &tauri::AppHandle) -> Result<bool, String> {
    let window = app.get_webview_window("main").ok_or("找不到主窗口")?;
    let next = !window.is_fullscreen().unwrap_or(false);
    window.set_fullscreen(next).map_err(|e| e.to_string())?;
    Ok(next)
}

// ---------- 尺寸同步（lib.rs WindowEvent::Resized 驱动；主线程调用） ----------

pub fn on_parent_resized(app: &tauri::AppHandle) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, SetWindowPos, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER};
    let child = app
        .state::<PlayerState>()
        .child_hwnd
        .load(Ordering::Relaxed);
    if child == 0 {
        return;
    }
    let Some(window) = app.get_webview_window("main") else { return };
    let Ok(main) = window.hwnd() else { return };
    let mut rect = windows::Win32::Foundation::RECT::default();
    if unsafe { GetClientRect(main, &mut rect) }.is_err() {
        return;
    }
    unsafe {
        let _ = SetWindowPos(
            HWND(child as _),
            None,
            0,
            0,
            rect.right,
            rect.bottom,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

// ---------- 属性读写小工具 ----------

unsafe fn query_string(session: &Session, name: &str) -> Option<String> {
    let n = CString::new(name).unwrap();
    let v = (session.lib.get_property_string)(session.mpv.0, n.as_ptr());
    if v.is_null() {
        return None;
    }
    let s = std::ffi::CStr::from_ptr(v).to_string_lossy().into_owned();
    (session.lib.free)(v as *mut c_void);
    Some(s)
}

unsafe fn set_prop(session: &Session, name: &str, value: &str) -> Result<(), String> {
    let (n, v) = (CString::new(name).unwrap(), CString::new(value).unwrap());
    if (session.lib.set_property_string)(session.mpv.0, n.as_ptr(), v.as_ptr()) != 0 {
        return Err(format!("mpv 设置 {name} 失败"));
    }
    Ok(())
}

// ---------- tauri 命令（与 mac.rs 同名同参，前端零感知） ----------

#[tauri::command]
pub fn cmd_player_load(app: tauri::AppHandle, path: String) -> Result<(), String> {
    player_load(&app, path)
}

#[tauri::command]
pub fn cmd_player_toggle_pause(app: tauri::AppHandle) -> Result<bool, String> {
    player_toggle_pause(&app)
}

#[tauri::command]
pub fn cmd_player_seek(app: tauri::AppHandle, target: f64, absolute: bool) -> Result<(), String> {
    player_seek(&app, target, absolute)
}

#[tauri::command]
pub fn cmd_player_stop(app: tauri::AppHandle) -> Result<(), String> {
    player_stop(&app)
}

#[tauri::command]
pub fn cmd_player_set_volume(app: tauri::AppHandle, volume: f64) -> Result<(), String> {
    player_set_volume(&app, volume)
}

#[tauri::command]
pub fn cmd_player_set_speed(app: tauri::AppHandle, speed: f64) -> Result<(), String> {
    player_set_speed(&app, speed)
}

#[tauri::command]
pub fn cmd_player_get_state(app: tauri::AppHandle) -> Result<PlayerEvent, String> {
    player_get_state(&app)
}

#[tauri::command]
pub fn cmd_player_screenshot(app: tauri::AppHandle) -> Result<String, String> {
    player_screenshot(&app)
}

#[tauri::command]
pub fn cmd_player_toggle_fullscreen(app: tauri::AppHandle) -> Result<bool, String> {
    player_toggle_fullscreen(&app)
}
