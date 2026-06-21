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
  <title>tsfrv view</title>
  <style>
    body { font-family: ui-monospace, monospace; margin: 0; background: #111; color: #eee; }
    header { padding: 0.75rem 1rem; background: #222; position: sticky; top: 0; }
    #status { color: #9fd; }
    pre { white-space: pre; overflow-x: auto; padding: 1rem; margin: 0; tab-size: 8; }
  </style>
</head>
<body>
  <header><span id="status">接続中...</span></header>
  <pre id="out"></pre>
  <script>
    const out = document.getElementById('out');
    const status = document.getElementById('status');
    let bytes = 0;
    fetch('/stream').then(async (res) => {
      if (!res.ok) throw new Error('HTTP ' + res.status);
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        bytes += value.length;
        out.textContent += decoder.decode(value, { stream: true });
        status.textContent = `受信中 (${bytes} bytes)`;
        window.scrollTo(0, document.body.scrollHeight);
      }
      status.textContent = `終了 (${bytes} bytes)`;
    }).catch((err) => {
      status.textContent = 'エラー: ' + err.message;
    });
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
    <li><a href="/view">/view</a>（ブラウザ向けストリームビューア）</li>
    <li><a href="/stream">/stream</a>（生テキスト）</li>
  </ul>
  <p>公告は配信間隔があるため、数十秒データが来ない時間があります。接続が切れたわけではありません。</p>
</body>
</html>"#
    )
}

fn html_headers() -> Result<worker::Headers> {
    let headers = worker::Headers::new();
    headers.set(
        "Content-Security-Policy",
        "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'",
    )?;
    headers.set("Referrer-Policy", "no-referrer")?;
    headers.set("X-Content-Type-Options", "nosniff")?;
    headers.set("X-Frame-Options", "DENY")?;
    Ok(headers)
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
        .get("/", |req, _ctx| {
            let mut url = req.url()?;
            url.set_path("/view");
            url.set_query(None);
            url.set_fragment(None);
            Ok(Response::redirect(url)?)
        })
        .get("/help", |_req, _ctx| {
            Ok(Response::from_html(help_html())?
                .with_headers(html_headers()?)
                .with_status(200))
        })
        .get("/view", |_req, _ctx| {
            let headers = html_headers()?;
            headers.set(
                "Content-Security-Policy",
                "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; form-action 'none'",
            )?;
            Ok(Response::from_html(view_html())?
                .with_headers(headers)
                .with_status(200))
        })
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