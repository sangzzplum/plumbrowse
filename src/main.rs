//! PlumBrowser — лёгкий кросс-платформенный браузер на Rust.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use serde_json::json;
use std::borrow::Cow;
use std::path::PathBuf;
use tao::{
    dpi::PhysicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    window::{Icon, Window, WindowBuilder},
};
use wry::{
    dpi::{LogicalPosition, LogicalSize},
    http::{header::CONTENT_TYPE, Request, Response},
    NewWindowResponse, PageLoadEvent, Rect, WebView, WebViewBuilder, WebViewId,
};

#[cfg(target_os = "macos")]
use tao::platform::macos::WindowBuilderExtMacOS;

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

fn bounds_content(win_w: f64, win_h: f64) -> Rect {
    let h = toolbar_height();
    Rect {
        position: LogicalPosition::new(0.0, h).into(),
        size: LogicalSize::new(win_w, (win_h - h).max(1.0)).into(),
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
    let path = req.uri().path();
    let (mime, body): (&str, Cow<'static, [u8]>) = match path {
        "/newtab" | "/newtab/" | "/" => (
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

      const urlInput = document.getElementById('url');
      document.getElementById('go').addEventListener('click', () => post('load:' + urlInput.value));
      urlInput.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') post('load:' + e.target.value);
      });
      urlInput.addEventListener('blur', () => post('focus_content'));

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
    html, body {{ margin:0; padding:0; width:100%; height:100%; overflow:hidden; }}
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
      </div>
    </div>
    <div class="nav-row">
      <div class="navbtn" id="back" title="Назад">←</div>
      <div class="navbtn" id="forward" title="Вперёд">→</div>
      <div class="navbtn" id="reload" title="Обновить">↻</div>
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
    html, body {{ margin:0; padding:0; width:100%; height:100%; overflow:hidden; }}
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
      <div class="drag" id="drag">PlumBrowser</div>
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
        <input id="url" placeholder="example.com или https://example.com" autocomplete="off" spellcheck="false" />
        <div class="go" id="go">Go</div>
      </div>
    </div>
  </div>
  <script>{script}</script>
</body>
</html>"#,
        script = TOOLBAR_SCRIPT,
        tab_bar_css = TAB_BAR_CSS
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
) -> WebView {
    WebViewBuilder::new()
        .with_url(url)
        .with_user_agent(content_user_agent())
        .with_bounds(bounds_content(ww, wh))
        .with_visible(visible)
        .with_focused(visible)
        .with_clipboard(true)
        .with_back_forward_navigation_gestures(true)
        .with_devtools(cfg!(debug_assertions))
        .with_custom_protocol("plum".to_string(), plum_protocol)
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
        })
        .build_as_child(window)
        .expect("failed to build content webview")
}

fn show_tab(tabs: &[Tab], current: usize, toolbar: &WebView) {
    for (i, tab) in tabs.iter().enumerate() {
        let visible = i == current;
        let _ = tab.webview.set_visible(visible);
    }
    focus_active_tab(tabs, current);
    raise_toolbar(toolbar);
}

fn resize_all(toolbar: &WebView, tabs: &[Tab], ww: f64, wh: f64) {
    let _ = toolbar.set_bounds(bounds_toolbar(ww));
    let bounds = bounds_content(ww, wh);
    for tab in tabs {
        let _ = tab.webview.set_bounds(bounds);
    }
}

fn find_tab_idx(tabs: &[Tab], tab_id: u32) -> Option<usize> {
    tabs.iter().position(|t| t.id == tab_id)
}

fn build_toolbar(window: &Window, proxy: EventLoopProxy<UserEvent>, ww: f64) -> WebView {
    WebViewBuilder::new()
        .with_html(toolbar_html())
        .with_bounds(bounds_toolbar(ww))
        .with_focused(false)
        .with_accept_first_mouse(true)
        .with_ipc_handler(move |req: Request<String>| {
            let _ = proxy.send_event(UserEvent::Ipc(req.body().clone()));
        })
        .build_as_child(window)
        .expect("failed to build toolbar webview")
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

    let webview = build_content_webview(window, proxy.clone(), tab_id, url, ww, wh, true);
    tabs.push(Tab {
        id: tab_id,
        url: url.to_string(),
        title,
        webview,
    });
    *current = tabs.len() - 1;
    sync_toolbar(toolbar, tabs, *current);
    focus_active_tab(tabs, *current);
    raise_toolbar(toolbar);
}

fn build_window(event_loop: &tao::event_loop::EventLoop<UserEvent>) -> Window {
    let mut builder = WindowBuilder::new()
        .with_title("PlumBrowser")
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
    let first_webview = build_content_webview(
        &window,
        proxy.clone(),
        next_id,
        NEWTAB_URL,
        ww,
        wh,
        true,
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

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => *control_flow = ControlFlow::Exit,

                WindowEvent::Resized(sz) => {
                    let scale = window.scale_factor();
                    (ww, wh) = logical_size_from_physical(sz, scale);
                    resize_all(&toolbar, &tabs, ww, wh);
                    raise_toolbar(&toolbar);
                }

                WindowEvent::ScaleFactorChanged {
                    scale_factor,
                    new_inner_size,
                } => {
                    (ww, wh) = logical_size_from_physical(*new_inner_size, scale_factor);
                    resize_all(&toolbar, &tabs, ww, wh);
                    raise_toolbar(&toolbar);
                }

                WindowEvent::Focused(true) => focus_active_tab(&tabs, current),

                _ => {}
            },

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
                    );
                }

                _ if msg.starts_with("switch_tab:") => {
                    if let Some(rest) = msg.strip_prefix("switch_tab:") {
                        if let Ok(idx) = rest.trim().parse::<usize>() {
                            if idx < tabs.len() {
                                current = idx;
                                show_tab(&tabs, current, &toolbar);
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
                                    );
                                } else {
                                    if idx < current {
                                        current -= 1;
                                    } else {
                                        current = current.min(tabs.len() - 1);
                                    }
                                    show_tab(&tabs, current, &toolbar);
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
