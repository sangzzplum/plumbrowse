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
use tao::platform::windows::WindowExtWindows;

#[cfg(target_os = "windows")]
use wry::WebViewBuilderExtWindows;

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
    } else {
        152.0
    }
}

/// На macOS каждый новый child WebView добавляется поверх предыдущих — поднимаем toolbar наверх.
#[cfg(target_os = "macos")]
fn raise_toolbar(toolbar: &WebView) {
    use objc2_app_kit::NSWindowOrderingMode;
    use wry::WebViewExtMacOS;

    let view = toolbar.webview();
    // SAFETY: superview is valid for an attached WKWebView child.
    if let Some(superview) = unsafe { view.superview() } {
        superview.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Above, None);
    }
}

#[cfg(not(target_os = "macos"))]
fn raise_toolbar(_toolbar: &WebView) {}

fn focus_active_tab(tabs: &[Tab], current: usize) {
    if let Some(tab) = tabs.get(current) {
        let _ = tab.webview.focus();
    }
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
    webview.open_devtools();
}

fn close_docked_devtools(webview: &WebView) {
    webview.close_devtools();
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
const TOOLBAR_URL: &str = "plum://toolbar/";
const DEVTOOLS_WIDTH: f64 = 420.0;
const TOOLBAR_BG: RGBA = (32, 33, 36, 255);

fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn version_label() -> String {
    format!("PlumBrowser v{}", app_version())
}

fn toolbar_navigation_allowed(url: &str) -> bool {
    url.starts_with("plum://toolbar")
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

fn normalize_url(input: &str) -> Option<String> {
    let u = input.trim();
    if u.is_empty() {
        return None;
    }
    if u.starts_with("http://")
        || u.starts_with("https://")
        || u.starts_with("file://")
        || u.starts_with("plum://")
    {
        Some(u.to_string())
    } else {
        Some(format!("https://{u}"))
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
        "window.__setState({}, {}, {}, {});",
        json!(titles),
        json!(urls),
        current,
        json!(cur_url)
    );
    let _ = toolbar.evaluate_script(&script);
}

fn plum_protocol(_id: WebViewId, req: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let uri = req.uri();
    let host = uri.host().unwrap_or_default();
    let path = uri.path();

    if host == "toolbar" || path.starts_with("/toolbar") {
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
      if (window.ipc && window.ipc.postMessage) window.ipc.postMessage(msg);
    }

    document.addEventListener('selectstart', (e) => {
      if (!e.target.closest('input')) e.preventDefault();
    });
    document.addEventListener('mousedown', (e) => {
      if (e.detail > 1 && !e.target.closest('input')) e.preventDefault();
    });

    window.addEventListener('DOMContentLoaded', () => {
      const drag = document.getElementById('drag');
      if (drag) drag.addEventListener('pointerdown', () => post('win_drag'));

      const min = document.getElementById('min');
      const max = document.getElementById('max');
      const close = document.getElementById('close');
      if (min) min.addEventListener('click', () => post('win_min'));
      if (max) max.addEventListener('click', () => post('win_max_toggle'));
      if (close) close.addEventListener('click', () => post('win_close'));

      document.getElementById('addtab').addEventListener('click', () => post('new_tab:'));

      document.getElementById('back').addEventListener('click', () => post('nav_back'));
      document.getElementById('forward').addEventListener('click', () => post('nav_forward'));
      document.getElementById('reload').addEventListener('click', () => post('nav_reload'));
      const devtoolsBtn = document.getElementById('devtools');
      if (devtoolsBtn) devtoolsBtn.addEventListener('click', () => post('nav_devtools'));

      const urlInput = document.getElementById('url');
      document.getElementById('go').addEventListener('click', () => post('load:' + urlInput.value));
      urlInput.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') post('load:' + e.target.value);
      });
      urlInput.addEventListener('blur', () => post('focus_content'));

      document.addEventListener('keydown', (e) => {
        if (e.key === 'F12') {
          e.preventDefault();
          post('nav_devtools');
        }
      }, true);

      post('ready');
    });

    window.__setState = function(tabTitles, tabUrls, current, url) {
      const strip = document.getElementById('tab-strip');
      strip.innerHTML = '';

      tabTitles.forEach((title, i) => {
        const t = document.createElement('div');
        t.className = 'tab' + (i === current ? ' active' : '');
        t.onclick = () => post('switch_tab:' + i);

        const tt = document.createElement('div');
        tt.className = 'tab-title';
        tt.textContent = title;

        const x = document.createElement('div');
        x.className = 'tab-close';
        x.textContent = '×';
        x.onclick = (e) => { e.stopPropagation(); post('close_tab:' + i); };

        t.appendChild(tt);
        t.appendChild(x);
        strip.appendChild(t);
      });

      document.getElementById('url').value = url || '';
    };

    window.__setTabTitle = function(index, title) {
      const tab = document.querySelectorAll('#tab-strip .tab')[index];
      if (tab) {
        const el = tab.querySelector('.tab-title');
        if (el) el.textContent = title;
      }
    };
"#;

const TAB_BAR_CSS: &str = r#"
    body { user-select:none; -webkit-user-select:none; }
    input { user-select:text; -webkit-user-select:text; }
    .tabs-bar { display:flex; align-items:center; gap:8px; min-height:32px; }
    .tab-strip {
      display:flex; gap:8px; align-items:center;
      flex:0 1 auto; min-width:0; max-width:100%;
      overflow-x:auto; overflow-y:hidden; scrollbar-width:none;
    }
    .tab-strip::-webkit-scrollbar { display:none; }
    .addtab {
      width:36px; height:32px; border-radius:12px; background:var(--b);
      display:grid; place-items:center; cursor:pointer; user-select:none;
      flex:0 0 auto; font-size:18px; line-height:1;
    }
    .addtab:hover { background:var(--b2); }
    .tab {
      display:flex; align-items:center; gap:8px;
      width:168px; min-width:72px; max-width:220px; height:32px;
      padding:0 10px; border-radius:12px; background:var(--b);
      cursor:pointer; user-select:none; flex:0 0 auto;
    }
    .tab.active { background:var(--b2); }
    .tab-title {
      min-width:0; overflow:hidden; white-space:nowrap;
      text-overflow:ellipsis; flex:1 1 auto;
    }
    .tab-close {
      flex:0 0 auto; width:26px; height:26px; border-radius:10px;
      display:grid; place-items:center; color:var(--mut); font-weight:900;
    }
    .tab-close:hover { background:#2a2b2f; color:var(--fg); }
    .drag-fill {
      flex:1 1 auto; min-width:32px; height:32px;
      -webkit-app-region:drag;
    }
    .version {
      flex:0 0 auto; font-size:11px; color:var(--mut);
      padding:0 8px; white-space:nowrap; user-select:none;
      -webkit-app-region:drag;
    }
"#;

fn toolbar_html() -> String {
    let toolbar_h = toolbar_height() as i32;
    let version = version_label();

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
    .tabs-wrap {{ padding:28px 12px 0; }}
    {tab_bar_css}
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
        <div class="addtab" id="addtab" title="Новая вкладка">+</div>
        <div class="drag-fill"></div>
        <div class="version">{version}</div>
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
            tab_bar_css = TAB_BAR_CSS,
            version = version
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
    .drag {{ flex:1; height:100%; display:flex; align-items:center; color:var(--mut); font-weight:700; -webkit-app-region:drag; }}
    .winbtns {{ display:flex; gap:8px; -webkit-app-region:no-drag; }}
    .wbtn {{ width:40px; height:26px; border-radius:10px; background:var(--b); display:grid; place-items:center; cursor:pointer; }}
    .wbtn:hover {{ background:var(--b2); }}
    .wbtn.close {{ background:var(--danger); }}
    .toolbar {{ flex:1; display:flex; flex-direction:column; gap:8px; padding:8px 12px 10px; -webkit-app-region:no-drag; }}
    {tab_bar_css}
    .addtab, .tab, .navbtn, input, .go {{ -webkit-app-region:no-drag; }}
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
        <div class="addtab" id="addtab" title="Новая вкладка">+</div>
      </div>
      <div class="row">
        <div class="navbtn" id="back" title="Назад">←</div>
        <div class="navbtn" id="forward" title="Вперёд">→</div>
        <div class="navbtn" id="reload" title="Обновить">↻</div>
        <div class="navbtn" id="devtools" title="Инструменты разработчика (F12)">&#123; &#125;</div>
        <input id="url" placeholder="example.com или https://example.com" autocomplete="off" spellcheck="false" />
        <div class="go" id="go">Go</div>
      </div>
    </div>
  </div>
  <script>{script}</script>
</body>
</html>"#,
        script = TOOLBAR_SCRIPT,
        tab_bar_css = TAB_BAR_CSS,
        version = version
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
        .with_focused(visible)
        .with_clipboard(true)
        .with_back_forward_navigation_gestures(true)
        .with_devtools(true)
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
    let builder = builder.with_browser_accelerator_keys(false);

    builder
        .build_as_child(window)
        .expect("failed to build content webview")
}

#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
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
            {
                win_devtools::close_floating_window();
                win_devtools::embed_after_open(bounds_devtools_panel(ww, wh), window.scale_factor());
                if let Some(panel) = devtools_panel {
                    let _ = panel.set_visible(true);
                }
            }
        }
    }
    focus_active_tab(tabs, current);
    raise_toolbar(toolbar);
}

