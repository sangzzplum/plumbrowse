//! PlumBrowser — лёгкий кросс-платформенный браузер на Rust.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use serde_json::json;
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
#[cfg(target_os = "windows")]
use std::cell::UnsafeCell;
#[cfg(target_os = "windows")]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use tao::{
    dpi::PhysicalSize,
    event::{ElementState, Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    keyboard::{KeyCode, ModifiersState},
    window::{Icon, Window, WindowBuilder},
};
use wry::{
    dpi::{LogicalPosition, LogicalSize},
    http::{header::CONTENT_TYPE, Request, Response},
    NewWindowResponse, PageLoadEvent, Rect, WebView, WebViewBuilder, WebViewId, RGBA,
};

#[cfg(target_os = "macos")]
use tao::platform::macos::WindowBuilderExtMacOS;

#[cfg(target_os = "windows")]
use wry::{Theme, WebViewBuilderExtWindows, WebViewExtWindows};

fn logical_size(window: &Window) -> (f64, f64) {
    let size = window
        .inner_size()
        .to_logical::<f64>(window.scale_factor());
    (size.width, size.height)
}

fn logical_size_from_physical(size: PhysicalSize<u32>, scale: f64) -> (f64, f64) {
    let size = size.to_logical::<f64>(scale);
    (size.width, size.height)
}

fn toolbar_height() -> f64 {
    if cfg!(target_os = "macos") {
        118.0
    } else if cfg!(target_os = "windows") {
        152.0
    } else {
        152.0
    }
}

#[cfg(target_os = "windows")]
fn debug_log_paths() -> Vec<PathBuf> {
    let mut paths = vec![std::env::temp_dir().join("plumbrowser_debug.log")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            paths.push(dir.join("plumbrowser_debug.log"));
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        paths.push(PathBuf::from(local).join("PlumBrowser").join("debug.log"));
    }
    paths
}

#[cfg(target_os = "windows")]
fn init_windows_debug_log() {
    use std::io::Write;
    let paths = debug_log_paths();
    let stamp = format!(
        "=== PlumBrowser {} start temp={} exe={:?} ===",
        app_version(),
        std::env::temp_dir().display(),
        std::env::current_exe().ok()
    );
    for path in &paths {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
        {
            let _ = writeln!(file, "{stamp}");
            let _ = writeln!(file, "log_path={}", path.display());
            let _ = file.flush();
        }
    }
}

#[cfg(target_os = "windows")]
fn install_windows_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        log_windows_debug(&format!("PANIC: {info}"));
    }));
}

#[cfg(target_os = "windows")]
fn win_startup(step: &str) {
    log_windows_debug(&format!("startup: {step}"));
}

#[cfg(target_os = "windows")]
fn log_windows_debug(msg: &str) {
    use std::io::Write;
    for path in debug_log_paths() {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{msg}");
            let _ = file.flush();
        }
    }
}

#[cfg(target_os = "windows")]
fn log_windows_ipc(msg: &str) {
    log_windows_debug(&format!("ipc: {msg}"));
}

#[cfg(target_os = "windows")]
fn log_windows_layout(toolbar: &WebView, tabs: &[Tab], label: &str) {
    use std::io::Write;
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    fn write_rect(file: &mut std::fs::File, name: &str, hwnd: HWND) {
        let mut rect = RECT::default();
        let ok = unsafe { GetWindowRect(hwnd, &mut rect).is_ok() };
        let _ = writeln!(
            file,
            "{name}: hwnd={:?} rect=({},{})-({},{}) ok={ok}",
            hwnd.0, rect.left, rect.top, rect.right, rect.bottom
        );
    }

    let path = std::env::temp_dir().join("plumbrowser_debug.log");
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };

    let _ = writeln!(file, "\n=== {label} ===");
    for log_path in debug_log_paths() {
        let _ = writeln!(file, "also: {}", log_path.display());
    }
    if let Some(hwnd) = webview_host_hwnd(toolbar) {
        write_rect(&mut file, "toolbar", hwnd);
    } else {
        let _ = writeln!(file, "toolbar: no hwnd");
    }
    for (i, tab) in tabs.iter().enumerate() {
        if let Some(hwnd) = webview_host_hwnd(&tab.webview) {
            write_rect(&mut file, &format!("content[{i}]"), hwnd);
        }
    }
    let _ = writeln!(file, "log: {}", path.display());
}

/// Поднимаем toolbar поверх content-webview (на macOS и Windows child webviews наслаиваются).
#[cfg(target_os = "windows")]
fn webview_host_hwnd(webview: &WebView) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::HWND;
    use wry::WebViewExtWindows;

    let controller = webview.controller();
    let mut host = HWND::default();
    unsafe {
        controller.ParentWindow(&mut host).ok()?;
    }
    if host.0.is_null() {
        None
    } else {
        Some(host)
    }
}

#[cfg(target_os = "windows")]
fn sync_windows_z_order(
    toolbar: &WebView,
    tabs: &[Tab],
    devtools_panel: Option<&WebView>,
) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_BOTTOM, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER,
        SWP_NOSIZE,
    };

    for tab in tabs {
        if let Some(host) = webview_host_hwnd(&tab.webview) {
            unsafe {
                let _ = SetWindowPos(
                    host,
                    Some(HWND_BOTTOM),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                );
            }
        }
    }
    if let Some(panel) = devtools_panel {
        if let Some(host) = webview_host_hwnd(panel) {
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::BringWindowToTop;
                let _ = SetWindowPos(
                    host,
                    Some(HWND_TOP),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                );
                let _ = BringWindowToTop(host);
            }
        }
    }
    if let Some(host) = webview_host_hwnd(toolbar) {
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::BringWindowToTop;
            let _ = SetWindowPos(
                host,
                Some(HWND_TOP),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
            let _ = BringWindowToTop(host);
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_toolbar_data_url(snap: &ToolbarSnapshot) -> String {
    format!(
        "data:text/html;charset=utf-8,{}",
        percent_encode_ipc_path(&windows_toolbar_html(snap))
    )
}

fn raise_toolbar(
    toolbar: &WebView,
    window: &Window,
    tabs: Option<&[Tab]>,
    #[cfg(target_os = "windows")] devtools_panel: Option<&WebView>,
) {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::NSWindowOrderingMode;
        use wry::WebViewExtMacOS;

        let view = toolbar.webview();
        // SAFETY: superview is valid for an attached WKWebView child.
        if let Some(superview) = unsafe { view.superview() } {
            superview.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Above, None);
        }
        let _ = (window, tabs);
    }

    #[cfg(target_os = "windows")]
    {
        let (ww, _) = logical_size(window);
        let _ = toolbar.set_bounds(bounds_toolbar(ww));
        let _ = toolbar.set_visible(true);
        if let Some(tabs) = tabs {
            sync_windows_z_order(toolbar, tabs, devtools_panel);
        }
        let _ = toolbar.focus();
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (toolbar, window, tabs);
    }
}

fn focus_active_tab(tabs: &[Tab], current: usize) {
    #[cfg(not(target_os = "windows"))]
    if let Some(tab) = tabs.get(current) {
        let _ = tab.webview.focus();
    }
    #[cfg(target_os = "windows")]
    let _ = (tabs, current);
}

fn load_window_icon() -> Option<Icon> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/plumnet.png");
    let img = image::open(&path).ok()?;
    let img = img.to_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).ok()
}

/// Dock / панель задач на macOS — иконка окна сама по себе её не меняет.
#[cfg(target_os = "macos")]
fn set_dock_icon() {
    use objc2::AnyThread;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSString;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/plumnet.png");
    let Some(path_str) = path.to_str() else {
        return;
    };

    let ns_path = NSString::from_str(path_str);
    let Some(image) = NSImage::initWithContentsOfFile(NSImage::alloc(), &ns_path) else {
        return;
    };

    let app = NSApplication::sharedApplication(mtm);
    unsafe {
        app.setApplicationIconImage(Some(&image));
    }
}

#[cfg(not(target_os = "macos"))]
fn set_dock_icon() {}

