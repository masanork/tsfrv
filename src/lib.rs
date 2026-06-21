mod security;
mod telnet;

use chrono::Local;
use futures_util::future::{select, Either};
use futures_util::StreamExt;
use security::{default_target, resolve_target};
use telnet::{io_error, TelnetProcessor};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use worker::{
    ws_events::WebsocketEvent, Context, Env, Request, Response, Result, Router, SecureTransport,
    Socket, WebSocket, WebSocketPair,
};
use worker::*;

fn query_param(url: &worker::Url, key: &str, default: &str) -> String {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| default.to_string())
}

fn query_port(url: &worker::Url, key: &str, default: u16) -> u16 {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(default)
}

async fn connect_telnets(hostname: &str, port: u16) -> Result<Socket> {
    let socket = Socket::builder()
        .secure_transport(SecureTransport::On)
        .connect(hostname, port)?;
    socket.opened().await?;
    Ok(socket)
}

fn is_websocket_upgrade(req: &Request) -> bool {
    req.headers()
        .get("Upgrade")
        .ok()
        .flatten()
        .map(|value| value.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false)
}

fn stream_headers() -> Result<worker::Headers> {
    let headers = worker::Headers::new();
    headers.set("Content-Type", "text/plain; charset=utf-8")?;
    headers.set("Cache-Control", "no-cache, no-store, must-revalidate")?;
    headers.set("X-Content-Type-Options", "nosniff")?;
    headers.set("X-Accel-Buffering", "no")?;
    Ok(headers)
}

fn client_error(message: &str, status: u16) -> Result<Response> {
    Response::error(message, status)
}

async fn stream_telnet(env: &Env, server: String, port: u16) -> Result<Response> {
    let (server, port) = match resolve_target(env, &server, port) {
        Ok(target) => target,
        Err(_) => return client_error("target not allowed", 403),
    };
    let socket = connect_telnets(&server, port).await?;

    let stream = futures_util::stream::try_unfold(
        (socket, TelnetProcessor::new()),
        |(mut socket, mut processor)| async move {
            let mut buffer = vec![0u8; 4096];
            loop {
                match socket.read(&mut buffer).await {
                    Ok(0) => return Ok(None),
                    Ok(n) => {
                        let mut cleaned = Vec::new();
                        processor
                            .process_chunk(&mut socket, &buffer[..n], &mut cleaned)
                            .await?;

                        if !cleaned.is_empty() {
                            return Ok(Some((cleaned, (socket, processor))));
                        }
                    }
                    Err(error) => return Err(worker::Error::RustError(error.to_string())),
                }
            }
        },
    );

    Ok(Response::from_stream(stream)?
        .with_headers(stream_headers()?)
        .with_status(200))
}

async fn handle_websocket(req: Request, env: Env, ctx: Context) -> Result<Response> {
    let url = req.url()?;
    let (default_server, default_port) = default_target();
    let requested_server = query_param(&url, "server", &default_server);
    let requested_port = query_port(&url, "port", default_port);
    let (server, port) = match resolve_target(&env, &requested_server, requested_port) {
        Ok(target) => target,
        Err(_) => return client_error("target not allowed", 403),
    };

    let pair = WebSocketPair::new()?;
    let client = pair.client;
    let server_ws = pair.server;
    server_ws.accept()?;

    let mut telnet = connect_telnets(&server, port).await?;
    let mut processor = TelnetProcessor::new();

    ctx.wait_until(async move {
        if let Err(error) =
            bridge_websocket_to_telnet(server_ws, &mut telnet, &mut processor).await
        {
            console_error!("WebSocket bridge error: {error}");
        }
        let _ = telnet.close().await;
    });

    Response::from_websocket(client)
}

enum BridgeEvent {
    Ws(Option<Result<WebsocketEvent>>),
    Telnet(Result<Option<Vec<u8>>>),
}