#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
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
        win_devtools::register_panel_host(
            window.hwnd(),
            bounds_devtools_panel(ww, wh),
            window.scale_factor(),
        );
        if devtools_open {
            win_devtools::reposition_embedded(bounds_devtools_panel(ww, wh), window.scale_factor());
        }
    }
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
        win_devtools::close_floating_window();
        *devtools_open = false;
    } else {
        *devtools_open = true;
        open_docked_devtools(&tab.webview);
        #[cfg(target_os = "windows")]
        {
            if let Some(panel) = devtools_panel {
                let _ = panel.set_visible(true);
                let _ = panel.set_bounds(bounds_devtools_panel(ww, wh));
            }
            win_devtools::embed_after_open(bounds_devtools_panel(ww, wh), window.scale_factor());
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
    raise_toolbar(toolbar);
    focus_active_tab(tabs, current);
}

fn find_tab_idx(tabs: &[Tab], tab_id: u32) -> Option<usize> {
    tabs.iter().position(|t| t.id == tab_id)
}

fn build_toolbar(window: &Window, proxy: EventLoopProxy<UserEvent>, ww: f64) -> WebView {
    let builder = WebViewBuilder::new()
        .with_url(TOOLBAR_URL)
        .with_bounds(bounds_toolbar(ww))
        .with_background_color(TOOLBAR_BG)
        .with_focused(false)
        .with_accept_first_mouse(true)
        .with_devtools(false)
        .with_hotkeys_zoom(false)
        .with_back_forward_navigation_gestures(false)
        .with_custom_protocol("plum".to_string(), plum_protocol)
        .with_navigation_handler(|url| toolbar_navigation_allowed(&url))
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_initialization_script(TOOLBAR_LOCK_SCRIPT)
        .with_ipc_handler(move |req: Request<String>| {
            let _ = proxy.send_event(UserEvent::Ipc(req.body().clone()));
        });

    #[cfg(target_os = "windows")]
    let builder = builder
        .with_default_context_menus(false)
        .with_browser_accelerator_keys(false);

    builder
        .build_as_child(window)
        .expect("failed to build toolbar webview")
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
        .with_navigation_handler(|_| false)
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
        win_devtools::embed_after_open(bounds_devtools_panel(ww, wh), window.scale_factor());
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
    raise_toolbar(toolbar);
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

    #[cfg(not(target_os = "macos"))]
    {
        builder = builder.with_decorations(false);
    }

    builder
        .build(event_loop)
        .expect("failed to create window")
}


fn main() {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = build_window(&event_loop);
    set_dock_icon();

    let (mut ww, mut wh) = logical_size(&window);

    let mut next_id: u32 = 1;
    let mut devtools_open = false;
    let mut modifiers = ModifiersState::empty();

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

    let toolbar = build_toolbar(&window, proxy.clone(), ww);
    raise_toolbar(&toolbar);
    sync_toolbar(&toolbar, &tabs, current);

    #[cfg(target_os = "windows")]
    let devtools_panel = build_devtools_panel(&window, ww, wh);
    #[cfg(target_os = "windows")]
    win_devtools::register_panel_host(
        window.hwnd(),
        bounds_devtools_panel(ww, wh),
        window.scale_factor(),
    );

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
                    raise_toolbar(&toolbar);
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
                    raise_toolbar(&toolbar);
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

                WindowEvent::Focused(true) => focus_active_tab(&tabs, current),

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
                if let Some(url) = normalize_url(&url) {
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

            Event::UserEvent(UserEvent::Ipc(msg)) => match msg.as_str() {
                "win_drag" => {
                    let _ = window.drag_window();
                }
                "win_min" => window.set_minimized(true),
                "win_max_toggle" => {
                    let m = window.is_maximized();
                    window.set_maximized(!m);
                }
                "win_close" => *control_flow = ControlFlow::Exit,

                "ready" => sync_toolbar(&toolbar, &tabs, current),

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
                        if let Some(url) = normalize_url(rest) {
                            tabs[current].url = url.clone();
                            tabs[current].title.clear();
                            let _ = tabs[current].webview.load_url(&url);
                            let _ = tabs[current].webview.focus();
                            sync_toolbar(&toolbar, &tabs, current);
                            raise_toolbar(&toolbar);
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
            },

            _ => {}
        }
    });
}