fn webview_go_back(webview: &WebView) {
    #[cfg(target_os = "macos")]
    {
        use wry::WebViewExtMacOS;

        let wk = webview.webview();
        unsafe {
            if wk.canGoBack() {
                let _ = wk.goBack();
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = webview.evaluate_script("history.back()");
    }
}

fn open_docked_devtools(webview: &WebView) {
    #[cfg(not(target_os = "windows"))]
    webview.open_devtools();
    #[cfg(target_os = "windows")]
    let _ = webview;
}

fn close_docked_devtools(webview: &WebView) {
    #[cfg(not(target_os = "windows"))]
    webview.close_devtools();
    #[cfg(target_os = "windows")]
    let _ = webview;
}

fn webview_go_forward(webview: &WebView) {
    #[cfg(target_os = "macos")]
    {
        use wry::WebViewExtMacOS;

        let wk = webview.webview();
        unsafe {
            if wk.canGoForward() {
                let _ = wk.goForward();
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = webview.evaluate_script("history.forward()");
    }
}

const NEWTAB_URL: &str = "plum://newtab";
const OMNIBOX_PLACEHOLDER: &str = "Введите адрес или выполните поиск";
const DEVTOOLS_WIDTH: f64 = 420.0;
const TOOLBAR_BG: RGBA = (32, 33, 36, 255);

fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn version_label() -> String {
    format!("PlumBrowser v{}", app_version())
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn toolbar_navigation_allowed(url: &str) -> bool {
    // WebView2 maps custom schemes to http(s)://plum.* — must allow those through.
    if url.starts_with("http://plum.") || url.starts_with("https://plum.") {
        return true;
    }
    if url.starts_with("plum://") {
        return true;
    }
    if url.starts_with("data:text/html") {
        return true;
    }
    !url.starts_with("http://")
        && !url.starts_with("https://")
        && !url.starts_with("file://")
}

/// Toolbar — это UI, не сайт.
const TOOLBAR_LOCK_SCRIPT: &str = r#"
  document.addEventListener('contextmenu', e => e.preventDefault(), true);
  document.addEventListener('dragstart', e => e.preventDefault(), true);
  document.addEventListener('click', e => {
    const link = e.target.closest('a[href]');
    if (link) { e.preventDefault(); e.stopPropagation(); }
  }, true);
  document.addEventListener('keydown', e => {
    const k = e.key.toLowerCase();
    if (e.key === 'F5' || ((e.metaKey || e.ctrlKey) && k === 'r')) {
      e.preventDefault();
      e.stopPropagation();
    }
    if (e.key === 'F12' || (e.ctrlKey && e.shiftKey && k === 'i') || (e.metaKey && e.altKey && k === 'i')) {
      e.preventDefault();
      e.stopPropagation();
    }
  }, true);
"#;

const CONTENT_SHORTCUT_SCRIPT: &str = r#"
  function postDevtools() {
    try {
      if (window.chrome && window.chrome.webview && window.chrome.webview.postMessage) {
        window.chrome.webview.postMessage('toggle_devtools');
      } else if (window.ipc && window.ipc.postMessage) {
        window.ipc.postMessage('toggle_devtools');
      }
    } catch (e) {}
  }
  document.addEventListener('keydown', e => {
    const k = e.key.toLowerCase();
    if (e.key === 'F12') {
      e.preventDefault();
      e.stopPropagation();
      postDevtools();
      return;
    }
    if (e.ctrlKey && e.shiftKey && k === 'i') {
      e.preventDefault();
      e.stopPropagation();
      postDevtools();
      return;
    }
    if (e.metaKey && e.altKey && k === 'i') {
      e.preventDefault();
      e.stopPropagation();
      postDevtools();
    }
  }, true);
"#;

#[cfg(target_os = "windows")]
const WIN_CONTENT_CONTEXT_SCRIPT: &str = r#"
  document.addEventListener('contextmenu', function(e) {
    var old = document.getElementById('__plum_ctx');
    if (old) old.remove();
    var menu = document.createElement('div');
    menu.id = '__plum_ctx';
    menu.style.cssText = 'position:fixed;left:' + e.clientX + 'px;top:' + e.clientY + 'px;background:#2b2b2b;color:#eee;border:1px solid #444;border-radius:6px;padding:4px 0;z-index:2147483647;font:13px system-ui,sans-serif;min-width:190px;box-shadow:0 8px 24px rgba(0,0,0,.35)';
    function addItem(label, fn) {
      var el = document.createElement('div');
      el.textContent = label;
      el.style.cssText = 'padding:8px 14px;cursor:default';
      el.onmouseenter = function(){ el.style.background = '#3d3d3d'; };
      el.onmouseleave = function(){ el.style.background = 'transparent'; };
      el.onmousedown = function(ev) {
        ev.preventDefault();
        ev.stopPropagation();
        menu.remove();
        fn();
      };
      menu.appendChild(el);
    }
    addItem('Назад', function(){ history.back(); });
    addItem('Обновить', function(){ location.reload(); });
    addItem('Просмотреть код', function(){ postDevtools(); });
    document.documentElement.appendChild(menu);
    e.preventDefault();
    e.stopPropagation();
    setTimeout(function() {
      document.addEventListener('mousedown', function close(ev) {
        if (!menu.contains(ev.target)) menu.remove();
        document.removeEventListener('mousedown', close, true);
      }, true);
    }, 0);
  }, true);
"#;

/// Сайты показывают «браузер устарел», если UA похож на старый WebView — подставляем актуальный Chrome.
fn content_user_agent() -> &'static str {
    if cfg!(target_os = "macos") {
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
    } else if cfg!(target_os = "windows") {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
    } else {
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
    }
}

struct Tab {
    id: u32,
    url: String,
    title: String,
    loading: bool,
    webview: WebView,
}

#[derive(Debug, Clone)]
enum UserEvent {
    Ipc(String),
    /// Windows: wake tao loop after cross-thread toolbar IPC enqueue.
    WakeIpc,
    ToggleDevtools,
    Navigated { tab_id: u32, url: String },
    TitleChanged { tab_id: u32, title: String },
    NewWindow { url: String },
    FocusTab { tab_id: u32 },
    LoadStarted { tab_id: u32 },
    LoadFinished { tab_id: u32 },
    ForceLoad { tab_id: u32, url: String },
}

/// Windows toolbar is re-rendered from Rust (WebView2 often won't run toolbar JS / evaluate_script).
#[cfg(target_os = "windows")]
#[derive(Clone)]
struct ToolbarSnapshot {
    titles: Vec<String>,
    urls: Vec<String>,
    current: usize,
    cur_url: String,
    loading: bool,
}

#[cfg(target_os = "windows")]
impl ToolbarSnapshot {
    fn new_default() -> Self {
        Self {
            titles: vec!["Новая вкладка".to_string()],
            urls: vec![NEWTAB_URL.to_string()],
            current: 0,
            cur_url: String::new(),
            loading: false,
        }
    }
}

#[cfg(target_os = "windows")]
static TOOLBAR_SNAPSHOT: OnceLock<Mutex<ToolbarSnapshot>> = OnceLock::new();

#[cfg(target_os = "windows")]
static EVENT_PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();

#[cfg(target_os = "windows")]
fn toolbar_snapshot() -> &'static Mutex<ToolbarSnapshot> {
    TOOLBAR_SNAPSHOT.get_or_init(|| Mutex::new(ToolbarSnapshot::new_default()))
}

const IPC_ACK_HTML: &str =
    "<!doctype html><html><head><meta charset=\"utf-8\"></head><body></body></html>";

fn percent_encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn percent_encode_ipc_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(target_os = "windows")]
fn ipc_nav_url(msg: &str) -> String {
    format!("plum://ipc/{}", percent_encode_ipc_path(msg))
}

fn percent_decode_component(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Toolbar IPC via navigation (WebView2 custom-scheme pages often lack postMessage).
fn toolbar_ipc_from_url(url: &str) -> Option<String> {
    for prefix in [
        "plum://ipc/",
        "http://plum.ipc/",
        "https://plum.ipc/",
    ] {
        if let Some(rest) = url.strip_prefix(prefix) {
            return Some(percent_decode_component(rest));
        }
    }
    None
}

fn resolve_omnibox_input(input: &str) -> Option<String> {
    let u = input.trim();
    if u.is_empty() {
        return None;
    }

    let lower = u.to_lowercase();
    if lower == "plum.newtab"
        || lower == "plum://newtab"
        || lower.starts_with("http://plum.newtab")
        || lower.starts_with("https://plum.newtab")
    {
        return Some(NEWTAB_URL.to_string());
    }

    if u.starts_with("http://")
        || u.starts_with("https://")
        || u.starts_with("file://")
        || u.starts_with("plum://")
    {
        return Some(u.to_string());
    }

    // If it contains whitespace, it's a search query.
    if u.chars().any(char::is_whitespace) {
        return Some(format!(
            "https://www.google.com/search?q={}",
            percent_encode_query(u)
        ));
    }

    // Heuristic: looks like a host if it contains a dot.
    let looks_like_host = u.contains('.') && !u.starts_with('.') && !u.ends_with('.');
    if looks_like_host {
        // Prefer HTTP first — many sites redirect to HTTPS themselves.
        return Some(format!("http://{u}"));
    }

    // Otherwise search.
    Some(format!(
        "https://www.google.com/search?q={}",
        percent_encode_query(u)
    ))
}

fn is_plum_newtab_http_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.starts_with("http://plum.newtab") || lower.starts_with("https://plum.newtab")
}

fn is_internal_newtab_url(url: &str) -> bool {
    let u = url.trim();
    if u.is_empty() || u == "about:blank" || u.starts_with("data:text/html") {
        return true;
    }
    if u == NEWTAB_URL || u.starts_with("plum://newtab") {
        return true;
    }
    let lower = u.to_lowercase();
    lower == "plum.newtab" || is_plum_newtab_http_url(u)
}

fn is_blocked_plum_http_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    if !(lower.starts_with("http://plum.") || lower.starts_with("https://plum.")) {
        return false;
    }
    !lower.contains("plum.toolbar") && !lower.contains("plum.ipc")
}

/// Canonical URL stored in tab state.
fn logical_tab_url(url: &str) -> String {
    if is_internal_newtab_url(url) {
        NEWTAB_URL.to_string()
    } else {
        url.to_string()
    }
}

/// What we show in the omnibox (new tab page shows empty, like Chrome).
fn omnibox_display_url(url: &str) -> String {
    if is_internal_newtab_url(url) {
        String::new()
    } else {
        url.to_string()
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn browser_icon_data_url() -> String {
    static ICON: OnceLock<String> = OnceLock::new();
    ICON.get_or_init(|| {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/plumnet.png");
        std::fs::read(&path)
            .map(|bytes| format!("data:image/png;base64,{}", base64_encode(&bytes)))
            .unwrap_or_else(|_| String::new())
    })
    .clone()
}

fn tab_favicon_url(page_url: &str) -> String {
    if is_internal_newtab_url(page_url) {
        return browser_icon_data_url();
    }
    let rest = page_url
        .strip_prefix("https://")
        .or_else(|| page_url.strip_prefix("http://"));
    if let Some(rest) = rest {
        if let Some(host) = rest.split('/').next() {
            if !host.is_empty() {
                return format!("http://{host}/favicon.ico");
            }
        }
    }
    browser_icon_data_url()
}

fn webview_stop(webview: &WebView) {
    #[cfg(target_os = "macos")]
    {
        use wry::WebViewExtMacOS;
        unsafe {
            webview.webview().stopLoading();
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = webview.evaluate_script("window.stop()");
    }
}

fn bounds_toolbar(win_w: f64) -> Rect {
    let h = toolbar_height();
    Rect {
        position: LogicalPosition::new(0.0, 0.0).into(),
        size: LogicalSize::new(win_w, h).into(),
    }
}

fn bounds_content(win_w: f64, win_h: f64, devtools_open: bool) -> Rect {
    let h = toolbar_height();
    let devtools_w = if devtools_open && cfg!(target_os = "windows") {
        DEVTOOLS_WIDTH
    } else {
        0.0
    };
    Rect {
        position: LogicalPosition::new(0.0, h).into(),
        size: LogicalSize::new((win_w - devtools_w).max(1.0), (win_h - h).max(1.0)).into(),
    }
}

#[cfg(target_os = "windows")]
fn bounds_devtools_panel(win_w: f64, win_h: f64) -> Rect {
    let h = toolbar_height();
    let content_w = (win_w - DEVTOOLS_WIDTH).max(1.0);
    Rect {
        position: LogicalPosition::new(content_w, h).into(),
        size: LogicalSize::new(DEVTOOLS_WIDTH, (win_h - h).max(1.0)).into(),
    }
}

fn tab_label(tab: &Tab) -> String {
    if !tab.title.is_empty() {
        return tab.title.clone();
    }
    if tab.url.starts_with("plum://") || tab.url.starts_with("data:text/html") {
        return "Новая вкладка".to_string();
    }
    tab.url
        .replace("https://", "")
        .replace("http://", "")
}

/// Keep logical URLs in tab state and the omnibox (never show data: document URLs).
fn sync_toolbar(toolbar: &WebView, tabs: &[Tab], current: usize) {
    let titles: Vec<String> = tabs.iter().map(tab_label).collect();

    #[cfg(target_os = "windows")]
    {
        let loading = tabs.get(current).map(|t| t.loading).unwrap_or(false);
        let cur_url = tabs
            .get(current)
            .map(|t| omnibox_display_url(&t.url))
            .unwrap_or_default();
        let urls: Vec<String> = tabs
            .iter()
            .map(|t| omnibox_display_url(&logical_tab_url(&t.url)))
            .collect();
        {
            let mut snap = toolbar_snapshot()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            snap.titles = titles;
            snap.urls = urls;
            snap.current = current;
            snap.cur_url = cur_url;
            snap.loading = loading;
        }
        TOOLBAR_DIRTY.store(true, Ordering::SeqCst);
        return;
    }

    #[cfg(not(target_os = "windows"))]
    let urls: Vec<String> = tabs
        .iter()
        .map(|t| omnibox_display_url(&logical_tab_url(&t.url)))
        .collect();
    #[cfg(not(target_os = "windows"))]
    let cur_url = tabs
        .get(current)
        .map(|t| omnibox_display_url(&t.url))
        .unwrap_or_default();
    #[cfg(not(target_os = "windows"))]
    let loading = tabs.get(current).map(|t| t.loading).unwrap_or(false);

    #[cfg(not(target_os = "windows"))]
    let script = format!(
        r#"(function(){{
  var titles = {titles};
  var urls = {urls};
  var cur = {current};
  var url = {cur_url};
  var loading = {loading};
  function apply() {{
    if (typeof window.__setState === 'function') {{
      window.__setState(titles, urls, cur, url, loading);
      return true;
    }}
    return false;
  }}
  if (!apply()) {{
    window.__pendingState = [titles, urls, cur, url, loading];
    var tries = 0;
    (function retry() {{
      if (apply() || ++tries > 120) return;
      requestAnimationFrame(retry);
    }})();
  }}
}})();"#,
        titles = json!(titles),
        urls = json!(urls),
        current = current,
        cur_url = json!(cur_url),
        loading = json!(loading)
    );
    #[cfg(not(target_os = "windows"))]
    let _ = toolbar.evaluate_script(&script);
}

fn plum_protocol(_id: WebViewId, req: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let uri = req.uri();
    let host = uri.host().unwrap_or_default();
    let path = uri.path();
    let full = uri.to_string();

    if let Some(msg) = toolbar_ipc_from_url(&full) {
        #[cfg(target_os = "windows")]
        enqueue_ipc(msg);
        return Response::builder()
            .status(200)
            .header(CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Cow::Borrowed(IPC_ACK_HTML.as_bytes()))
            .unwrap();
    }

    // WebView2 can pass custom-scheme URLs with empty host where the authority
    // ends up inside the path (e.g. `plum://toolbar/` -> path `/toolbar/`).
    let is_toolbar = host == "toolbar"
        || full.starts_with("plum://toolbar")
        || full.starts_with("http://plum.toolbar")
        || full.starts_with("https://plum.toolbar")
        || path == "/toolbar"
        || path.starts_with("/toolbar/")
        || (host.is_empty() && path.contains("toolbar"));

    if is_toolbar {
        #[cfg(target_os = "windows")]
        let html = toolbar_snapshot()
            .lock()
            .map(|snap| windows_toolbar_html(&snap))
            .unwrap_or_else(|e| windows_toolbar_html(&e.into_inner()));
        #[cfg(not(target_os = "windows"))]
        let html = toolbar_html();
        return Response::builder()
            .status(200)
            .header(CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Cow::Owned(html.into_bytes()))
            .unwrap();
    }

    let (mime, body): (&str, Cow<'static, [u8]>) = match path {
        "/newtab" | "/newtab/" | "/" if host.is_empty() || host == "newtab" || host == "plum.newtab" => (
            "text/html; charset=utf-8",
            Cow::Borrowed(NEWTAB_HTML.as_bytes()),
        ),
        _ => (
            "text/plain; charset=utf-8",
            Cow::Borrowed(&b"Not found"[..]),
        ),
    };

    Response::builder()
        .status(200)
        .header(CONTENT_TYPE, mime)
        .body(body)
        .unwrap()
}

const NEWTAB_HTML: &str = r#"<!doctype html>
<html lang="ru">
<head>
  <meta charset="utf-8"/>
  <meta name="viewport" content="width=device-width,initial-scale=1"/>
  <title>Новая вкладка</title>
  <style>
    :root { --bg:#0f1115; --fg:#e8eaed; --mut:#9aa0a6; --accent:#8ab4f8; }
    * { box-sizing:border-box; }
    html, body { margin:0; height:100%; }
    body {
      display:flex; flex-direction:column; align-items:center; justify-content:center;
      background:radial-gradient(1200px 600px at 50% 18%, #1b2333, var(--bg));
      color:var(--fg); font:16px/1.4 system-ui, "Segoe UI", Arial, sans-serif;
      padding:24px;
    }
    .brand { font-size:42px; font-weight:700; letter-spacing:-0.02em; margin-bottom:28px; }
    .search-form { width:min(560px, 92vw); }
    .search-box {
      display:flex; align-items:center; gap:10px;
      background:rgba(255,255,255,0.06); border:1px solid rgba(255,255,255,0.1);
      border-radius:28px; padding:6px 8px 6px 18px;
      box-shadow:0 8px 32px rgba(0,0,0,0.25);
    }
    .search-box:focus-within { border-color:rgba(138,180,248,0.55); }
    .search-box input {
      flex:1; border:none; outline:none; background:transparent;
      color:var(--fg); font:inherit; padding:10px 0;
    }
    .search-box input::placeholder { color:var(--mut); }
    .search-box button {
      border:none; border-radius:20px; background:var(--accent); color:#0f1115;
      font:inherit; font-weight:600; padding:10px 18px; cursor:pointer;
    }
    .search-box button:hover { filter:brightness(1.05); }
    .hint { margin-top:18px; color:var(--mut); font-size:14px; text-align:center; }
  </style>
</head>
<body>
  <div class="brand">PlumBrowser</div>
  <form class="search-form" id="search-form">
    <div class="search-box">
      <input id="search" type="search" autocomplete="off" spellcheck="false"
        placeholder="Введите адрес или выполните поиск" autofocus />
      <button type="submit">Перейти</button>
    </div>
  </form>
  <div class="hint">Или используйте строку адреса вверху окна</div>
  <script>
    function post(msg) {
      try {
        if (window.chrome && window.chrome.webview && window.chrome.webview.postMessage) {
          window.chrome.webview.postMessage(msg);
          return;
        }
      } catch (e) {}
      try {
        if (window.ipc && window.ipc.postMessage) {
          window.ipc.postMessage(msg);
        }
      } catch (e2) {}
    }
    function navigate(input) {
      var q = (input || '').trim();
      if (!q) return;
      post('load:' + q);
    }
    document.getElementById('search-form').addEventListener('submit', function(e) {
      e.preventDefault();
      navigate(document.getElementById('search').value);
    });
  </script>
</body>
</html>
"#;

fn newtab_data_url() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| {
        format!(
            "data:text/html;charset=utf-8,{}",
            percent_encode_ipc_path(NEWTAB_HTML)
        )
    })
}

const TOOLBAR_SCRIPT: &str = r#"
    function post(msg) {
      try {
        if (window.ipc && window.ipc.postMessage) {
          window.ipc.postMessage(msg);
          return;
        }
      } catch (e) {}
      try {
        if (window.chrome && window.chrome.webview && window.chrome.webview.postMessage) {
          window.chrome.webview.postMessage(msg);
          return;
        }
      } catch (e2) {}
      try {
        window.location.replace('plum://ipc/' + encodeURIComponent(msg));
      } catch (e3) {}
    }

    function browserIconDataUrl() {
      return window.__plumBrowserIcon || svgFallbackDataUrl();
    }

    function svgFallbackDataUrl() {
      return browserIconDataUrl();
    }

    function faviconUrl(pageUrl) {
      if (!pageUrl || pageUrl.includes('plum.newtab') || pageUrl.startsWith('plum://') || pageUrl.startsWith('data:')) {
        return browserIconDataUrl();
      }
      try {
        const u = new URL(pageUrl);
        return u.origin + '/favicon.ico';
      } catch (e) {
        return browserIconDataUrl();
      }
    }

    document.addEventListener('selectstart', (e) => {
      if (!e.target.closest('input')) e.preventDefault();
    });
    document.addEventListener('mousedown', (e) => {
      if (e.detail > 1 && !e.target.closest('input')) e.preventDefault();
    });

    function bindBtn(id, fn) {
      const el = document.getElementById(id);
      if (el) el.addEventListener('click', fn);
    }

    function initToolbar() {
      const drag = document.getElementById('drag');
      if (drag) drag.addEventListener('pointerdown', () => post('win_drag'));

      bindBtn('min', () => post('win_min'));
      bindBtn('max', () => post('win_max_toggle'));
      bindBtn('close', () => post('win_close'));

      bindBtn('addtab-inline', () => post('new_tab:'));
      bindBtn('addtab-fixed', () => post('new_tab:'));

      const strip = document.getElementById('tab-strip');
      if (strip) {
        strip.addEventListener('wheel', (e) => {
          if (Math.abs(e.deltaY) > Math.abs(e.deltaX)) {
            strip.scrollLeft += e.deltaY;
            e.preventDefault();
          }
        }, { passive: false });
        if (typeof ResizeObserver !== 'undefined') {
          new ResizeObserver(() => layoutTabs()).observe(strip);
        }
      }

      bindBtn('back', () => post('nav_back'));
      bindBtn('forward', () => post('nav_forward'));
      bindBtn('reload', () => {
        const btn = document.getElementById('reload');
        const loading = btn && btn.dataset.loading === '1';
        post(loading ? 'nav_stop' : 'nav_reload');
      });
      bindBtn('devtools', () => post('nav_devtools'));

      const urlInput = document.getElementById('url');
      const go = document.getElementById('go');
      if (go && urlInput) {
        go.addEventListener('click', () => post('load:' + urlInput.value));
        urlInput.addEventListener('keydown', (e) => {
          if (e.key === 'Enter') post('load:' + e.target.value);
        });
        urlInput.addEventListener('blur', () => post('focus_content'));
      }
    }

    function bootToolbar() {
      initToolbar();
      if (window.__pendingState && typeof window.__setState === 'function') {
        const pending = window.__pendingState;
        window.__setState(pending[0], pending[1], pending[2], pending[3], pending[4] || false);
        delete window.__pendingState;
      }
    }

    const TAB_MIN = 32;
    const TAB_MAX = 220;
    const TAB_GAP = 8;

    function layoutTabs() {
      const strip = document.getElementById('tab-strip');
      if (!strip) return;
      requestAnimationFrame(() => {
        const tabs = [...strip.querySelectorAll('.tab')];
        const n = tabs.length;
        if (n === 0) return;

        const avail = strip.clientWidth;
        // WebView2 often reports 0 until the first real layout pass.
        if (avail < 8) {
          requestAnimationFrame(() => layoutTabs());
          return;
        }

        const gap = TAB_GAP;
        strip.classList.remove('scroll');

        const minTotal = n * TAB_MIN + Math.max(0, n - 1) * gap;
        if (minTotal > avail) {
          strip.classList.add('scroll');
          tabs.forEach(t => {
            t.style.flex = '0 0 ' + TAB_MIN + 'px';
            t.style.width = TAB_MIN + 'px';
            t.style.minWidth = TAB_MIN + 'px';
            t.style.maxWidth = TAB_MIN + 'px';
          });
          const active = strip.querySelector('.tab.active');
          if (active) active.scrollIntoView({ inline: 'nearest', block: 'nearest' });
          const addInline = document.getElementById('addtab-inline');
          const addFixed = document.getElementById('addtab-fixed');
          if (addInline) addInline.style.display = 'none';
          if (addFixed) addFixed.style.display = 'grid';
          return;
        }

        let width = Math.floor((avail - Math.max(0, n - 1) * gap) / n);
        width = Math.min(TAB_MAX, Math.max(TAB_MIN, width));
        tabs.forEach(t => {
          t.style.flex = '1 1 0';
          t.style.width = width + 'px';
          t.style.minWidth = TAB_MIN + 'px';
          t.style.maxWidth = TAB_MAX + 'px';
        });

        const addInline = document.getElementById('addtab-inline');
        const addFixed = document.getElementById('addtab-fixed');
        if (addInline) addInline.style.display = 'grid';
        if (addFixed) addFixed.style.display = 'none';
      });
    }

    window.addEventListener('resize', () => layoutTabs());

    window.__setState = function(tabTitles, tabUrls, current, url, loading) {
      const strip = document.getElementById('tab-strip');
      if (!strip) return;
      strip.innerHTML = '';

      tabTitles.forEach((title, i) => {
        const t = document.createElement('div');
        t.className = 'tab' + (i === current ? ' active' : '');
        t.onclick = () => post('switch_tab:' + i);

        const icon = document.createElement('img');
        icon.className = 'tab-icon';
        icon.src = faviconUrl(tabUrls[i] || '');
        icon.referrerPolicy = 'no-referrer';
        icon.loading = 'lazy';
        icon.decoding = 'async';
        icon.onerror = () => { icon.onerror = null; icon.src = browserIconDataUrl(); };

        const tt = document.createElement('div');
        tt.className = 'tab-title';
        tt.textContent = title;

        const x = document.createElement('div');
        x.className = 'tab-close';
        x.textContent = '×';
        x.onclick = (e) => { e.stopPropagation(); post('close_tab:' + i); };

        t.appendChild(icon);
        t.appendChild(tt);
        t.appendChild(x);
        strip.appendChild(t);
      });

      const urlEl = document.getElementById('url');
      if (urlEl) urlEl.value = url || '';
      const reloadBtn = document.getElementById('reload');
      if (reloadBtn) {
        reloadBtn.dataset.loading = loading ? '1' : '0';
        reloadBtn.textContent = loading ? '✕' : '↻';
        reloadBtn.title = loading ? 'Остановить загрузку' : 'Обновить';
      }
      layoutTabs();
    };

    window.__setTabTitle = function(index, title) {
      const tab = document.querySelectorAll('#tab-strip .tab')[index];
      if (tab) {
        const el = tab.querySelector('.tab-title');
        if (el) el.textContent = title;
      }
      layoutTabs();
    };

    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', bootToolbar);
    } else {
      queueMicrotask(bootToolbar);
    }