async fn bridge_websocket_to_telnet(
    ws: WebSocket,
    telnet: &mut Socket,
    processor: &mut TelnetProcessor,
) -> Result<()> {
    let mut accumulated_koukoku = Vec::new();
    let mut accumulated_chat = Vec::new();
    let mut event_stream = ws.events()?;

    loop {
        let event = {
            let ws_next = event_stream.next();
            let telnet_read = read_telnet_chunk(telnet);
            futures_util::pin_mut!(ws_next);
            futures_util::pin_mut!(telnet_read);

            match select(ws_next, telnet_read).await {
                Either::Left((event, _)) => BridgeEvent::Ws(event),
                Either::Right((chunk, _)) => BridgeEvent::Telnet(chunk),
            }
        };

        match event {
            BridgeEvent::Ws(Some(Ok(WebsocketEvent::Message(message)))) => {
                if let Some(text) = message.text() {
                    telnet.write_all(text.as_bytes()).await.map_err(io_error)?;
                    if !text.ends_with('\n') {
                        telnet.write_all(b"\n").await.map_err(io_error)?;
                    }
                } else if let Some(bytes) = message.bytes() {
                    telnet.write_all(&bytes).await.map_err(io_error)?;
                }
            }
            BridgeEvent::Ws(Some(Ok(WebsocketEvent::Close(_)))) | BridgeEvent::Ws(None) => break,
            BridgeEvent::Ws(Some(Err(error))) => return Err(error),
            BridgeEvent::Telnet(Ok(Some(chunk))) => {
                let mut cleaned = Vec::new();
                processor
                    .process_chunk(telnet, &chunk, &mut cleaned)
                    .await?;

                if cleaned.is_empty() {
                    continue;
                }

                let decoded = String::from_utf8_lossy(&cleaned);
                if cleaned.len() <= 8 {
                    accumulated_koukoku.extend_from_slice(decoded.as_bytes());
                } else {
                    accumulated_chat.extend_from_slice(format!("{decoded}\n").as_bytes());
                }
                ws.send_with_str(&decoded)?;
            }
            BridgeEvent::Telnet(Ok(None)) => break,
            BridgeEvent::Telnet(Err(error)) => return Err(error),
        }
    }

    let timestamp = Local::now().format("%Y%m%d%H%M");
    console_log!(
        "Session ended at {timestamp}: koukoku={} bytes, chat={} bytes",
        accumulated_koukoku.len(),
        accumulated_chat.len()
    );

    Ok(())
}

async fn read_telnet_chunk(telnet: &mut Socket) -> Result<Option<Vec<u8>>> {
    let mut buffer = vec![0u8; 4096];
    match telnet.read(&mut buffer).await {
        Ok(0) => Ok(None),
        Ok(n) => Ok(Some(buffer[..n].to_vec())),
        Err(error) => Err(worker::Error::RustError(error.to_string())),
    }
}