#[cfg(target_os = "windows")]
mod win_devtools {
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::thread;
    use std::time::Duration;

    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, WPARAM};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumChildWindows, EnumWindows, GetClassNameW, GetWindowRect, GetWindowTextLengthW,
        GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, MoveWindow, PostMessageW,
        SetParent, ShowWindow, GWL_STYLE, SW_SHOW, WM_CLOSE, WS_CHILD, WS_POPUP,
    };
    use wry::dpi::{PhysicalPosition, PhysicalSize, Rect};

    static EMBEDDED_HWND: AtomicIsize = AtomicIsize::new(0);
    static PANEL_HOST_HWND: AtomicIsize = AtomicIsize::new(0);

    pub fn register_panel_host(main_hwnd: isize, panel: Rect, scale: f64) {
        let target = rect_to_physical(panel, scale);
        if let Some(hwnd) = find_child_near(main_hwnd, &target) {
            PANEL_HOST_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
        }
    }

    pub fn embed_after_open(panel: Rect, scale: f64) {
        let parent_hwnd = PANEL_HOST_HWND.load(Ordering::SeqCst);
        let local = local_panel_rect(panel, scale);
        thread::spawn(move || {
            for _ in 0..80 {
                thread::sleep(Duration::from_millis(50));
                if let Some(devtools) = find_devtools_window() {
                    let parent = hwnd_from_isize(parent_hwnd);
                    embed_window(devtools, parent, &local);
                    EMBEDDED_HWND.store(devtools.0 as isize, Ordering::SeqCst);
                    return;
                }
            }
        });
    }

    pub fn reposition_embedded(panel: Rect, scale: f64) {
        let hwnd = EMBEDDED_HWND.load(Ordering::SeqCst);
        if hwnd == 0 {
            return;
        }
        let devtools = HWND(hwnd as _);
        if !unsafe { IsWindowVisible(devtools).as_bool() } {
            return;
        }
        embed_window(devtools, panel_host(), &local_panel_rect(panel, scale));
    }

    pub fn close_floating_window() {
        let hwnd = EMBEDDED_HWND.load(Ordering::SeqCst);
        if hwnd != 0 {
            unsafe {
                let _ = PostMessageW(HWND(hwnd as _), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            EMBEDDED_HWND.store(0, Ordering::SeqCst);
            return;
        }
        if let Some(devtools) = find_devtools_window() {
            unsafe {
                let _ = PostMessageW(devtools, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn hwnd_from_isize(hwnd: isize) -> HWND {
        if hwnd != 0 {
            HWND(hwnd as _)
        } else {
            HWND::default()
        }
    }

    fn panel_host() -> HWND {
        hwnd_from_isize(PANEL_HOST_HWND.load(Ordering::SeqCst))
    }

    fn local_panel_rect(panel: Rect, scale: f64) -> RECT {
        let size: PhysicalSize<f64> = panel.size.to_physical(scale);
        RECT {
            left: 0,
            top: 0,
            right: size.width.round() as i32,
            bottom: size.height.round() as i32,
        }
    }

    fn rect_to_physical(panel: Rect, scale: f64) -> RECT {
        let pos: PhysicalPosition<f64> = panel.position.to_physical(scale);
        let size: PhysicalSize<f64> = panel.size.to_physical(scale);
        RECT {
            left: pos.x.round() as i32,
            top: pos.y.round() as i32,
            right: (pos.x + size.width).round() as i32,
            bottom: (pos.y + size.height).round() as i32,
        }
    }

    fn find_child_near(main_hwnd: isize, target: &RECT) -> Option<HWND> {
        let mut ctx = FindChildCtx {
            target: *target,
            best: HWND::default(),
            best_dist: i64::MAX,
        };
        unsafe {
            let _ = EnumChildWindows(
                HWND(main_hwnd as _),
                Some(enum_child),
                LPARAM(&mut ctx as *mut _ as isize),
            );
        }
        if ctx.best.0.is_null() {
            None
        } else {
            Some(ctx.best)
        }
    }

    struct FindChildCtx {
        target: RECT,
        best: HWND,
        best_dist: i64,
    }

    unsafe extern "system" fn enum_child(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut FindChildCtx);
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return true.into();
        }
        let dist = (rect.left - ctx.target.left).abs() as i64
            + (rect.top - ctx.target.top).abs() as i64
            + (rect.right - ctx.target.right).abs() as i64
            + (rect.bottom - ctx.target.bottom).abs() as i64;
        if dist < ctx.best_dist {
            ctx.best = hwnd;
            ctx.best_dist = dist;
        }
        true.into()
    }

    fn embed_window(devtools: HWND, parent: HWND, local: &RECT) {
        if parent.0.is_null() {
            return;
        }
        unsafe {
            let _ = SetParent(devtools, parent);
            let style =
                windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(devtools, GWL_STYLE)
                    as u32;
            let child_style = (style & !WS_POPUP.0) | WS_CHILD.0;
            windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
                devtools,
                GWL_STYLE,
                child_style as isize,
            );
            let w = local.right - local.left;
            let h = local.bottom - local.top;
            let _ = MoveWindow(devtools, local.left, local.top, w, h, true);
            let _ = ShowWindow(devtools, SW_SHOW);
        }
    }

    fn find_devtools_window() -> Option<HWND> {
        let pid = unsafe { GetCurrentProcessId() };
        unsafe {
            let mut found = HWND::default();
            let mut ctx = FindDevtoolsCtx {
                pid,
                found: &mut found,
            };
            let _ = EnumWindows(Some(enum_devtools), LPARAM(&mut ctx as *mut _ as isize));
            if found.0.is_null() {
                None
            } else {
                Some(found)
            }
        }
    }

    struct FindDevtoolsCtx<'a> {
        pid: u32,
        found: &'a mut HWND,
    }

    unsafe extern "system" fn enum_devtools(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut FindDevtoolsCtx<'_>);
        if !IsWindowVisible(hwnd).as_bool() {
            return true.into();
        }

        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != ctx.pid {
            return true.into();
        }

        let mut class_buf = [0u16; 256];
        let class_len = GetClassNameW(hwnd, &mut class_buf);
        if class_len > 0 {
            let class_name = String::from_utf16_lossy(&class_buf[..class_len as usize]);
            if class_name.contains("Chrome_WidgetWin") {
                *ctx.found = hwnd;
                return false.into();
            }
        }

        let len = GetWindowTextLengthW(hwnd);
        if len > 0 {
            let mut buf = vec![0u16; (len + 1) as usize];
            let read = GetWindowTextW(hwnd, &mut buf);
            if read > 0 {
                let title = String::from_utf16_lossy(&buf[..read as usize]);
                if title.contains("DevTools") {
                    *ctx.found = hwnd;
                    return false.into();
                }
            }
        }

        true.into()
    }
}
