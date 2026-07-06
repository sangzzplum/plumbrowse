//! PlumBrowser — лёгкий кросс-платформенный браузер на Rust.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use serde_json::json;
use std::borrow::Cow;
use std::path::PathBuf;
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
fn log_windows_debug(msg: &str) {
    use std::io::Write;
    let path = std::env::temp_dir().join("plumbrowser_debug.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{msg}");
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

/// Content HWND sometimes spans the full window while only painting below the toolbar.
/// Clip hit-testing so clicks in the toolbar band reach the toolbar webview.
#[cfg(target_os = "windows")]
fn enforce_content_clipping(webview: &WebView, window: &Window) {
    use tao::platform::windows::WindowExtWindows;
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{CreateRectRgn, MapWindowPoints, SetWindowRgn};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    let Some(host) = webview_host_hwnd(webview) else {
        return;
    };
    let scale = window.scale_factor();
    let toolbar_phys = (toolbar_height() * scale).round() as i32;
    let parent = HWND(window.hwnd() as _);

    let mut host_rect = RECT::default();
    if unsafe { GetWindowRect(host, &mut host_rect) }.is_err() {
        return;
    }

    let mut top_left = POINT {
        x: host_rect.left,
        y: host_rect.top,
    };
    if unsafe { MapWindowPoints(None, Some(parent), std::slice::from_mut(&mut top_left)) } == 0 {
        return;
    }

    if top_left.y >= toolbar_phys {
        unsafe {
            let _ = SetWindowRgn(host, None, true);
        }
        return;
    }

    let client_w = host_rect.right - host_rect.left;
    let client_h = host_rect.bottom - host_rect.top;
    let clip_top = (toolbar_phys - top_left.y).clamp(0, client_h);
    if clip_top >= client_h {
        return;
    }

    unsafe {
        let rgn = CreateRectRgn(0, clip_top, client_w, client_h);
        if !rgn.is_invalid() {
            let _ = SetWindowRgn(host, rgn, true);
        }
    }
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

#[cfg(target_os = "windows")]
fn sync_windows_devtools(panel: &WebView, tab: &Tab, open: bool) {
    if open {
        win_devtools::open_in_panel(panel, &tab.url);
    } else {
        win_devtools::close_panel(panel);
    }
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
  document.addEventListener('keydown', e => {
    const k = e.key.toLowerCase();
    if (e.key === 'F12') {
      e.preventDefault();
      e.stopPropagation();
      if (window.ipc && window.ipc.postMessage) window.ipc.postMessage('toggle_devtools');
      return;
    }
    if (e.ctrlKey && e.shiftKey && k === 'i') {
      e.preventDefault();
      e.stopPropagation();
      if (window.ipc && window.ipc.postMessage) window.ipc.postMessage('toggle_devtools');
      return;
    }
    if (e.metaKey && e.altKey && k === 'i') {
      e.preventDefault();
      e.stopPropagation();
      if (window.ipc && window.ipc.postMessage) window.ipc.postMessage('toggle_devtools');
    }
  }, true);
"#;

#[cfg(target_os = "windows")]
const DEVTOOLS_PANEL_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"/><style>
  html,body{margin:0;height:100%;background:#1e1e1e;overflow:hidden}
</style></head><body></body></html>"#;

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
    webview: WebView,
}

#[derive(Debug, Clone)]
enum UserEvent {
    Ipc(String),
    ToggleDevtools,
    Navigated { tab_id: u32, url: String },
    TitleChanged { tab_id: u32, title: String },
    NewWindow { url: String },
    FocusTab { tab_id: u32 },
}

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
        return Some(format!("https://{u}"));
    }

    // Otherwise search.
    Some(format!(
        "https://www.google.com/search?q={}",
        percent_encode_query(u)
    ))
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

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
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
    if tab.url.starts_with("plum://") {
        return "Новая вкладка".to_string();
    }
    tab.url
        .replace("https://", "")
        .replace("http://", "")
}

fn sync_toolbar(toolbar: &WebView, tabs: &[Tab], current: usize) {
    let titles = tabs.iter().map(tab_label).collect::<Vec<_>>();
    let urls = tabs.iter().map(|t| t.url.clone()).collect::<Vec<_>>();
    let cur_url = tabs.get(current).map(|t| t.url.as_str()).unwrap_or("");

    let script = format!(
        r#"(function(){{
  var titles = {titles};
  var urls = {urls};
  var cur = {current};
  var url = {cur_url};
  function apply() {{
    if (typeof window.__setState === 'function') {{
      window.__setState(titles, urls, cur, url);
      return true;
    }}
    return false;
  }}
  if (!apply()) {{
    window.__pendingState = [titles, urls, cur, url];
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
        cur_url = json!(cur_url)
    );
    #[cfg(target_os = "windows")]
    match toolbar.evaluate_script(&script) {
        Ok(()) => log_windows_debug("sync_toolbar ok"),
        Err(err) => log_windows_debug(&format!("sync_toolbar failed: {err}")),
    }
    #[cfg(not(target_os = "windows"))]
    let _ = toolbar.evaluate_script(&script);
}

fn plum_protocol(_id: WebViewId, req: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let uri = req.uri();
    let host = uri.host().unwrap_or_default();
    let path = uri.path();
    let full = uri.to_string();

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
        let html = toolbar_html();
        return Response::builder()
            .status(200)
            .header(CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Cow::Owned(html.into_bytes()))
            .unwrap();
    }

    let (mime, body): (&str, Cow<'static, [u8]>) = match path {
        "/newtab" | "/newtab/" | "/" if host.is_empty() || host == "newtab" => (
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
    :root { --bg:#0f1115; --fg:#e8eaed; --mut:#9aa0a6; --card:#171a21; }
    body{
      margin:0; height:100vh; display:grid; place-items:center;
      background:radial-gradient(1200px 600px at 50% 20%, #1b2333, var(--bg));
      color:var(--fg); font:16px/1.35 system-ui, Segoe UI, Arial;
    }
    .wrap{ width:min(720px, 92vw); }
    .logo{ font-size:34px; font-weight:800; margin-bottom:14px; }
    .hint{ color:var(--mut); margin-bottom:18px; }
    .card{ background:rgba(23,26,33,0.82); border:1px solid rgba(255,255,255,0.06); border-radius:18px; padding:18px; }
  </style>
</head>
<body>
  <div class="wrap">
    <div class="logo">PlumBrowser</div>
    <div class="hint">Введите адрес в строку поиска выше.</div>
    <div class="card">Кликайте по ссылки — URL обновится автоматически. Cmd+клик и target="_blank" откроют новую вкладку.</div>
  </div>
</body>
</html>
"#;

const TOOLBAR_SCRIPT: &str = r#"
    function post(msg) {
      // Navigation IPC is reliable on WebView2 toolbars; postMessage often exists but is a no-op.
      try {
        window.location.replace('plum://ipc/' + encodeURIComponent(msg));
      } catch (e) {}
      try {
        if (window.chrome && window.chrome.webview && window.chrome.webview.postMessage) {
          window.chrome.webview.postMessage(msg);
        } else if (window.ipc && window.ipc.postMessage) {
          window.ipc.postMessage(msg);
        }
      } catch (e2) {}
    }

    function svgFallbackDataUrl() {
      const svg =
        '<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 24 24">' +
        '<circle cx="12" cy="12" r="9.4" fill="none" stroke="rgba(232,234,237,0.85)" stroke-width="1.6"/>' +
        '<path d="M2.8 12h18.4" fill="none" stroke="rgba(154,160,166,0.85)" stroke-width="1.2"/>' +
        '<path d="M12 2.8c2.6 2.6 4.1 6 4.1 9.2s-1.5 6.6-4.1 9.2c-2.6-2.6-4.1-6-4.1-9.2S9.4 5.4 12 2.8Z" fill="none" stroke="rgba(154,160,166,0.85)" stroke-width="1.2"/>' +
        '</svg>';
      return 'data:image/svg+xml;utf8,' + encodeURIComponent(svg);
    }

    function faviconUrl(pageUrl) {
      try {
        const u = new URL(pageUrl);
        // simplest "real" favicon path; many sites still serve it
        return u.origin + '/favicon.ico';
      } catch (e) {
        return svgFallbackDataUrl();
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
      bindBtn('reload', () => post('nav_reload'));
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
        window.__setState(pending[0], pending[1], pending[2], pending[3]);
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

    window.__setState = function(tabTitles, tabUrls, current, url) {
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
        icon.onerror = () => { icon.onerror = null; icon.src = svgFallbackDataUrl(); };

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
      height:32px; padding:0 28px 0 8px; border-radius:12px; background:var(--b);
      cursor:pointer; user-select:none; box-sizing:border-box;
    }
    .tab.active { background:var(--b2); }
    .tab-icon { flex:0 0 auto; width:16px; height:16px; opacity:0.95; display:block; border-radius:4px; }
    .tab-title {
      min-width:0; overflow:hidden; white-space:nowrap;
      text-overflow:ellipsis; flex:1 1 auto;
    }
    .tab-close {
      position:absolute; right:6px; top:4px;
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
      <input id="url" placeholder="example.com или https://example.com" autocomplete="off" spellcheck="false" />
      <div class="go" id="go">Go</div>
    </div>
  </div>
  <script>{script}</script>
</body>
</html>"#,
            script = TOOLBAR_SCRIPT,
            tab_bar_css = TAB_BAR_CSS
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
        <input id="url" placeholder="адрес или поиск" autocomplete="off" spellcheck="false" />
        <div class="go" id="go">Go</div>
      </div>
    </div>
  </div>
  <script>{script}</script>
</body>
</html>"#,
        script = TOOLBAR_SCRIPT,
        tab_bar_css = format!("{TAB_BAR_CSS}\n{WINDOWS_TAB_BAR_CSS}"),
        version = version_label(),
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
    let builder = WebViewBuilder::new()
        .with_url(url)
        .with_user_agent(content_user_agent())
        .with_bounds(bounds_content(ww, wh, devtools_open))
        .with_visible(visible)
        .with_focused(if cfg!(target_os = "windows") { false } else { visible })
        .with_clipboard(true)
        .with_back_forward_navigation_gestures(true)
        .with_devtools(!cfg!(target_os = "windows"))
        .with_hotkeys_zoom(true)
        .with_initialization_script(CONTENT_SHORTCUT_SCRIPT)
        .with_custom_protocol("plum".to_string(), plum_protocol)
        .with_ipc_handler({
            let proxy = proxy.clone();
            move |req: Request<String>| {
                if req.body() == "toggle_devtools" {
                    let _ = proxy.send_event(UserEvent::ToggleDevtools);
                }
            }
        })
        .with_on_page_load_handler({
            let proxy = proxy.clone();
            move |event, _| {
                if matches!(event, PageLoadEvent::Finished) {
                    let _ = proxy.send_event(UserEvent::FocusTab { tab_id });
                }
            }
        })
        .with_navigation_handler({
            let proxy = proxy.clone();
            move |nav_url| {
                let _ = proxy.send_event(UserEvent::Navigated {
                    tab_id,
                    url: nav_url,
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
        .with_additional_browser_args("--remote-debugging-port=9222 --remote-allow-origins=*");

    builder
        .build_as_child(window)
        .expect("failed to build content webview")
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
    for tab in tabs {
        enforce_content_clipping(&tab.webview, window);
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
        if let Some(panel) = devtools_panel {
            sync_windows_devtools(panel, tab, false);
        }
        *devtools_open = false;
    } else {
        *devtools_open = true;
        open_docked_devtools(&tab.webview);
        #[cfg(target_os = "windows")]
        if let Some(panel) = devtools_panel {
            sync_windows_devtools(panel, tab, true);
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
    let proxy_page = proxy.clone();
    let proxy_ipc = proxy.clone();
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
                let _ = proxy_nav.send_event(UserEvent::Ipc(msg));
                return false;
            }
            toolbar_navigation_allowed(&url)
        })
        .with_on_page_load_handler(move |event, _| {
            if matches!(event, PageLoadEvent::Finished) {
                let _ = proxy_page.send_event(UserEvent::Ipc("ready".to_string()));
            }
        })
        .with_ipc_handler(move |req: Request<String>| {
            let _ = proxy_ipc.send_event(UserEvent::Ipc(req.body().clone()));
        });

    let html = toolbar_html();
    let builder = builder
        .with_html(&html)
        .with_custom_protocol("plum".to_string(), plum_protocol);

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
        url: url.to_string(),
        title,
        webview,
    });
    *current = tabs.len() - 1;
    if devtools_open {
        open_docked_devtools(&tabs[*current].webview);
        #[cfg(target_os = "windows")]
        if let Some(panel) = devtools_panel {
            sync_windows_devtools(panel, &tabs[*current], true);
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


fn main() {
    #[cfg(target_os = "windows")]
    std::env::set_var(
        "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
        "--remote-debugging-port=9222 --remote-allow-origins=*",
    );

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = build_window(&event_loop);
    set_dock_icon();

    let (mut ww, mut wh) = logical_size(&window);

    let mut next_id: u32 = 1;
    let mut devtools_open = false;
    let mut modifiers = ModifiersState::empty();

    // Windows: toolbar must be created before content webviews or HTML often stays blank (white bar).
    let toolbar = build_toolbar(&window, proxy.clone(), ww);

    #[cfg(target_os = "windows")]
    let devtools_panel = build_devtools_panel(&window, ww, wh);

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

    let mut tabs = vec![Tab {
        id: 1,
        url: NEWTAB_URL.to_string(),
        title: "Новая вкладка".to_string(),
        webview: first_webview,
    }];
    let mut current: usize = 0;

    #[cfg(target_os = "windows")]
    let mut z_order_nudges: u32 = 0;

    raise_toolbar(
        &toolbar,
        &window,
        Some(&tabs),
        #[cfg(target_os = "windows")]
        Some(&devtools_panel),
    );

    resize_all(
        &window,
        &toolbar,
        &tabs,
        ww,
        wh,
        devtools_open,
        #[cfg(target_os = "windows")]
        Some(&devtools_panel),
    );

    sync_toolbar(&toolbar, &tabs, current);

    #[cfg(target_os = "windows")]
    log_windows_layout(&toolbar, &tabs, "startup");

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => *control_flow = ControlFlow::Exit,

                WindowEvent::Resized(sz) => {
                    let scale = window.scale_factor();
                    (ww, wh) = logical_size_from_physical(sz, scale);
                    resize_all(
                        &window,
                        &toolbar,
                        &tabs,
                        ww,
                        wh,
                        devtools_open,
                        #[cfg(target_os = "windows")]
                        Some(&devtools_panel),
                    );
                    raise_toolbar(
                        &toolbar,
                        &window,
                        Some(&tabs),
                        #[cfg(target_os = "windows")]
                        Some(&devtools_panel),
                    );
                    sync_toolbar(&toolbar, &tabs, current);
                }

                WindowEvent::ScaleFactorChanged {
                    scale_factor,
                    new_inner_size,
                } => {
                    (ww, wh) = logical_size_from_physical(*new_inner_size, scale_factor);
                    resize_all(
                        &window,
                        &toolbar,
                        &tabs,
                        ww,
                        wh,
                        devtools_open,
                        #[cfg(target_os = "windows")]
                        Some(&devtools_panel),
                    );
                    raise_toolbar(
                        &toolbar,
                        &window,
                        Some(&tabs),
                        #[cfg(target_os = "windows")]
                        Some(&devtools_panel),
                    );
                }

                WindowEvent::ModifiersChanged(m) => modifiers = m,

                WindowEvent::KeyboardInput {
                    event:
                        tao::event::KeyEvent {
                            physical_key,
                            state: ElementState::Pressed,
                            repeat: false,
                            ..
                        },
                    ..
                } if devtools_shortcut(physical_key, modifiers) => {
                    toggle_devtools(
                        &window,
                        &toolbar,
                        &tabs,
                        current,
                        &mut devtools_open,
                        ww,
                        wh,
                        #[cfg(target_os = "windows")]
                        Some(&devtools_panel),
                    );
                }

                WindowEvent::Focused(true) => {
                    #[cfg(not(target_os = "windows"))]
                    focus_active_tab(&tabs, current);
                }

                _ => {}
            },

            Event::UserEvent(UserEvent::ToggleDevtools) => {
                toggle_devtools(
                    &window,
                    &toolbar,
                    &tabs,
                    current,
                    &mut devtools_open,
                    ww,
                    wh,
                    #[cfg(target_os = "windows")]
                    Some(&devtools_panel),
                );
            }

            Event::UserEvent(UserEvent::FocusTab { tab_id }) => {
                if let Some(idx) = find_tab_idx(&tabs, tab_id) {
                    if idx == current {
                        focus_active_tab(&tabs, current);
                    }
                }
            }

            Event::UserEvent(UserEvent::Navigated { tab_id, url }) => {
                if url == "about:blank" {
                    // WebView2 sometimes emits transient navigations; don't reflect them into UI.
                    return;
                }
                if let Some(idx) = find_tab_idx(&tabs, tab_id) {
                    tabs[idx].url = url;
                    if idx == current {
                        sync_toolbar(&toolbar, &tabs, current);
                        focus_active_tab(&tabs, current);
                    } else {
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

            Event::UserEvent(UserEvent::TitleChanged { tab_id, title }) => {
                if let Some(idx) = find_tab_idx(&tabs, tab_id) {
                    tabs[idx].title = title;
                    let titles = tabs.iter().map(tab_label).collect::<Vec<_>>();
                    let script = format!(
                        "window.__setTabTitle({}, {});",
                        idx,
                        json!(titles[idx])
                    );
                    let _ = toolbar.evaluate_script(&script);
                }
            }

            Event::UserEvent(UserEvent::NewWindow { url }) => {
                if let Some(url) = resolve_omnibox_input(&url) {
                    open_new_tab(
                        &window,
                        &proxy,
                        &mut tabs,
                        &mut current,
                        &mut next_id,
                        &toolbar,
                        &url,
                        ww,
                        wh,
                        devtools_open,
                        #[cfg(target_os = "windows")]
                        Some(&devtools_panel),
                    );
                }
            }

            Event::UserEvent(UserEvent::Ipc(msg)) => {
                #[cfg(target_os = "windows")]
                log_windows_ipc(&msg);
                match msg.as_str() {
                "win_drag" => {
                    let _ = window.drag_window();
                }
                "win_min" => window.set_minimized(true),
                "win_max_toggle" => {
                    let m = window.is_maximized();
                    window.set_maximized(!m);
                    (ww, wh) = logical_size(&window);
                    resize_all(
                        &window,
                        &toolbar,
                        &tabs,
                        ww,
                        wh,
                        devtools_open,
                        #[cfg(target_os = "windows")]
                        Some(&devtools_panel),
                    );
                    raise_toolbar(
                        &toolbar,
                        &window,
                        Some(&tabs),
                        #[cfg(target_os = "windows")]
                        Some(&devtools_panel),
                    );
                    sync_toolbar(&toolbar, &tabs, current);
                }
                "win_close" => *control_flow = ControlFlow::Exit,

                "ready" => {
                    sync_toolbar(&toolbar, &tabs, current);
                    raise_toolbar(
                        &toolbar,
                        &window,
                        Some(&tabs),
                        #[cfg(target_os = "windows")]
                        Some(&devtools_panel),
                    );
                    #[cfg(target_os = "windows")]
                    log_windows_layout(&toolbar, &tabs, "toolbar_ready");
                }

                "focus_content" => focus_active_tab(&tabs, current),

                "nav_back" => {
                    webview_go_back(&tabs[current].webview);
                    focus_active_tab(&tabs, current);
                }
                "nav_forward" => {
                    webview_go_forward(&tabs[current].webview);
                    focus_active_tab(&tabs, current);
                }
                "nav_reload" => {
                    let _ = tabs[current].webview.reload();
                    focus_active_tab(&tabs, current);
                }

                "nav_devtools" => {
                    toggle_devtools(
                        &window,
                        &toolbar,
                        &tabs,
                        current,
                        &mut devtools_open,
                        ww,
                        wh,
                        #[cfg(target_os = "windows")]
                        Some(&devtools_panel),
                    );
                }

                _ if msg.starts_with("load:") => {
                    if let Some(rest) = msg.strip_prefix("load:") {
                        if let Some(url) = resolve_omnibox_input(rest) {
                            tabs[current].url = url.clone();
                            tabs[current].title.clear();
                            let _ = tabs[current].webview.load_url(&url);
                            #[cfg(not(target_os = "windows"))]
                            let _ = tabs[current].webview.focus();
                            sync_toolbar(&toolbar, &tabs, current);
                            raise_toolbar(
                                &toolbar,
                                &window,
                                Some(&tabs),
                                #[cfg(target_os = "windows")]
                                Some(&devtools_panel),
                            );
                        }
                    }
                }

                _ if msg.starts_with("new_tab:") => {
                    open_new_tab(
                        &window,
                        &proxy,
                        &mut tabs,
                        &mut current,
                        &mut next_id,
                        &toolbar,
                        NEWTAB_URL,
                        ww,
                        wh,
                        devtools_open,
                        #[cfg(target_os = "windows")]
                        Some(&devtools_panel),
                    );
                }

                _ if msg.starts_with("switch_tab:") => {
                    if let Some(rest) = msg.strip_prefix("switch_tab:") {
                        if let Ok(idx) = rest.trim().parse::<usize>() {
                            if idx < tabs.len() {
                                current = idx;
                                show_tab(
                                    &window,
                                    &tabs,
                                    current,
                                    &toolbar,
                                    devtools_open,
                                    ww,
                                    wh,
                                    #[cfg(target_os = "windows")]
                                    Some(&devtools_panel),
                                );
                                sync_toolbar(&toolbar, &tabs, current);
                            }
                        }
                    }
                }

                _ if msg.starts_with("close_tab:") => {
                    if let Some(rest) = msg.strip_prefix("close_tab:") {
                        if let Ok(idx) = rest.trim().parse::<usize>() {
                            if idx < tabs.len() {
                                tabs.remove(idx);
                                if tabs.is_empty() {
                                    open_new_tab(
                                        &window,
                                        &proxy,
                                        &mut tabs,
                                        &mut current,
                                        &mut next_id,
                                        &toolbar,
                                        NEWTAB_URL,
                                        ww,
                                        wh,
                                        devtools_open,
                                        #[cfg(target_os = "windows")]
                                        Some(&devtools_panel),
                                    );
                                } else {
                                    if idx < current {
                                        current -= 1;
                                    } else {
                                        current = current.min(tabs.len() - 1);
                                    }
                                    show_tab(
                                        &window,
                                        &tabs,
                                        current,
                                        &toolbar,
                                        devtools_open,
                                        ww,
                                        wh,
                                        #[cfg(target_os = "windows")]
                                        Some(&devtools_panel),
                                    );
                                    sync_toolbar(&toolbar, &tabs, current);
                                }
                            }
                        }
                    }
                }

                _ => {}
                }
            },

            #[cfg(target_os = "windows")]
            Event::MainEventsCleared => {
                if z_order_nudges < 60 {
                    raise_toolbar(
                        &toolbar,
                        &window,
                        Some(&tabs),
                        Some(&devtools_panel),
                    );
                    if z_order_nudges % 5 == 0 {
                        sync_toolbar(&toolbar, &tabs, current);
                    }
                    z_order_nudges += 1;
                }
            }

            _ => {}
        }
    });
}


#[cfg(target_os = "windows")]
mod win_devtools {
    use serde_json::Value;
    use wry::WebView;

    const CDP_BASE: &str = "http://127.0.0.1:9222";

    pub fn open_in_panel(panel: &WebView, page_url: &str) {
        for _ in 0..50 {
            if let Some(url) = inspector_url_for_page(page_url) {
                let _ = panel.set_visible(true);
                let _ = panel.load_url(&url);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = panel.set_visible(true);
    }

    pub fn close_panel(panel: &WebView) {
        let _ = panel.set_visible(false);
        let _ = panel.load_url("about:blank");
    }

    fn inspector_url_for_page(page_url: &str) -> Option<String> {
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
        for endpoint in ["/json/list", "/json"] {
            if let Ok(resp) = ureq::get(&format!("{CDP_BASE}{endpoint}")).call() {
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
                    .is_some_and(|u| urls_match(u, page_url))
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
