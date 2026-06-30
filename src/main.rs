use serde_json::json;
use std::borrow::Cow;
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use wry::{
    dpi::{LogicalPosition, LogicalSize},
    http::{header::CONTENT_TYPE, Request, Response},
    Rect, WebView, WebViewBuilder, WebViewId,
};

const TOOLBAR_H: f64 = 120.0;
const TITLEBAR_H: f64 = 44.0;

// URL нашей страницы новой вкладки
const NEWTAB_URL: &str = "plum://newtab";

#[derive(Clone, Debug)]
struct Tab {
    url: String,
}

#[derive(Debug, Clone)]
enum UserEvent {
    Ipc(String),
}

fn normalize_url(input: &str) -> Option<String> {
    let u = input.trim();
    if u.is_empty() {
        return None;
    }
    if u.starts_with("http://") || u.starts_with("https://") || u.starts_with("file://") || u.starts_with("plum://") {
        Some(u.to_string())
    } else {
        Some(format!("https://{u}"))
    }
}

fn bounds_toolbar(win_w: f64) -> Rect {
    Rect {
        position: LogicalPosition::new(0.0, 0.0).into(),
        size: LogicalSize::new(win_w, TOOLBAR_H).into(),
    }
}

fn bounds_content(win_w: f64, win_h: f64) -> Rect {
    Rect {
        position: LogicalPosition::new(0.0, TOOLBAR_H).into(),
        size: LogicalSize::new(win_w, (win_h - TOOLBAR_H).max(1.0)).into(),
    }
}

fn tab_label(url: &str) -> String {
    // Для красоты: в табах показываем коротко.
    // Если не парсится — покажем как есть.
    url.replace("https://", "")
        .replace("http://", "")
        .replace("plum://localhost/", "")
}