"#;

const TAB_BAR_CSS: &str = r#"
    body { user-select:none; -webkit-user-select:none; }
    input { user-select:text; -webkit-user-select:text; }
    .tabs-bar { display:flex; align-items:center; gap:8px; height:32px; min-height:32px; width:100%; }
    .tab-strip {
      display:flex; gap:8px; align-items:center;
      flex:1 1 auto; min-width:48px;
      height:32px;
      overflow-x:auto; overflow-y:hidden;
      scrollbar-gutter: stable both-edges;
      scrollbar-width:thin; scrollbar-color:#5f6368 transparent;
    }
    .tab-strip::-webkit-scrollbar { height:6px; }
    .tab-strip::-webkit-scrollbar-thumb { background:#5f6368; border-radius:8px; }
    .tab-strip::-webkit-scrollbar-track { background:transparent; }
    .addtab {
      width:36px; height:32px; border-radius:12px; background:var(--b);
      display:grid; place-items:center; cursor:pointer; user-select:none;
      flex:0 0 auto; font-size:18px; line-height:1;
    }
    .addtab:hover { background:var(--b2); }
    .tab {
      position:relative;
      display:flex; align-items:center; gap:6px;
      height:32px; padding:0 34px 0 8px; border-radius:12px; background:var(--b);
      cursor:pointer; user-select:none; box-sizing:border-box;
    }
    .tab.active { background:var(--b2); }
    .tab-icon { flex:0 0 auto; width:16px; height:16px; opacity:0.95; display:block; border-radius:4px; }
    .tab-title {
      min-width:0; overflow:hidden; white-space:nowrap;
      text-overflow:ellipsis; flex:1 1 auto;
    }
    .tab-close {
      position:absolute; right:4px; top:4px;
      width:24px; height:24px; border-radius:10px;
      display:grid; place-items:center; color:var(--mut); font-weight:900;
      opacity:0; pointer-events:none;
    }
    .tab:hover .tab-close { opacity:1; pointer-events:auto; }
    .tab-close:hover { background:#2a2b2f; color:var(--fg); }
    .tab-strip.scroll .tab-title { display:none; }
    .tab-strip.scroll .tab { padding:0; width:32px; border-radius:10px; display:grid; place-items:center; }
    .tab-strip.scroll .tab-close { position:static; right:auto; top:auto; }
    .tab-strip.scroll .tab:hover .tab-icon { opacity:0; }
    .tab-strip.scroll .tab:hover .tab-close { opacity:1; pointer-events:auto; }
"#;

const WINDOWS_TAB_BAR_CSS: &str = r#"
    .tabs-bar {
      display:grid;
      grid-template-columns:minmax(0,1fr) 36px;
      align-items:center;
      gap:8px;
      height:32px;
      min-height:32px;
      width:100%;
    }
    .tab-strip {
      grid-column:1;
      min-width:0;
      width:100%;
    }
    #addtab-inline { grid-column:2; }
    #addtab-fixed { grid-column:2; }
    .toolbar { position:relative; }
"#;

const WINDOWS_TAB_LAYOUT_SCRIPT: &str = r#"
(function(){
  var TAB_MIN=32,TAB_MAX=220,TAB_GAP=8;
  function layoutTabs(){
    var strip=document.getElementById('tab-strip');
    if(!strip)return;
    var tabs=[].slice.call(strip.querySelectorAll('.tab'));
    var n=tabs.length;
    if(n===0)return;
    var avail=strip.clientWidth;
    if(avail<8){requestAnimationFrame(layoutTabs);return;}
    strip.classList.remove('scroll');
    var minTotal=n*TAB_MIN+Math.max(0,n-1)*TAB_GAP;
    if(minTotal>avail){
      strip.classList.add('scroll');
      tabs.forEach(function(t){
        t.style.flex='0 0 '+TAB_MIN+'px';
        t.style.width=TAB_MIN+'px';
        t.style.minWidth=TAB_MIN+'px';
        t.style.maxWidth=TAB_MIN+'px';
      });
      var active=strip.querySelector('.tab.active');
      if(active)active.scrollIntoView({inline:'nearest',block:'nearest'});
      return;
    }
    var width=Math.floor((avail-Math.max(0,n-1)*TAB_GAP)/n);
    width=Math.min(TAB_MAX,Math.max(TAB_MIN,width));
    tabs.forEach(function(t){
      t.style.flex='1 1 0';
      t.style.width=width+'px';
      t.style.minWidth=TAB_MIN+'px';
      t.style.maxWidth=TAB_MAX+'px';
    });
  }
  layoutTabs();
  window.addEventListener('resize',layoutTabs);
  var strip=document.getElementById('tab-strip');
  if(strip){
    strip.addEventListener('wheel',function(e){
      if(Math.abs(e.deltaY)>Math.abs(e.deltaX)){
        strip.scrollLeft+=e.deltaY;
        e.preventDefault();
      }
    },{passive:false});
  }
})();
"#;

/// Server-rendered toolbar for Windows — tabs and buttons use inline navigation IPC (no JS required).
#[cfg(target_os = "windows")]
fn windows_toolbar_html(snap: &ToolbarSnapshot) -> String {
    let toolbar_h = toolbar_height() as i32;
    let version = version_label();
    let cur_url_attr = html_escape(&snap.cur_url);

    let mut tabs_html = String::new();
    for (i, title) in snap.titles.iter().enumerate() {
        let active = if i == snap.current { " active" } else { "" };
        let title_esc = html_escape(title);
        let page_url = snap.urls.get(i).map(String::as_str).unwrap_or("");
        let icon_url = html_escape(&tab_favicon_url(page_url));
        let switch = ipc_nav_url(&format!("switch_tab:{i}"));
        let close = ipc_nav_url(&format!("close_tab:{i}"));
        tabs_html.push_str(&format!(
            r#"<div class="tab{active}" onclick="window.location.replace('{switch}')">"#
        ));
        tabs_html.push_str(&format!(
            r#"<img class="tab-icon" src="{icon_url}" alt="" referrerpolicy="no-referrer" />"#
        ));
        tabs_html.push_str(&format!(
            r#"<div class="tab-title">{title_esc}</div>"#
        ));
        tabs_html.push_str(&format!(
            r#"<div class="tab-close" onclick="event.stopPropagation();window.location.replace('{close}')">×</div></div>"#
        ));
    }

    let new_tab = ipc_nav_url("new_tab:");
    let win_drag = ipc_nav_url("win_drag");
    let win_min = ipc_nav_url("win_min");
    let win_max = ipc_nav_url("win_max_toggle");
    let win_close = ipc_nav_url("win_close");
    let nav_back = ipc_nav_url("nav_back");
    let nav_forward = ipc_nav_url("nav_forward");
    let (nav_reload_label, nav_reload_title, nav_reload_action) = if snap.loading {
        ("✕", "Остановить загрузку", ipc_nav_url("nav_stop"))
    } else {
        ("↻", "Обновить", ipc_nav_url("nav_reload"))
    };
    let nav_devtools = ipc_nav_url("nav_devtools");

    format!(
        r#"<!doctype html>
<html lang="ru">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width,initial-scale=1" />
  <style>
    :root {{
      --bg:#202124; --fg:#e8eaed; --mut:#9aa0a6; --b:#303134; --b2:#3c4043; --danger:#5b2b2b;
      --toolbarH:{toolbar_h}px; --titlebarH:44px;
    }}
    * {{ box-sizing:border-box; }}
    html, body {{ margin:0; padding:0; width:100%; height:100%; overflow:hidden; background:#202124; }}
    body {{ background:var(--bg); color:var(--fg); font:14px/1.2 system-ui,"Segoe UI",Arial; }}
    .chrome {{ height:var(--toolbarH); display:flex; flex-direction:column; }}
    .titlebar {{
      height:var(--titlebarH); display:flex; align-items:center; gap:10px;
      padding:0 12px; border-bottom:1px solid #2b2c2f;
      user-select:none; -webkit-user-select:none;
    }}
    .drag {{ flex:1; height:100%; display:flex; align-items:center; color:var(--mut); font-weight:700; cursor:default; }}
    .winbtns {{ display:flex; gap:8px; }}
    .wbtn {{
      width:40px; height:26px; border-radius:10px; background:var(--b);
      display:grid; place-items:center; cursor:pointer; user-select:none;
    }}
    .wbtn:hover {{ background:var(--b2); }}
    .wbtn.close {{ background:var(--danger); }}
    .toolbar {{ flex:1; display:flex; flex-direction:column; gap:8px; padding:8px 12px 10px; min-height:0; overflow:visible; }}
    .tabs-bar {{ flex-shrink:0; }}
    {tab_bar_css}
    .row {{ display:flex; gap:8px; align-items:center; }}
    .navbtn {{
      width:36px; height:36px; border-radius:12px; background:var(--b);
      display:grid; place-items:center; cursor:pointer; user-select:none;
      font-size:16px; flex:0 0 auto;
    }}
    .navbtn:hover {{ background:var(--b2); }}
    input {{
      flex:1; min-width:200px; padding:10px 14px; border-radius:16px;
      border:1px solid #3c4043; outline:none; background:#111; color:var(--fg);
    }}
    .go {{ padding:10px 14px; border-radius:16px; background:var(--b); cursor:pointer; user-select:none; flex:0 0 auto; }}
    .go:hover {{ background:var(--b2); }}
  </style>
</head>
<body>
  <div class="chrome">
    <div class="titlebar">
      <div class="drag" onpointerdown="window.location.replace('{win_drag}')">{version}</div>
      <div class="winbtns">
        <div class="wbtn" title="Свернуть" onclick="window.location.replace('{win_min}')">—</div>
        <div class="wbtn" title="Развернуть" onclick="window.location.replace('{win_max}')">□</div>
        <div class="wbtn close" title="Закрыть" onclick="window.location.replace('{win_close}')">×</div>
      </div>
    </div>
    <div class="toolbar">
      <div class="tabs-bar">
        <div class="tab-strip" id="tab-strip">{tabs_html}</div>
        <div class="addtab" title="Новая вкладка" onclick="window.location.replace('{new_tab}')">+</div>
      </div>
      <div class="row">
        <div class="navbtn" title="Назад" onclick="window.location.replace('{nav_back}')">←</div>
        <div class="navbtn" title="Вперёд" onclick="window.location.replace('{nav_forward}')">→</div>
        <div class="navbtn" title="{nav_reload_title}" onclick="window.location.replace('{nav_reload_action}')">{nav_reload_label}</div>
        <div class="navbtn" title="Инструменты разработчика (F12)" onclick="window.location.replace('{nav_devtools}')">&#123; &#125;</div>
        <input id="url" value="{cur_url_attr}" placeholder="{omnibox_placeholder}" autocomplete="off" spellcheck="false"
          onkeydown="if(event.key==='Enter')window.location.replace('plum://ipc/load:'+encodeURIComponent(this.value))" />
        <div class="go" onclick="window.location.replace('plum://ipc/load:'+encodeURIComponent(document.getElementById('url').value))">Перейти</div>
      </div>
    </div>
  </div>
  <script>{tab_layout_script}</script>
</body>
</html>"#,
        tab_bar_css = format!("{TAB_BAR_CSS}\n{WINDOWS_TAB_BAR_CSS}"),
        omnibox_placeholder = OMNIBOX_PLACEHOLDER,
        tab_layout_script = WINDOWS_TAB_LAYOUT_SCRIPT,
    )
}

fn toolbar_html() -> String {
    let toolbar_h = toolbar_height() as i32;

    if cfg!(target_os = "macos") {
        return format!(
            r#"<!doctype html>
<html lang="ru">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width,initial-scale=1" />
  <style>
    :root {{
      --bg:#202124; --fg:#e8eaed; --mut:#9aa0a6; --b:#303134; --b2:#3c4043;
      --toolbarH:{toolbar_h}px;
    }}
    * {{ box-sizing:border-box; }}
    html, body {{ margin:0; padding:0; width:100%; height:100%; overflow:hidden; background:#202124; }}
    body {{ background:var(--bg); color:var(--fg); font:14px/1.2 system-ui,"Segoe UI",Arial; }}
    .chrome {{ height:var(--toolbarH); display:flex; flex-direction:column; }}
    .tabs-wrap {{ padding:28px 12px 0; -webkit-app-region:drag; }}
    {tab_bar_css}
    .tabs-bar {{ -webkit-app-region:no-drag; }}
    .addtab, .tab, .navbtn, input, .go {{ -webkit-app-region:no-drag; }}
    .nav-row {{ display:flex; gap:8px; align-items:center; padding:8px 12px 10px; }}
    .navbtn {{
      width:36px; height:36px; border-radius:12px; background:var(--b);
      display:grid; place-items:center; cursor:pointer; user-select:none;
      font-size:16px; flex:0 0 auto;
    }}
    .navbtn:hover {{ background:var(--b2); }}
    input {{
      flex:1; min-width:200px; padding:10px 14px; border-radius:16px;
      border:1px solid #3c4043; outline:none; background:#111; color:var(--fg);
    }}
    .go {{
      padding:10px 14px; border-radius:16px; background:var(--b);
      cursor:pointer; user-select:none; flex:0 0 auto;
    }}
    .go:hover {{ background:var(--b2); }}
  </style>
</head>
<body>
  <div class="chrome">
    <div class="tabs-wrap">
      <div class="tabs-bar">
        <div class="tab-strip" id="tab-strip"></div>
        <div class="addtab" id="addtab-inline" title="Новая вкладка">+</div>
        <div class="addtab" id="addtab-fixed" title="Новая вкладка" style="display:none">+</div>
      </div>
    </div>
    <div class="nav-row">
      <div class="navbtn" id="back" title="Назад">←</div>
      <div class="navbtn" id="forward" title="Вперёд">→</div>
      <div class="navbtn" id="reload" title="Обновить">↻</div>
      <div class="navbtn" id="devtools" title="Инструменты разработчика (F12)">&#123; &#125;</div>
      <input id="url" placeholder="{omnibox_placeholder}" autocomplete="off" spellcheck="false" />
      <div class="go" id="go">Перейти</div>
    </div>
  </div>
  <script>window.__plumBrowserIcon={browser_icon};</script>
  <script>{script}</script>
</body>
</html>"#,
            script = TOOLBAR_SCRIPT,
            tab_bar_css = TAB_BAR_CSS,
            omnibox_placeholder = OMNIBOX_PLACEHOLDER,
            browser_icon = json!(browser_icon_data_url()),
        );
    }

    format!(
        r#"<!doctype html>
<html lang="ru">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width,initial-scale=1" />
  <style>
    :root {{
      --bg:#202124; --fg:#e8eaed; --mut:#9aa0a6; --b:#303134; --b2:#3c4043; --danger:#5b2b2b;
      --toolbarH:{toolbar_h}px; --titlebarH:44px;
    }}
    * {{ box-sizing:border-box; }}
    html, body {{ margin:0; padding:0; width:100%; height:100%; overflow:hidden; background:#202124; }}
    body {{ background:var(--bg); color:var(--fg); font:14px/1.2 system-ui,"Segoe UI",Arial; }}
    .chrome {{ height:var(--toolbarH); display:flex; flex-direction:column; }}
    .titlebar {{
      height:var(--titlebarH); display:flex; align-items:center; gap:10px;
      padding:0 12px; border-bottom:1px solid #2b2c2f;
      user-select:none; -webkit-user-select:none;
    }}
    .drag {{ flex:1; height:100%; display:flex; align-items:center; color:var(--mut); font-weight:700; cursor:default; }}
    .winbtns {{ display:flex; gap:8px; }}
    .wbtn {{
      width:40px; height:26px; border-radius:10px; background:var(--b);
      display:grid; place-items:center; cursor:pointer; user-select:none;
    }}
    .wbtn:hover {{ background:var(--b2); }}
    .wbtn.close {{ background:var(--danger); }}
    .toolbar {{ flex:1; display:flex; flex-direction:column; gap:8px; padding:8px 12px 10px; min-height:0; overflow:visible; }}
    .tabs-bar {{ flex-shrink:0; }}
    {tab_bar_css}
    .row {{ display:flex; gap:8px; align-items:center; }}
    .navbtn {{
      width:36px; height:36px; border-radius:12px; background:var(--b);
      display:grid; place-items:center; cursor:pointer; user-select:none;
      font-size:16px; flex:0 0 auto;
    }}
    .navbtn:hover {{ background:var(--b2); }}
    input {{
      flex:1; min-width:200px; padding:10px 14px; border-radius:16px;
      border:1px solid #3c4043; outline:none; background:#111; color:var(--fg);
    }}
    .go {{ padding:10px 14px; border-radius:16px; background:var(--b); cursor:pointer; user-select:none; flex:0 0 auto; }}
    .go:hover {{ background:var(--b2); }}
  </style>
</head>
<body>
  <div class="chrome">
    <div class="titlebar">
      <div class="drag" id="drag">{version}</div>
      <div class="winbtns">
        <div class="wbtn" id="min" title="Свернуть">—</div>
        <div class="wbtn" id="max" title="Развернуть">□</div>
        <div class="wbtn close" id="close" title="Закрыть">×</div>
      </div>
    </div>
    <div class="toolbar">
      <div class="tabs-bar">
        <div class="tab-strip" id="tab-strip"></div>
        <div class="addtab" id="addtab-inline" title="Новая вкладка">+</div>
        <div class="addtab" id="addtab-fixed" title="Новая вкладка" style="display:none">+</div>
      </div>
      <div class="row">
        <div class="navbtn" id="back" title="Назад">←</div>
        <div class="navbtn" id="forward" title="Вперёд">→</div>
        <div class="navbtn" id="reload" title="Обновить">↻</div>
        <div class="navbtn" id="devtools" title="Инструменты разработчика (F12)">&#123; &#125;</div>
        <input id="url" placeholder="{omnibox_placeholder}" autocomplete="off" spellcheck="false" />
        <div class="go" id="go">Перейти</div>
      </div>
    </div>
  </div>
  <script>window.__plumBrowserIcon={browser_icon};</script>
  <script>{script}</script>
</body>
</html>"#,
        script = TOOLBAR_SCRIPT,
        tab_bar_css = format!("{TAB_BAR_CSS}\n{WINDOWS_TAB_BAR_CSS}"),
        version = version_label(),
        omnibox_placeholder = OMNIBOX_PLACEHOLDER,
        browser_icon = json!(browser_icon_data_url()),
    )
}

fn build_content_webview(
    window: &Window,
    proxy: EventLoopProxy<UserEvent>,
    tab_id: u32,
    url: &str,
    ww: f64,
    wh: f64,
    visible: bool,
    devtools_open: bool,
) -> WebView {
    #[cfg(target_os = "windows")]
    let content_init = format!("{CONTENT_SHORTCUT_SCRIPT}\n{WIN_CONTENT_CONTEXT_SCRIPT}");
    #[cfg(not(target_os = "windows"))]
    let content_init = CONTENT_SHORTCUT_SCRIPT.to_string();

    let builder = WebViewBuilder::new()
        .with_user_agent(content_user_agent())
        .with_bounds(bounds_content(ww, wh, devtools_open))
        .with_visible(visible)
        .with_focused(if cfg!(target_os = "windows") { false } else { visible })
        .with_clipboard(true)
        .with_back_forward_navigation_gestures(true)
        .with_devtools(true)
        .with_hotkeys_zoom(true)
        .with_initialization_script(&content_init)
        .with_custom_protocol("plum".to_string(), plum_protocol)
        .with_ipc_handler({
            let proxy = proxy.clone();
            move |req: Request<String>| {
                match req.body().as_str() {
                    "toggle_devtools" => {
                        let _ = proxy.send_event(UserEvent::ToggleDevtools);
                    }
                    body if body.starts_with("load:") => {
                        let _ = proxy.send_event(UserEvent::Ipc(body.to_string()));
                    }
                    _ => {}
                }
            }
        })
        .with_on_page_load_handler({
            let proxy = proxy.clone();
            move |event, _| {
                match event {
                    PageLoadEvent::Started => {
                        let _ = proxy.send_event(UserEvent::LoadStarted { tab_id });
                    }
                    PageLoadEvent::Finished => {
                        let _ = proxy.send_event(UserEvent::LoadFinished { tab_id });
                        let _ = proxy.send_event(UserEvent::FocusTab { tab_id });
                    }
                }
            }
        })
        .with_navigation_handler({
            let proxy = proxy.clone();
            move |nav_url| {
                if nav_url == "about:blank" {
                    return true;
                }
                if is_blocked_plum_http_url(&nav_url) {
                    return false;
                }
                if is_plum_newtab_http_url(&nav_url) {
                    let _ = proxy.send_event(UserEvent::ForceLoad {
                        tab_id,
                        url: NEWTAB_URL.to_string(),
                    });
                    return false;
                }
                let _ = proxy.send_event(UserEvent::Navigated {
                    tab_id,
                    url: nav_url.to_string(),
                });
                true
            }
        })
        .with_document_title_changed_handler({
            let proxy = proxy.clone();
            move |title| {
                let _ = proxy.send_event(UserEvent::TitleChanged { tab_id, title });
            }
        })
        .with_new_window_req_handler({
            let proxy = proxy.clone();
            move |nav_url, _features| {
                let _ = proxy.send_event(UserEvent::NewWindow { url: nav_url });
                NewWindowResponse::Deny
            }
        });

    #[cfg(target_os = "windows")]
    let builder = builder
        .with_browser_accelerator_keys(false)
        .with_additional_browser_args("--remote-debugging-port=9222");

    let is_newtab = url == NEWTAB_URL || is_internal_newtab_url(url);
    let webview = if is_newtab {
        builder
            .with_html(NEWTAB_HTML)
            .build_as_child(window)
            .expect("failed to build content webview")
    } else {
        builder
            .with_url(url)
            .build_as_child(window)
            .expect("failed to build content webview")
    };

    webview
}

#[allow(unused_variables)]
fn show_tab(
    window: &Window,
    tabs: &[Tab],
    current: usize,
    toolbar: &WebView,
    devtools_open: bool,
    ww: f64,
    wh: f64,
    #[cfg(target_os = "windows")] devtools_panel: Option<&WebView>,
) {
    for (i, tab) in tabs.iter().enumerate() {
        let visible = i == current;
        let _ = tab.webview.set_visible(visible);
        if !visible {
            close_docked_devtools(&tab.webview);
        }
    }
    if devtools_open {
        if let Some(tab) = tabs.get(current) {
            open_docked_devtools(&tab.webview);
            #[cfg(target_os = "windows")]
            if let Some(panel) = devtools_panel {
                sync_windows_devtools(panel, tab, true);
            } else {
                request_devtools_panel();
            }
        }
    }
    focus_active_tab(tabs, current);
    raise_toolbar(
        toolbar,
        window,
        Some(tabs),
        #[cfg(target_os = "windows")]
        devtools_panel,
    );
}

#[allow(unused_variables)]
fn resize_all(
    window: &Window,
    toolbar: &WebView,
    tabs: &[Tab],
    ww: f64,
    wh: f64,
    devtools_open: bool,
    #[cfg(target_os = "windows")] devtools_panel: Option<&WebView>,
) {
    let _ = toolbar.set_bounds(bounds_toolbar(ww));
    let bounds = bounds_content(ww, wh, devtools_open);
    for tab in tabs {
        let _ = tab.webview.set_bounds(bounds);
    }
    #[cfg(target_os = "windows")]
    if let Some(panel) = devtools_panel {
        let _ = panel.set_bounds(bounds_devtools_panel(ww, wh));
        let _ = panel.set_visible(devtools_open);
    }
    #[cfg(target_os = "windows")]
    sync_windows_z_order(toolbar, tabs, devtools_panel);
}

fn devtools_shortcut(physical_key: KeyCode, modifiers: ModifiersState) -> bool {
    if physical_key == KeyCode::F12 {
        return true;
    }
    let inspect_combo = modifiers.control_key()
        && modifiers.shift_key()
        && matches!(physical_key, KeyCode::KeyI);
    #[cfg(target_os = "macos")]
    let inspect_combo = inspect_combo
        || (modifiers.super_key() && modifiers.alt_key() && matches!(physical_key, KeyCode::KeyI));
    inspect_combo
}

fn toggle_devtools(
    window: &Window,
    toolbar: &WebView,
    tabs: &[Tab],
    current: usize,
    devtools_open: &mut bool,
    ww: f64,
    wh: f64,
    #[cfg(target_os = "windows")] devtools_panel: Option<&WebView>,
) {
    let Some(tab) = tabs.get(current) else {
        return;
    };

    if *devtools_open {
        close_docked_devtools(&tab.webview);
        #[cfg(target_os = "windows")]
        {
            cancel_devtools_panel_request();
            if let Some(panel) = devtools_panel {
                sync_windows_devtools(panel, tab, false);
            }
        }
        *devtools_open = false;
    } else {
        *devtools_open = true;
        open_docked_devtools(&tab.webview);
        #[cfg(target_os = "windows")]
        if let Some(panel) = devtools_panel {
            sync_windows_devtools(panel, tab, true);
        }
        #[cfg(target_os = "windows")]
        if devtools_panel.is_none() {
            request_devtools_panel();
        }
    }

    resize_all(
        window,
        toolbar,
        tabs,
        ww,
        wh,
        *devtools_open,
        #[cfg(target_os = "windows")]
        devtools_panel,
    );
    raise_toolbar(
        toolbar,
        window,
        Some(tabs),
        #[cfg(target_os = "windows")]
        devtools_panel,
    );
    focus_active_tab(tabs, current);
}

fn find_tab_idx(tabs: &[Tab], tab_id: u32) -> Option<usize> {
    tabs.iter().position(|t| t.id == tab_id)
}

fn build_toolbar(window: &Window, proxy: EventLoopProxy<UserEvent>, ww: f64) -> WebView {
    #[cfg(not(target_os = "windows"))]
    let proxy_page = proxy.clone();
    let proxy_ipc = proxy.clone();
    #[cfg(not(target_os = "windows"))]
    let proxy_nav = proxy.clone();
    let builder = WebViewBuilder::new()
        .with_bounds(bounds_toolbar(ww))
        .with_background_color(TOOLBAR_BG)
        .with_visible(true)
        .with_focused(true)
        .with_accept_first_mouse(true)
        .with_devtools(false)
        .with_hotkeys_zoom(false)
        .with_back_forward_navigation_gestures(false)
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_initialization_script(TOOLBAR_LOCK_SCRIPT)
        .with_navigation_handler(move |url| {
            if let Some(msg) = toolbar_ipc_from_url(&url) {
                #[cfg(target_os = "windows")]
                log_windows_debug(&format!("toolbar nav ipc: {msg}"));
                #[cfg(target_os = "windows")]
                enqueue_ipc(msg);
                #[cfg(not(target_os = "windows"))]
                let _ = proxy_nav.send_event(UserEvent::Ipc(msg));
                return false;
            }
            toolbar_navigation_allowed(&url)
        })
        .with_on_page_load_handler(move |event, _| {
            if matches!(event, PageLoadEvent::Finished) {
                #[cfg(not(target_os = "windows"))]
                let _ = proxy_page.send_event(UserEvent::Ipc("ready".to_string()));
            }
        })
        .with_ipc_handler(move |req: Request<String>| {
            let _ = proxy_ipc.send_event(UserEvent::Ipc(req.body().clone()));
        });

    #[cfg(target_os = "windows")]
    let builder = {
        let initial = ToolbarSnapshot::new_default();
        builder
            .with_url(&windows_toolbar_data_url(&initial))
            .with_custom_protocol("plum".to_string(), plum_protocol)
    };

    #[cfg(not(target_os = "windows"))]
    let builder = builder.with_html(&toolbar_html());

    #[cfg(target_os = "windows")]
    let builder = builder
        .with_default_context_menus(false)
        .with_browser_accelerator_keys(false);

    let toolbar = builder
        .build_as_child(window)
        .expect("failed to build toolbar webview");

    #[cfg(target_os = "windows")]
    {
        let _ = toolbar.set_theme(Theme::Dark);
    }

    toolbar
}

#[cfg(target_os = "windows")]
const DEVTOOLS_PANEL_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"/><style>
  html,body{margin:0;height:100%;background:#1e1e1e;overflow:hidden}
</style></head><body></body></html>"#;

#[cfg(target_os = "windows")]
fn build_devtools_panel(window: &Window, ww: f64, wh: f64) -> WebView {
    WebViewBuilder::new()
        .with_html(DEVTOOLS_PANEL_HTML)
        .with_bounds(bounds_devtools_panel(ww, wh))
        .with_background_color((30, 30, 30, 255))
        .with_visible(false)
        .with_devtools(false)
        .with_hotkeys_zoom(false)
        .with_navigation_handler(|url| {
            url.starts_with("http://127.0.0.1:9222") || url.starts_with("about:")
        })
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_default_context_menus(false)
        .with_browser_accelerator_keys(false)
        .build_as_child(window)
        .expect("failed to build devtools panel")
}

fn open_new_tab(
    window: &Window,
    proxy: &EventLoopProxy<UserEvent>,
    tabs: &mut Vec<Tab>,
    current: &mut usize,
    next_id: &mut u32,
    toolbar: &WebView,
    url: &str,
    ww: f64,
    wh: f64,
    devtools_open: bool,
    #[cfg(target_os = "windows")] devtools_panel: Option<&WebView>,
) {
    for tab in tabs.iter() {
        let _ = tab.webview.set_visible(false);
    }

    let tab_id = *next_id;
    *next_id += 1;

    let title = if url.starts_with("plum://") {
        "Новая вкладка".to_string()
    } else {
        String::new()
    };

    let webview = build_content_webview(
        window,
        proxy.clone(),
        tab_id,
        url,
        ww,
        wh,
        true,
        devtools_open,
    );
    tabs.push(Tab {
        id: tab_id,
        url: logical_tab_url(url),
        title,
        loading: false,
        webview,
    });
    *current = tabs.len() - 1;
    if devtools_open {
        open_docked_devtools(&tabs[*current].webview);
        #[cfg(target_os = "windows")]
        if let Some(panel) = devtools_panel {
            sync_windows_devtools(panel, &tabs[*current], true);
        } else {
            request_devtools_panel();
        }
    }
    sync_toolbar(toolbar, tabs, *current);
    resize_all(
        window,
        toolbar,
        tabs,
        ww,
        wh,
        devtools_open,
        #[cfg(target_os = "windows")]
        devtools_panel,
    );
    focus_active_tab(tabs, *current);
    raise_toolbar(
        toolbar,
        window,
        Some(tabs),
        #[cfg(target_os = "windows")]
        devtools_panel,
    );
}

#[cfg(target_os = "windows")]
struct WindowsRunState {
    window: Window,
    toolbar: WebView,
    devtools_panel: Option<WebView>,
    tabs: Vec<Tab>,
    current: usize,
    next_id: u32,
    ww: f64,
    wh: f64,
    devtools_open: bool,
    modifiers: ModifiersState,
    z_order_nudges: u32,
    proxy: EventLoopProxy<UserEvent>,
    /// 0 = pending, 1 = window shown, 2 = bootstrap complete
    bootstrap_phase: u8,
    startup_focus_frames: u32,
}

#[cfg(target_os = "windows")]
struct DevtoolsConnectState {
    page_url: String,
    attempts: u32,
}

#[cfg(target_os = "windows")]
const DEVTOOLS_CONNECT_MAX: u32 = 150;

#[cfg(target_os = "windows")]
fn devtools_connect_slot() -> &'static Mutex<Option<DevtoolsConnectState>> {
    static SLOT: OnceLock<Mutex<Option<DevtoolsConnectState>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

#[cfg(target_os = "windows")]
fn sync_windows_devtools(panel: &WebView, tab: &Tab, open: bool) {
    if open {
        let page_url = logical_tab_url(&tab.url);
        *devtools_connect_slot()
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(DevtoolsConnectState {
            page_url,
            attempts: 0,
        });
        let _ = panel.set_visible(true);
        log_windows_debug("devtools: connecting docked panel");
    } else {
        *devtools_connect_slot()
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        win_devtools::close_panel(panel);
        log_windows_debug("devtools: docked panel closed");
    }
}

#[cfg(target_os = "windows")]
fn tick_devtools_connect(app: &mut WindowsRunState) {
    let mut slot = devtools_connect_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let Some(state) = slot.as_mut() else {
        return;
    };
    if !app.devtools_open {
        *slot = None;
        return;
    }
    let Some(panel) = app.devtools_panel.as_ref() else {
        *slot = None;
        return;
    };

    if let Some(url) = win_devtools::inspector_url_for_page(&state.page_url) {
        log_windows_debug("devtools: inspector ready");
        let _ = panel.load_url(&url);
        let _ = panel.set_visible(true);
        raise_toolbar(
            &app.toolbar,
            &app.window,
            Some(&app.tabs),
            app.devtools_panel.as_ref(),
        );
        *slot = None;
        return;
    }

    state.attempts += 1;
    if state.attempts >= DEVTOOLS_CONNECT_MAX {
        log_windows_debug("devtools: CDP connect timed out, opening DevTools window");
        if let Some(tab) = app.tabs.get(app.current) {
            tab.webview.open_devtools();
        }
        if let Some(panel) = app.devtools_panel.as_ref() {
            win_devtools::close_panel(panel);
        }
        app.devtools_open = false;
        resize_all(
            &app.window,
            &app.toolbar,
            &app.tabs,
            app.ww,
            app.wh,
            false,
            app.devtools_panel.as_ref(),
        );
        *slot = None;
    }
}

#[cfg(target_os = "windows")]
fn tick_devtools_panel_build(app: &mut WindowsRunState) {
    if DEVTOOLS_PANEL_PENDING.swap(false, Ordering::SeqCst) {
        DEVTOOLS_PANEL_BUILD_NEXT.store(true, Ordering::SeqCst);
        return;
    }
    if !DEVTOOLS_PANEL_BUILD_NEXT.swap(false, Ordering::SeqCst) {
        return;
    }
    if app.devtools_panel.is_some() || !app.devtools_open {
        return;
    }

    win_startup("lazy: devtools panel");
    log_windows_debug("devtools: creating docked panel webview");
    app.devtools_panel = Some(build_devtools_panel(&app.window, app.ww, app.wh));
    resize_all(
        &app.window,
        &app.toolbar,
        &app.tabs,
        app.ww,
        app.wh,
        app.devtools_open,
        app.devtools_panel.as_ref(),
    );
    raise_toolbar(
        &app.toolbar,
        &app.window,
        Some(&app.tabs),
        app.devtools_panel.as_ref(),
    );
    if let (Some(panel), Some(tab)) = (
        app.devtools_panel.as_ref(),
        app.tabs.get(app.current),
    ) {
        sync_windows_devtools(panel, tab, true);
    }
}

#[cfg(target_os = "windows")]
fn request_devtools_panel() {
    DEVTOOLS_PANEL_PENDING.store(true, Ordering::SeqCst);
}

#[cfg(target_os = "windows")]
fn cancel_devtools_panel_request() {
    DEVTOOLS_PANEL_PENDING.store(false, Ordering::SeqCst);
    DEVTOOLS_PANEL_BUILD_NEXT.store(false, Ordering::SeqCst);
}

#[cfg(target_os = "windows")]
fn focus_main_window(window: &Window) {
    use tao::platform::windows::WindowExtWindows;
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
    };

    let hwnd_raw = window.hwnd();
    if hwnd_raw != 0 {
        unsafe {
            let hwnd = windows::Win32::Foundation::HWND(hwnd_raw as _);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
            let _ = BringWindowToTop(hwnd);
        }
    }
    window.set_focus();
}

#[cfg(target_os = "windows")]
#[derive(Copy, Clone)]
struct WinAppPtr(*mut UnsafeCell<WindowsRunState>);

#[cfg(target_os = "windows")]
unsafe impl Send for WinAppPtr {}

#[cfg(target_os = "windows")]
unsafe impl Sync for WinAppPtr {}

#[cfg(target_os = "windows")]
static WIN_APP_PTR: OnceLock<WinAppPtr> = OnceLock::new();

#[cfg(target_os = "windows")]
static TOOLBAR_DIRTY: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "windows")]
static WIN_IPC_FALLBACK: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

#[cfg(target_os = "windows")]
static DEVTOOLS_PANEL_PENDING: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "windows")]
static DEVTOOLS_PANEL_BUILD_NEXT: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "windows")]
static WIN_EXIT: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "windows")]
static WIN_IPC_READY: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "windows")]
fn win_ipc_fallback() -> &'static Mutex<Vec<String>> {
    WIN_IPC_FALLBACK.get_or_init(|| Mutex::new(Vec::new()))
}