fn view_html() -> String {
    r#"<!DOCTYPE html>
<html lang="ja">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>tsfrv view</title>
  <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/@xterm/xterm@5.5.0/css/xterm.min.css">
  <link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=DotGothic16&family=VT323&display=swap">
  <style>
    @font-face {
      font-family: 'DSEG7';
      src: url('https://cdn.jsdelivr.net/npm/dseg@0.46.0/fonts/DSEG7-Classic/DSEG7-Classic.woff2') format('woff2');
      font-weight: normal;
      font-style: normal;
    }
    :root {
      --phosphor: #39ff14;
      --phosphor-dim: #1a8f0a;
      --bezel: #1a1410;
      --panel: #0d120d;
    }
    body {
      margin: 0;
      background: #000;
      color: var(--phosphor-dim);
      font-family: 'VT323', monospace;
    }
    header {
      display: flex;
      gap: 1.25rem;
      align-items: center;
      flex-wrap: wrap;
      padding: 0.6rem 1rem;
      background: linear-gradient(180deg, #141a14 0%, var(--panel) 100%);
      border-bottom: 2px solid #2a1f14;
      box-shadow: inset 0 -1px 0 #000, 0 2px 8px #0008;
      position: sticky;
      top: 0;
      z-index: 2;
    }
    .brand {
      font-size: 1.4rem;
      color: var(--phosphor);
      letter-spacing: 0.15em;
      text-shadow: 0 0 6px #39ff1466;
    }
    .leds {
      display: flex;
      gap: 1rem;
      align-items: center;
    }
    .led-group {
      display: flex;
      align-items: center;
      gap: 0.4rem;
      font-size: 1.1rem;
      letter-spacing: 0.1em;
    }
    .led {
      width: 11px;
      height: 11px;
      border-radius: 50%;
      background: #1a1010;
      box-shadow: inset 0 1px 3px #000c;
      transition: background 0.05s, box-shadow 0.05s;
    }
    .led-tx.on {
      background: #ff2222;
      box-shadow: 0 0 6px #ff2222, 0 0 14px #ff222288, inset 0 0 4px #fff4;
    }
    .led-rx.on {
      background: #22ff44;
      box-shadow: 0 0 6px #22ff44, 0 0 14px #22ff4488, inset 0 0 4px #fff4;
    }
    .counter {
      display: flex;
      flex-direction: column;
      gap: 0.1rem;
    }
    .counter-label {
      font-size: 0.85rem;
      letter-spacing: 0.12em;
      color: #5a6a5a;
    }
    .seven-seg {
      font-family: 'DSEG7', monospace;
      font-size: 1.6rem;
      color: #ff4422;
      text-shadow: 0 0 6px #ff442288;
      letter-spacing: 0.08em;
      min-width: 7.5em;
    }
    #state {
      font-size: 1.15rem;
      color: var(--phosphor);
      text-shadow: 0 0 4px #39ff1444;
    }
    #hint {
      margin-left: auto;
      font-size: 0.95rem;
      color: #4a5a4a;
      max-width: 20rem;
    }
    #screen {
      position: relative;
      height: calc(100vh - 3.5rem);
      background: #020a02;
      overflow: hidden;
    }
    #screen::before {
      content: '';
      position: absolute;
      inset: 0;
      pointer-events: none;
      z-index: 1;
      background: repeating-linear-gradient(
        0deg,
        transparent,
        transparent 2px,
        #00000018 2px,
        #00000018 4px
      );
    }
    #screen::after {
      content: '';
      position: absolute;
      inset: 0;
      pointer-events: none;
      z-index: 1;
      background: radial-gradient(ellipse at center, transparent 55%, #00000088 100%);
    }
    #terminal {
      height: 100%;
      padding: 0.5rem;
      box-sizing: border-box;
    }
    .xterm { height: 100%; }
    .xterm-screen {
      text-shadow: 0 0 5px #39ff1466, 0 0 12px #39ff1422;
    }
    @media (max-width: 640px) {
      #hint { display: none; }
    }
  </style>
</head>
<body>
  <header>
    <span class="brand">TSFRV</span>
    <div class="leds">
      <div class="led-group">
        <span>TX</span>
        <span id="led-tx" class="led led-tx"></span>
      </div>
      <div class="led-group">
        <span>RX</span>
        <span id="led-rx" class="led led-rx"></span>
      </div>
    </div>
    <div class="counter">
      <span class="counter-label">RX BYTES</span>
      <span id="bytes" class="seven-seg">0000000</span>
    </div>
    <span id="state">CONNECT</span>
    <span id="hint">公告は配信間隔があり、数十秒止まることがあります</span>
  </header>
  <div id="screen">
    <div id="terminal"></div>
  </div>
  <script src="https://cdn.jsdelivr.net/npm/@xterm/xterm@5.5.0/lib/xterm.min.js"></script>
  <script>
    const ledTx = document.getElementById('led-tx');
    const ledRx = document.getElementById('led-rx');
    const bytesEl = document.getElementById('bytes');
    const stateEl = document.getElementById('state');

    const PHOSPHOR = '#39ff14';
    const term = new Terminal({
      disableStdin: true,
      cursorBlink: true,
      convertEol: true,
      scrollback: 5000,
      fontFamily: '"DotGothic16", "VT323", monospace',
      fontSize: 16,
      lineHeight: 1.1,
      theme: {
        background: '#020a02',
        foreground: PHOSPHOR,
        cursor: PHOSPHOR,
        cursorAccent: '#020a02',
        selectionBackground: '#39ff1433',
      },
    });
    term.open(document.getElementById('terminal'));

    let bytes = 0;
    let idleTimer = null;

    function pulseLed(el, duration) {
      el.classList.add('on');
      clearTimeout(el._pulse);
      el._pulse = setTimeout(() => el.classList.remove('on'), duration);
    }

    function setBytes(n) {
      bytesEl.textContent = String(n).padStart(7, '0');
    }

    function setState(text) {
      stateEl.textContent = text;
    }

    function resetIdleTimer() {
      clearTimeout(idleTimer);
      idleTimer = setTimeout(() => setState('WAIT'), 8000);
    }

    async function pump() {
      setState('CONNECT');
      setBytes(0);
      const decoder = new TextDecoder('utf-8');
      try {
        pulseLed(ledTx, 250);
        const res = await fetch('/stream', { cache: 'no-store' });
        if (!res.ok) throw new Error('HTTP ' + res.status);
        pulseLed(ledTx, 120);
        setState('ONLINE');
        const reader = res.body.getReader();
        while (true) {
          const { value, done } = await reader.read();
          if (done) break;
          bytes += value.length;
          pulseLed(ledRx, 90);
          setBytes(bytes);
          term.write(decoder.decode(value, { stream: true }));
          setState('RECV');
          resetIdleTimer();
        }
        term.write(decoder.decode());
        setState('CLOSED');
      } catch (err) {
        setState('ERROR');
        term.write('\r\n[ ' + err.message + ' ]\r\n');
      }
    }

    pump();
  </script>