fn sync_toolbar(toolbar: &WebView, tabs: &[Tab], current: usize) {
    let titles = tabs.iter().map(|t| tab_label(&t.url)).collect::<Vec<_>>();
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

const NEWTAB_HTML: &str = r#"<!doctype html>
<html lang="ru">
<head>
  <meta charset="utf-8"/>
  <meta name="viewport" content="width=device-width,initial-scale=1"/>
  <title>Новая вкладка</title>
  <style>
    :root { --bg:#0f1115; --fg:#e8eaed; --mut:#9aa0a6; --card:#171a21; --b:#2a2f3a; }
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
    <div class="hint">Новая вкладка (пустая страница).</div>
    <div class="card">Позже сюда можно добавить быстрые ссылки и поиск.</div>
  </div>
</body>
</html>
"#;

fn build_content(window: &tao::window::Window, ww: f64, wh: f64) -> WebView {
  WebViewBuilder::new()
    .with_url(NEWTAB_URL)
    .with_bounds(bounds_content(ww, wh))
    .with_custom_protocol("plum".to_string(), |_id: WebViewId, req: Request<Vec<u8>>| {
      let path = req.uri().path();

      // ВАЖНО: &b"Not found"[..] => &[u8], чтобы Cow был Cow<[u8]>, а не Cow<[u8; 9]>
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
    })
    .build_as_child(window)
    .expect("failed to build content webview")
}

fn build_toolbar(
    window: &tao::window::Window,
    proxy: tao::event_loop::EventLoopProxy<UserEvent>,
    ww: f64,
) -> WebView {
    // ВАЖНО: TOOLBAR_H в Rust должен соответствовать --toolbarH в CSS.
    let toolbar_html = format!(
        r#"<!doctype html>
<html lang="ru">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width,initial-scale=1" />
  <style>
    :root {{
      --bg:#202124; --fg:#e8eaed; --mut:#9aa0a6; --b:#303134; --b2:#3c4043; --danger:#5b2b2b;
      --toolbarH:{toolbar_h}px; --titlebarH:{titlebar_h}px;
    }}
    *{{ box-sizing:border-box; }}
    body{{ margin:0; background:var(--bg); color:var(--fg); font:14px/1.2 system-ui,"Segoe UI",Arial; overflow:hidden; }}
    .chrome{{ height:var(--toolbarH); display:flex; flex-direction:column; }}

    .titlebar{{
      height:var(--titlebarH);
      display:flex; align-items:center; gap:10px;
      padding:0 12px;
      border-bottom:1px solid #2b2c2f;
      user-select:none; -webkit-user-select:none;
    }}
    .drag{{ flex:1; height:100%; display:flex; align-items:center; color:var(--mut); font-weight:700; }}
    .winbtns{{ display:flex; gap:8px; }}
    .wbtn{{ width:40px; height:26px; border-radius:10px; background:var(--b); display:grid; place-items:center; cursor:pointer; }}
    .wbtn:hover{{ background:var(--b2); }}
    .wbtn.close{{ background:var(--danger); }}

    .toolbar{{ flex:1; display:flex; flex-direction:column; gap:10px; padding:10px 12px; }}

    .tabs{{ display:flex; gap:8px; align-items:center; overflow:hidden; }}
    .tabs-spacer{{ flex:1; }}

    .addtab{{
      width:36px; height:32px;
      border-radius:12px;
      background:var(--b);
      display:grid; place-items:center;
      cursor:pointer;
      user-select:none;
      flex:0 0 auto;
      font-size:18px;
      line-height:1;
    }}
    .addtab:hover{{ background:var(--b2); }}

    .tab{{
      display:flex; align-items:center; gap:8px;
      min-width:56px;
      height:32px;
      padding:0 10px;
      border-radius:12px;
      background:var(--b);
      cursor:pointer;
      user-select:none;

      flex:1 1 150px;
      max-width:340px;
    }}
    .tab.active{{ background:var(--b2); }}

    .tab-title{{
      min-width:0;
      overflow:hidden;
      white-space:nowrap;
      text-overflow:ellipsis;
      flex:1 1 auto;
    }}

    .tab-close{{
      flex:0 0 auto;
      width:26px; height:26px;
      border-radius:10px;
      display:grid; place-items:center;
      color:var(--mut);
      font-weight:900;
    }}
    .tab-close:hover{{ background:#2a2b2f; color:var(--fg); }}

    .row{{ display:flex; gap:10px; align-items:center; }}
    input{{ flex:1; min-width:260px; padding:12px 14px; border-radius:16px; border:1px solid #3c4043; outline:none; background:#111; color:var(--fg); }}
    .go{{ padding:12px 14px; border-radius:16px; background:var(--b); cursor:pointer; user-select:none; }}
    .go:hover{{ background:var(--b2); }}
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
      <div class="tabs" id="tabs"></div>
      <div class="row">
        <input id="url" placeholder="example.com или https://example.com" autocomplete="off" />
        <div class="go" id="go">Go</div>
      </div>
    </div>
  </div>

  <script>
    function post(msg) {{
      if (window.ipc && window.ipc.postMessage) window.ipc.postMessage(msg);
    }}

    window.addEventListener('DOMContentLoaded', () => {{
      document.getElementById('drag').addEventListener('pointerdown', () => post('win_drag'));
      document.getElementById('min').addEventListener('click', () => post('win_min'));
      document.getElementById('max').addEventListener('click', () => post('win_max_toggle'));
      document.getElementById('close').addEventListener('click', () => post('win_close'));

      document.getElementById('go').addEventListener('click', () => post('load:' + document.getElementById('url').value));
      document.getElementById('url').addEventListener('keydown', (e) => {{
        if (e.key === 'Enter') post('load:' + e.target.value);
      }});

      post('ready');
    }});

    window.__setState = function(tabTitles, tabUrls, current, url) {{
      const tabsEl = document.getElementById('tabs');
      tabsEl.innerHTML = '';

      tabTitles.forEach((title, i) => {{
        const t = document.createElement('div');
        t.className = 'tab' + (i === current ? ' active' : '');
        t.onclick = () => post('switch_tab:' + i);

        const tt = document.createElement('div');
        tt.className = 'tab-title';
        tt.textContent = title;

        const x = document.createElement('div');
        x.className = 'tab-close';
        x.textContent = '×';
        x.onclick = (e) => {{ e.stopPropagation(); post('close_tab:' + i); }};

        t.appendChild(tt);
        t.appendChild(x);
        tabsEl.appendChild(t);
      }});

      const spacer = document.createElement('div');
      spacer.className = 'tabs-spacer';
      tabsEl.appendChild(spacer);

      const add = document.createElement('div');
      add.className = 'addtab';
      add.textContent = '+';
      add.onclick = () => post('new_tab:');
      tabsEl.appendChild(add);

      document.getElementById('url').value = url || '';
    }};
  </script>
</body>
</html>
"#,
        toolbar_h = TOOLBAR_H as i32,
        titlebar_h = TITLEBAR_H as i32
    );

    WebViewBuilder::new()
        .with_html(toolbar_html)
        .with_bounds(bounds_toolbar(ww))
        .with_ipc_handler(move |req: Request<String>| {
            let _ = proxy.send_event(UserEvent::Ipc(req.body().clone()));
        })
        .build_as_child(window)
        .expect("failed to build toolbar webview")
}

fn main() {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title("PlumBrowser")
        .with_decorations(false)
        .with_inner_size(LogicalSize::new(1200.0, 800.0))
        .build(&event_loop)
        .expect("failed to create window");

    let mut tabs = vec![Tab {
        url: NEWTAB_URL.to_string(),
    }];
    let mut current: usize = 0;

    let size = window.inner_size();
    let mut ww = size.width as f64;
    let mut wh = size.height as f64;

    let toolbar = build_toolbar(&window, proxy.clone(), ww);
    let content = build_content(&window, ww, wh);

    sync_toolbar(&toolbar, &tabs, current);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => *control_flow = ControlFlow::Exit,

                WindowEvent::Resized(sz) => {
                    ww = sz.width as f64;
                    wh = sz.height as f64;
                    let _ = toolbar.set_bounds(bounds_toolbar(ww));
                    let _ = content.set_bounds(bounds_content(ww, wh));
                }

                _ => {}
            },

            Event::UserEvent(UserEvent::Ipc(msg)) => {
                // Окно
                match msg.as_str() {
                    "win_drag" => {
                        let _ = window.drag_window();
                        return;
                    }
                    "win_min" => {
                        window.set_minimized(true);
                        return;
                    }
                    "win_max_toggle" => {
                        let m = window.is_maximized();
                        window.set_maximized(!m);
                        return;
                    }
                    "win_close" => {
                        *control_flow = ControlFlow::Exit;
                        return;
                    }
                    _ => {}
                }

                // Браузер
                if msg == "ready" {
                    sync_toolbar(&toolbar, &tabs, current);
                    return;
                }

                if let Some(rest) = msg.strip_prefix("load:") {
                    if let Some(url) = normalize_url(rest) {
                        tabs[current].url = url.clone();
                        let _ = content.load_url(&url);
                        sync_toolbar(&toolbar, &tabs, current);
                    }
                } else if msg.starts_with("new_tab:") {
                    // всегда открываем нашу страницу новой вкладки
                    let url = NEWTAB_URL.to_string();
                    tabs.push(Tab { url: url.clone() });
                    current = tabs.len() - 1;
                    let _ = content.load_url(&url);
                    sync_toolbar(&toolbar, &tabs, current);
                } else if let Some(rest) = msg.strip_prefix("switch_tab:") {
                    if let Ok(idx) = rest.trim().parse::<usize>() {
                        if idx < tabs.len() {
                            current = idx;
                            let url = tabs[current].url.clone();
                            let _ = content.load_url(&url);
                            sync_toolbar(&toolbar, &tabs, current);
                        }
                    }
                } else if let Some(rest) = msg.strip_prefix("close_tab:") {
                    if let Ok(idx) = rest.trim().parse::<usize>() {
                        if idx < tabs.len() {
                            tabs.remove(idx);
                            if tabs.is_empty() {
                                tabs.push(Tab { url: NEWTAB_URL.to_string() });
                            }
                            current = current.min(tabs.len() - 1);
                            let url = tabs[current].url.clone();
                            let _ = content.load_url(&url);
                            sync_toolbar(&toolbar, &tabs, current);
                        }
                    }
                }
            }

            _ => {}
        }
    });
}