#[cfg(target_os = "windows")]
fn flush_toolbar(toolbar: &WebView) {
    if !TOOLBAR_DIRTY.swap(false, Ordering::SeqCst) {
        return;
    }
    let snap = toolbar_snapshot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    match toolbar.load_url(&windows_toolbar_data_url(&snap)) {
        Ok(()) => log_windows_debug("sync_toolbar reload ok"),
        Err(err) => log_windows_debug(&format!("sync_toolbar reload failed: {err}")),
    }
}

#[cfg(target_os = "windows")]
unsafe fn drain_pending_ipc(app_ptr: WinAppPtr, control_flow: &mut ControlFlow) {
    if !WIN_IPC_READY.load(Ordering::SeqCst) {
        return;
    }
    loop {
        let batch = win_ipc_fallback()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect::<Vec<_>>();
        if batch.is_empty() {
            break;
        }
        for msg in batch {
            if msg == "ready" {
                continue;
            }
            log_windows_debug(&format!("handled ipc: {msg}"));
            let app = win_app_mut(app_ptr);
            let mut flow = ControlFlow::Wait;
            let mut ctx = IpcContext {
                control_flow: &mut flow,
                window: &app.window,
                toolbar: &app.toolbar,
                tabs: &mut app.tabs,
                current: &mut app.current,
                next_id: &mut app.next_id,
                proxy: &app.proxy,
                ww: &mut app.ww,
                wh: &mut app.wh,
                devtools_open: &mut app.devtools_open,
                devtools_panel: app.devtools_panel.as_ref(),
            };
            process_ipc(&msg, &mut ctx);
            if matches!(flow, ControlFlow::Exit) {
                WIN_EXIT.store(true, Ordering::SeqCst);
                *control_flow = ControlFlow::Exit;
            }
        }
    }
    let app = win_app_mut(app_ptr);
    flush_toolbar(&app.toolbar);
}