</body>
</html>"#.to_string()
}

fn help_html() -> String {
    let (default_server, default_port) = default_target();
    format!(
        r#"<!DOCTYPE html>
<html lang="ja">
<head>
  <meta charset="utf-8">
  <title>tsfrv</title>
  <style>
    body {{ font-family: system-ui, sans-serif; max-width: 48rem; margin: 2rem auto; line-height: 1.6; }}
    code {{ background: #f4f4f4; padding: 0.1rem 0.3rem; border-radius: 0.2rem; }}
  </style>
</head>
<body>
  <h1>tsfrv</h1>
  <p>Telnet over SSL/TLS 電子公告ビューア (Cloudflare Workers / Rust WASM)</p>
  <h2>エンドポイント</h2>
  <ul>
    <li><code>GET /stream</code> — 公告をテキストストリームで表示（デフォルト: {default_server}:{default_port}）</li>
    <li><code>GET /stream?server=HOST&amp;port=PORT</code> — 許可された telnets サーバーへ接続</li>
    <li><code>GET /ws?server=HOST&amp;port=PORT</code> — WebSocket で双方向セッション</li>
  </ul>
  <h2>例</h2>
  <ul>
    <li><a href="/view">/view</a>（ブラウザ向けターミナルビューア・推奨）</li>
    <li><a href="/stream">/stream</a>（生テキスト）</li>
  </ul>
  <p>公告は配信間隔があるため、数十秒データが来ない時間があります。接続が切れたわけではありません。</p>
</body>
</html>"#
    )
}

fn html_headers(csp: &str) -> Result<worker::Headers> {
    let headers = worker::Headers::new();
    headers.set("Content-Type", "text/html; charset=utf-8")?;
    headers.set("Content-Security-Policy", csp)?;
    headers.set("Referrer-Policy", "no-referrer")?;
    headers.set("X-Content-Type-Options", "nosniff")?;
    headers.set("X-Frame-Options", "DENY")?;
    Ok(headers)
}

fn html_response(html: String, csp: &str) -> Result<Response> {
    Ok(Response::from_html(html)?
        .with_headers(html_headers(csp)?)
        .with_status(200))
}

#[event(fetch, respond_with_errors)]
pub async fn main(req: Request, env: Env, ctx: Context) -> Result<Response> {
    if req.path() == "/ws" {
        if !is_websocket_upgrade(&req) {
            return Response::error(
                "WebSocket upgrade required. Connect with Upgrade: websocket",
                426,
            );
        }
        return handle_websocket(req, env, ctx).await;
    }

    let router = Router::new();

    router
        .get("/", |_req, _ctx| Ok(html_response(
            help_html(),
            "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'",
        )?))
        .get("/help", |_req, _ctx| Ok(html_response(
            help_html(),
            "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'",
        )?))
        .get("/view", |_req, _ctx| Ok(html_response(
            view_html(),
            "default-src 'none'; script-src 'unsafe-inline' https://cdn.jsdelivr.net; style-src 'unsafe-inline' https://cdn.jsdelivr.net https://fonts.googleapis.com; font-src https://cdn.jsdelivr.net https://fonts.gstatic.com; connect-src 'self'; base-uri 'none'; form-action 'none'",
        )?))
        .get_async("/stream", |req, ctx| async move {
            let url = req.url()?;
            let (default_server, default_port) = default_target();
            let server = query_param(&url, "server", &default_server);
            let port = query_port(&url, "port", default_port);
            stream_telnet(&ctx.env, server, port).await
        })
        .run(req, env)
        .await
}