#[cfg(target_os = "windows")]
unsafe fn win_app_mut<'a>(ptr: WinAppPtr) -> &'a mut WindowsRunState {
    &mut *(*ptr.0).get()
}

#[cfg(target_os = "windows")]
fn register_win_app(ptr: WinAppPtr) {
    let _ = WIN_APP_PTR.set(ptr);
    log_windows_debug(&format!("win ipc host installed ptr={:p}", ptr.0));
}

#[cfg(target_os = "windows")]
fn dispatch_win_ipc(msg: &str) {
    if msg == "ready" {
        return;
    }
    win_ipc_fallback()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(msg.to_string());
    if let Some(proxy) = EVENT_PROXY.get() {
        let _ = proxy.send_event(UserEvent::WakeIpc);
    }
}

#[cfg(target_os = "windows")]
fn enqueue_ipc(msg: String) {
    dispatch_win_ipc(&msg);
}

fn content_load_url(webview: &WebView, url: &str) {
    if is_internal_newtab_url(url) || url == NEWTAB_URL {
        let _ = webview.load_url(newtab_data_url());
    } else {
        let _ = webview.load_url(url);
    }
}

struct IpcContext<'a> {
    control_flow: &'a mut ControlFlow,
    window: &'a Window,
    toolbar: &'a WebView,
    tabs: &'a mut Vec<Tab>,
    current: &'a mut usize,
    next_id: &'a mut u32,
    proxy: &'a EventLoopProxy<UserEvent>,
    ww: &'a mut f64,
    wh: &'a mut f64,
    devtools_open: &'a mut bool,
    #[cfg(target_os = "windows")]
    devtools_panel: Option<&'a WebView>,
}

fn process_ipc(msg: &str, ctx: &mut IpcContext<'_>) {
    match msg {
        "win_drag" => {
            let _ = ctx.window.drag_window();
        }
        "win_min" => ctx.window.set_minimized(true),
        "win_max_toggle" => {
            let m = ctx.window.is_maximized();
            ctx.window.set_maximized(!m);
            (*ctx.ww, *ctx.wh) = logical_size(ctx.window);
            resize_all(
                ctx.window,
                ctx.toolbar,
                ctx.tabs,
                *ctx.ww,
                *ctx.wh,
                *ctx.devtools_open,
                #[cfg(target_os = "windows")]
                ctx.devtools_panel,
            );
            raise_toolbar(
                ctx.toolbar,
                ctx.window,
                Some(ctx.tabs),
                #[cfg(target_os = "windows")]
                ctx.devtools_panel,
            );
            sync_toolbar(ctx.toolbar, ctx.tabs, *ctx.current);
        }
        "win_close" => {
            #[cfg(target_os = "windows")]
            WIN_EXIT.store(true, Ordering::SeqCst);
            *ctx.control_flow = ControlFlow::Exit;
        }
        "ready" => {
            #[cfg(not(target_os = "windows"))]
            sync_toolbar(ctx.toolbar, ctx.tabs, *ctx.current);
            raise_toolbar(
                ctx.toolbar,
                ctx.window,
                Some(ctx.tabs),
                #[cfg(target_os = "windows")]
                ctx.devtools_panel,
            );
            #[cfg(target_os = "windows")]
            log_windows_layout(ctx.toolbar, ctx.tabs, "toolbar_ready");
        }
        "focus_content" => focus_active_tab(ctx.tabs, *ctx.current),
        "nav_back" => {
            webview_go_back(&ctx.tabs[*ctx.current].webview);
            focus_active_tab(ctx.tabs, *ctx.current);
        }
        "nav_forward" => {
            webview_go_forward(&ctx.tabs[*ctx.current].webview);
            focus_active_tab(ctx.tabs, *ctx.current);
        }
        "nav_reload" => {
            let _ = ctx.tabs[*ctx.current].webview.reload();
            ctx.tabs[*ctx.current].loading = true;
            sync_toolbar(ctx.toolbar, ctx.tabs, *ctx.current);
            focus_active_tab(ctx.tabs, *ctx.current);
        }
        "nav_stop" => {
            ctx.tabs[*ctx.current].loading = false;
            webview_stop(&ctx.tabs[*ctx.current].webview);
            sync_toolbar(ctx.toolbar, ctx.tabs, *ctx.current);
            focus_active_tab(ctx.tabs, *ctx.current);
        }
        "nav_devtools" => {
            toggle_devtools(
                ctx.window,
                ctx.toolbar,
                ctx.tabs,
                *ctx.current,
                ctx.devtools_open,
                *ctx.ww,
                *ctx.wh,
                #[cfg(target_os = "windows")]
                ctx.devtools_panel,
            );
        }
        _ if msg.starts_with("load:") => {
            if let Some(rest) = msg.strip_prefix("load:") {
                if let Some(url) = resolve_omnibox_input(rest) {
                    let url = logical_tab_url(&url);
                    ctx.tabs[*ctx.current].url = url.clone();
                    ctx.tabs[*ctx.current].title.clear();
                    ctx.tabs[*ctx.current].loading = true;
                    content_load_url(&ctx.tabs[*ctx.current].webview, &url);
                    #[cfg(not(target_os = "windows"))]
                    let _ = ctx.tabs[*ctx.current].webview.focus();
                    sync_toolbar(ctx.toolbar, ctx.tabs, *ctx.current);
                    raise_toolbar(
                        ctx.toolbar,
                        ctx.window,
                        Some(ctx.tabs),
                        #[cfg(target_os = "windows")]
                        ctx.devtools_panel,
                    );
                }
            }
        }
        _ if msg.starts_with("new_tab:") => {
            open_new_tab(
                ctx.window,
                ctx.proxy,
                ctx.tabs,
                ctx.current,
                ctx.next_id,
                ctx.toolbar,
                NEWTAB_URL,
                *ctx.ww,
                *ctx.wh,
                *ctx.devtools_open,
                #[cfg(target_os = "windows")]
                ctx.devtools_panel,
            );
        }
        _ if msg.starts_with("switch_tab:") => {
            if let Some(rest) = msg.strip_prefix("switch_tab:") {
                if let Ok(idx) = rest.trim().parse::<usize>() {
                    if idx < ctx.tabs.len() {
                        *ctx.current = idx;
                        show_tab(
                            ctx.window,
                            ctx.tabs,
                            *ctx.current,
                            ctx.toolbar,
                            *ctx.devtools_open,
                            *ctx.ww,
                            *ctx.wh,
                            #[cfg(target_os = "windows")]
                            ctx.devtools_panel,
                        );
                        sync_toolbar(ctx.toolbar, ctx.tabs, *ctx.current);
                    }
                }
            }
        }
        _ if msg.starts_with("close_tab:") => {
            if let Some(rest) = msg.strip_prefix("close_tab:") {
                if let Ok(idx) = rest.trim().parse::<usize>() {
                    if idx < ctx.tabs.len() {
                        ctx.tabs.remove(idx);
                        if ctx.tabs.is_empty() {
                            open_new_tab(
                                ctx.window,
                                ctx.proxy,
                                ctx.tabs,
                                ctx.current,
                                ctx.next_id,
                                ctx.toolbar,
                                NEWTAB_URL,
                                *ctx.ww,
                                *ctx.wh,
                                *ctx.devtools_open,
                                #[cfg(target_os = "windows")]
                                ctx.devtools_panel,
                            );
                        } else {
                            if idx < *ctx.current {
                                *ctx.current -= 1;
                            } else {
                                *ctx.current = (*ctx.current).min(ctx.tabs.len() - 1);
                            }
                            show_tab(
                                ctx.window,
                                ctx.tabs,
                                *ctx.current,
                                ctx.toolbar,
                                *ctx.devtools_open,
                                *ctx.ww,
                                *ctx.wh,
                                #[cfg(target_os = "windows")]
                                ctx.devtools_panel,
                            );
                            sync_toolbar(ctx.toolbar, ctx.tabs, *ctx.current);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_app_event(
    event: Event<'_, UserEvent>,
    control_flow: &mut ControlFlow,
    window: &Window,
    toolbar: &WebView,
    devtools_panel: Option<&WebView>,
    tabs: &mut Vec<Tab>,
    current: &mut usize,
    next_id: &mut u32,
    proxy: &EventLoopProxy<UserEvent>,
    ww: &mut f64,
    wh: &mut f64,
    devtools_open: &mut bool,
    modifiers: &mut ModifiersState,
    z_order_nudges: Option<&mut u32>,
) {
    match event {
        Event::WindowEvent { event, .. } => match event {
            WindowEvent::CloseRequested => *control_flow = ControlFlow::Exit,

            WindowEvent::Resized(sz) => {
                let scale = window.scale_factor();
                (*ww, *wh) = logical_size_from_physical(sz, scale);
                resize_all(
                    window,
                    toolbar,
                    tabs,
                    *ww,
                    *wh,
                    *devtools_open,
                    #[cfg(target_os = "windows")]
                    devtools_panel,
                );
                raise_toolbar(
                    toolbar,
                    window,
                    Some(tabs),
                    #[cfg(target_os = "windows")]
                    devtools_panel,
                );
                sync_toolbar(toolbar, tabs, *current);
            }

            #[cfg(target_os = "windows")]
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(panel) = devtools_panel {
                    let scale = window.scale_factor();
                    let y = position.to_logical::<f64>(scale).y;
                    if y < toolbar_height() {
                        raise_toolbar(toolbar, window, Some(tabs), Some(panel));
                        let _ = toolbar.focus();
                    }
                }
            }

            WindowEvent::ScaleFactorChanged {
                scale_factor,
                new_inner_size,
            } => {
                (*ww, *wh) = logical_size_from_physical(*new_inner_size, scale_factor);
                resize_all(
                    window,
                    toolbar,
                    tabs,
                    *ww,
                    *wh,
                    *devtools_open,
                    #[cfg(target_os = "windows")]
                    devtools_panel,
                );
                raise_toolbar(
                    toolbar,
                    window,
                    Some(tabs),
                    #[cfg(target_os = "windows")]
                    devtools_panel,
                );
            }

            WindowEvent::ModifiersChanged(m) => *modifiers = m,

            WindowEvent::KeyboardInput {
                event:
                    tao::event::KeyEvent {
                        physical_key,
                        state: ElementState::Pressed,
                        repeat: false,
                        ..
                    },
                ..
            } if devtools_shortcut(physical_key, *modifiers) => {
                toggle_devtools(
                    window,
                    toolbar,
                    tabs,
                    *current,
                    devtools_open,
                    *ww,
                    *wh,
                    #[cfg(target_os = "windows")]
                    devtools_panel,
                );
            }

            WindowEvent::Focused(true) => {
                #[cfg(not(target_os = "windows"))]
                focus_active_tab(tabs, *current);
            }

            _ => {}
        },

        Event::UserEvent(UserEvent::ToggleDevtools) => {
            toggle_devtools(
                window,
                toolbar,
                tabs,
                *current,
                devtools_open,
                *ww,
                *wh,
                #[cfg(target_os = "windows")]
                devtools_panel,
            );
        }

        Event::UserEvent(UserEvent::FocusTab { tab_id }) => {
            if let Some(idx) = find_tab_idx(tabs, tab_id) {
                if idx == *current {
                    focus_active_tab(tabs, *current);
                }
            }
        }

        Event::UserEvent(UserEvent::Navigated { tab_id, url }) => {
            if url == "about:blank" {
                return;
            }
            if let Some(idx) = find_tab_idx(tabs, tab_id) {
                let logical = logical_tab_url(&url);
                let was_external = !is_internal_newtab_url(&tabs[idx].url);
                // Ignore transient internal URLs while an external page is loading.
                if tabs[idx].loading && was_external && is_internal_newtab_url(&logical) {
                    return;
                }
                if tabs[idx].url == logical {
                    return;
                }
                tabs[idx].url = logical;
                if idx == *current {
                    sync_toolbar(toolbar, tabs, *current);
                    focus_active_tab(tabs, *current);
                } else {
                    sync_toolbar(toolbar, tabs, *current);
                }
            }
        }

        Event::UserEvent(UserEvent::LoadStarted { tab_id }) => {
            if let Some(idx) = find_tab_idx(tabs, tab_id) {
                tabs[idx].loading = true;
                if idx == *current {
                    sync_toolbar(toolbar, tabs, *current);
                }
            }
        }

        Event::UserEvent(UserEvent::LoadFinished { tab_id }) => {
            if let Some(idx) = find_tab_idx(tabs, tab_id) {
                tabs[idx].loading = false;
                if idx == *current {
                    sync_toolbar(toolbar, tabs, *current);
                }
            }
        }

        Event::UserEvent(UserEvent::ForceLoad { tab_id, url }) => {
            if let Some(idx) = find_tab_idx(tabs, tab_id) {
                let url = logical_tab_url(&url);
                tabs[idx].url = url.clone();
                tabs[idx].title.clear();
                tabs[idx].loading = true;
                content_load_url(&tabs[idx].webview, &url);
                if idx == *current {
                    sync_toolbar(toolbar, tabs, *current);
                }
            }
        }

        Event::UserEvent(UserEvent::TitleChanged { tab_id, title }) => {
            if let Some(idx) = find_tab_idx(tabs, tab_id) {
                tabs[idx].title = title;
                #[cfg(target_os = "windows")]
                sync_toolbar(toolbar, tabs, *current);
                #[cfg(not(target_os = "windows"))]
                {
                    let titles = tabs.iter().map(tab_label).collect::<Vec<_>>();
                    let script = format!(
                        "window.__setTabTitle({}, {});",
                        idx,
                        json!(titles[idx])
                    );
                    let _ = toolbar.evaluate_script(&script);
                }
            }
        }

        Event::UserEvent(UserEvent::NewWindow { url }) => {
            if let Some(url) = resolve_omnibox_input(&url) {
                open_new_tab(
                    window,
                    proxy,
                    tabs,
                    current,
                    next_id,
                    toolbar,
                    &url,
                    *ww,
                    *wh,
                    *devtools_open,
                    #[cfg(target_os = "windows")]
                    devtools_panel,
                );
            }
        }

        Event::UserEvent(UserEvent::WakeIpc) => {}

        Event::UserEvent(UserEvent::Ipc(msg)) => {
            #[cfg(target_os = "windows")]
            log_windows_ipc(&msg);
            let mut ipc_ctx = IpcContext {
                control_flow,
                window,
                toolbar,
                tabs,
                current,
                next_id,
                proxy,
                ww,
                wh,
                devtools_open,
                #[cfg(target_os = "windows")]
                devtools_panel: devtools_panel,
            };
            process_ipc(&msg, &mut ipc_ctx);
        }

        #[cfg(target_os = "windows")]
        Event::MainEventsCleared => {
            if let Some(nudges) = z_order_nudges {
                if let Some(panel) = devtools_panel {
                    if *nudges < 60 {
                        raise_toolbar(toolbar, window, Some(tabs), Some(panel));
                        *nudges += 1;
                    }
                }
            }
        }

        _ => {}
    }
}

fn build_window(event_loop: &tao::event_loop::EventLoop<UserEvent>) -> Window {
    let mut builder = WindowBuilder::new()
        .with_title(version_label())
        .with_inner_size(LogicalSize::new(1200.0, 800.0));

    if let Some(icon) = load_window_icon() {
        builder = builder.with_window_icon(Some(icon));
    }

    #[cfg(target_os = "macos")]
    {
        builder = builder
            .with_decorations(true)
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true);
    }

    #[cfg(target_os = "windows")]
    {
        // Visible from the start — if content webview creation stalls, the shell still appears.
        builder = builder.with_decorations(false);
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        builder = builder.with_decorations(false);
    }

    builder
        .build(event_loop)
        .expect("failed to create window")
}


#[cfg(target_os = "windows")]
fn bootstrap_win_app(app: &mut WindowsRunState) {
    match app.bootstrap_phase {
        2 => return,
        0 => {
            win_startup("bootstrap: show window");
            app.window.set_visible(true);
            focus_main_window(&app.window);
            app.startup_focus_frames = 120;
            app.bootstrap_phase = 1;
            log_windows_debug("bootstrap: window shown (phase 0)");
        }
        1 => {
            win_startup("bootstrap: content webview");
            let webview = build_content_webview(
                &app.window,
                app.proxy.clone(),
                1,
                NEWTAB_URL,
                app.ww,
                app.wh,
                true,
                app.devtools_open,
            );
            app.tabs.push(Tab {
                id: 1,
                url: NEWTAB_URL.to_string(),
                title: "Новая вкладка".to_string(),
                loading: false,
                webview,
            });
            app.current = 0;
            app.next_id = 2;

            focus_main_window(&app.window);
            raise_toolbar(
                &app.toolbar,
                &app.window,
                Some(&app.tabs),
                app.devtools_panel.as_ref(),
            );
            resize_all(
                &app.window,
                &app.toolbar,
                &app.tabs,
                app.ww,
                app.wh,
                app.devtools_open,
                app.devtools_panel.as_ref(),
            );
            sync_toolbar(&app.toolbar, &app.tabs, app.current);
            log_windows_layout(&app.toolbar, &app.tabs, "startup");
            WIN_IPC_READY.store(true, Ordering::SeqCst);
            flush_toolbar(&app.toolbar);
            app.bootstrap_phase = 2;
            log_windows_debug("bootstrap complete");
        }
        _ => {}
    }
}

#[cfg(target_os = "windows")]
fn tick_startup_focus(app: &mut WindowsRunState) {
    if app.startup_focus_frames == 0 {
        return;
    }
    app.startup_focus_frames -= 1;
    if app.startup_focus_frames % 15 == 0 {
        focus_main_window(&app.window);
    }
}

#[cfg(target_os = "windows")]
fn run_windows_app(
    event_loop: tao::event_loop::EventLoop<UserEvent>,
    state: WindowsRunState,
) {
    log_windows_debug("run_windows_app start");
    let raw = Box::into_raw(Box::new(UnsafeCell::new(state)));
    let app_ptr = WinAppPtr(raw);
    register_win_app(app_ptr);

    log_windows_debug("entering event loop");
    event_loop.run(move |event, _, control_flow| {
        if WIN_EXIT.load(Ordering::SeqCst) {
            *control_flow = ControlFlow::Exit;
        }
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(16));
        unsafe {
            bootstrap_win_app(win_app_mut(app_ptr));
            tick_startup_focus(win_app_mut(app_ptr));
            drain_pending_ipc(app_ptr, control_flow);
            tick_devtools_connect(win_app_mut(app_ptr));
        }
        if *control_flow == ControlFlow::Exit {
            return;
        }
        unsafe {
            let app = win_app_mut(app_ptr);
            dispatch_app_event(
                event,
                control_flow,
                &app.window,
                &app.toolbar,
                app.devtools_panel.as_ref(),
                &mut app.tabs,
                &mut app.current,
                &mut app.next_id,
                &app.proxy,
                &mut app.ww,
                &mut app.wh,
                &mut app.devtools_open,
                &mut app.modifiers,
                Some(&mut app.z_order_nudges),
            );
            tick_devtools_panel_build(app);
        }
    });
}


#[cfg(target_os = "windows")]
fn ensure_webview2_debug_port() {
    const VAR: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";
    const FLAG: &str = "--remote-debugging-port=9222";
    match std::env::var(VAR) {
        Ok(existing) if existing.contains("remote-debugging-port") => {}
        Ok(existing) => {
            std::env::set_var(VAR, format!("{existing} {FLAG}"));
        }
        Err(_) => {
            std::env::set_var(VAR, FLAG);
        }
    }
}

fn main() {
    #[cfg(target_os = "windows")]
    init_windows_debug_log();

    #[cfg(target_os = "windows")]
    ensure_webview2_debug_port();

    #[cfg(target_os = "windows")]
    install_windows_panic_hook();

    #[cfg(target_os = "windows")]
    win_startup("main begin");

    #[cfg(target_os = "windows")]
    win_startup("creating event loop");
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    #[cfg(target_os = "windows")]
    let _ = EVENT_PROXY.set(proxy.clone());

    #[cfg(target_os = "windows")]
    win_startup("building window");
    let window = build_window(&event_loop);
    set_dock_icon();

    #[cfg_attr(target_os = "windows", allow(unused_mut))]
    let (mut ww, mut wh) = logical_size(&window);

    let mut next_id: u32 = 1;
    #[cfg_attr(target_os = "windows", allow(unused_mut))]
    let mut devtools_open = false;
    #[cfg_attr(target_os = "windows", allow(unused_mut))]
    let mut modifiers = ModifiersState::empty();

    #[cfg(target_os = "windows")]
    win_startup("building toolbar");
    let toolbar = build_toolbar(&window, proxy.clone(), ww);

    #[cfg(target_os = "windows")]
    win_startup("entering run_windows_app");
    #[cfg(target_os = "windows")]
    {
        run_windows_app(
            event_loop,
            WindowsRunState {
                window,
                toolbar,
                devtools_panel: None,
                tabs: vec![],
                current: 0,
                next_id: 1,
                ww,
                wh,
                devtools_open,
                modifiers,
                z_order_nudges: 0,
                proxy,
                bootstrap_phase: 0,
                startup_focus_frames: 0,
            },
        );
        return;
    }

    let first_webview = build_content_webview(
        &window,
        proxy.clone(),
        next_id,
        NEWTAB_URL,
        ww,
        wh,
        true,
        devtools_open,
    );
    next_id += 1;

    #[cfg_attr(target_os = "windows", allow(unused_mut))]
    let mut tabs = vec![Tab {
        id: 1,
        url: NEWTAB_URL.to_string(),
        title: "Новая вкладка".to_string(),
        loading: false,
        webview: first_webview,
    }];
    #[cfg_attr(target_os = "windows", allow(unused_mut))]
    let mut current: usize = 0;

    #[cfg(not(target_os = "windows"))]
    {
        raise_toolbar(
            &toolbar,
            &window,
            Some(&tabs),
        );

        resize_all(
            &window,
            &toolbar,
            &tabs,
            ww,
            wh,
            devtools_open,
        );

        sync_toolbar(&toolbar, &tabs, current);

        event_loop.run(move |event, _, control_flow| {
            *control_flow = ControlFlow::Wait;
            dispatch_app_event(
                event,
                control_flow,
                &window,
                &toolbar,
                None,
                &mut tabs,
                &mut current,
                &mut next_id,
                &proxy,
                &mut ww,
                &mut wh,
                &mut devtools_open,
                &mut modifiers,
                None,
            );
        });
    }
}

#[cfg(target_os = "windows")]
mod win_devtools {
    use serde_json::Value;
    use std::time::Duration;
    use wry::WebView;

    const CDP_BASE: &str = "http://127.0.0.1:9222";

    pub fn close_panel(panel: &WebView) {
        let _ = panel.set_visible(false);
        let _ = panel.load_url("about:blank");
    }

    pub fn inspector_url_for_page(page_url: &str) -> Option<String> {
        let targets = fetch_targets()?;
        pick_target(&targets, page_url).and_then(|target| {
            if let Some(path) = target.get("devtoolsFrontendUrl").and_then(|v| v.as_str()) {
                return Some(frontend_url(path));
            }
            let ws = target.get("webSocketDebuggerUrl")?.as_str()?;
            Some(format!(
                "{CDP_BASE}/devtools/inspector.html?ws={}",
                ws.trim_start_matches("ws://")
            ))
        })
    }

    fn frontend_url(path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{CDP_BASE}{path}")
        }
    }

    fn fetch_targets() -> Option<Vec<Value>> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_millis(400))
            .timeout_read(Duration::from_millis(400))
            .build();
        for endpoint in ["/json/list", "/json"] {
            if let Ok(resp) = agent.get(&format!("{CDP_BASE}{endpoint}")).call() {
                if let Ok(targets) = resp.into_json::<Vec<Value>>() {
                    if !targets.is_empty() {
                        return Some(targets);
                    }
                }
            }
        }
        None
    }

    fn normalize_url(url: &str) -> String {
        url.trim_end_matches('/').to_string()
    }

    fn urls_match(a: &str, b: &str) -> bool {
        normalize_url(a) == normalize_url(b)
    }

    fn target_matches_page(target_url: &str, page_url: &str) -> bool {
        if urls_match(target_url, page_url) {
            return true;
        }
        let target = normalize_url(target_url);
        let page = normalize_url(page_url);
        if page == "plum://newtab" {
            return target.contains("newtab")
                || target.starts_with("data:text/html")
                || target.contains("plum.newtab");
        }
        false
    }

    fn pick_target<'a>(targets: &'a [Value], page_url: &str) -> Option<&'a Value> {
        let pages: Vec<&Value> = targets
            .iter()
            .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
            .collect();

        pages
            .iter()
            .copied()
            .find(|t| {
                t.get("url")
                    .and_then(|v| v.as_str())
                    .is_some_and(|u| target_matches_page(u, page_url))
            })
            .or_else(|| {
                pages
                    .iter()
                    .copied()
                    .find(|t| t.get("url").and_then(|v| v.as_str()).is_some_and(|u| !u.is_empty()))
            })
            .or_else(|| pages.last().copied())
    }
}
