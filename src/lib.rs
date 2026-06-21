use chrono::Local;
use futures_util::future::{select, Either};
use futures_util::StreamExt;
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use worker::{
    ws_events::WebsocketEvent, Context, Request, Response, Result, Router, SecureTransport, Socket,
    WebSocket, WebSocketPair,
};
use worker::*;

const DEFAULT_SERVER: &str = "koukoku.shadan.open.ad.jp";
const DEFAULT_PORT: u16 = 992;

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

async fn stream_telnet(server: String, port: u16) -> Result<Response> {
    let socket = connect_telnets(&server, port).await?;
    let stream = futures_util::stream::try_unfold(socket, |mut socket| async move {
        let mut buffer = vec![0u8; 4096];
        match socket.read(&mut buffer).await {
            Ok(0) => Ok(None),
            Ok(n) => Ok(Some((buffer[..n].to_vec(), socket))),
            Err(error) => Err(worker::Error::RustError(error.to_string())),
        }
    });

    let headers = worker::Headers::new();
    headers.set("Content-Type", "text/plain; charset=utf-8")?;
    headers.set("X-Tsfrv-Server", &server)?;
    headers.set("X-Tsfrv-Port", &port.to_string())?;

    Ok(Response::from_stream(stream)?
        .with_headers(headers)
        .with_status(200))
}

async fn handle_websocket(req: Request, ctx: Context) -> Result<Response> {
    let url = req.url()?;
    let server = query_param(&url, "server", DEFAULT_SERVER);
    let port = query_port(&url, "port", DEFAULT_PORT);

    let pair = WebSocketPair::new()?;
    let client = pair.client;
    let server_ws = pair.server;
    server_ws.accept()?;

    let mut telnet = connect_telnets(&server, port).await?;

    ctx.wait_until(async move {
        if let Err(error) = bridge_websocket_to_telnet(server_ws, &mut telnet).await {
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

async fn bridge_websocket_to_telnet(ws: WebSocket, telnet: &mut Socket) -> Result<()> {
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
                let decoded = String::from_utf8_lossy(&chunk);
                if chunk.len() <= 8 {
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

fn io_error(error: io::Error) -> worker::Error {
    worker::Error::RustError(error.to_string())
}

fn help_html() -> String {
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
    <li><code>GET /stream</code> — 公告をテキストストリームで表示（デフォルト: {DEFAULT_SERVER}:{DEFAULT_PORT}）</li>
    <li><code>GET /stream?server=HOST&amp;port=PORT</code> — 任意の telnets サーバーへ接続</li>
    <li><code>GET /ws?server=HOST&amp;port=PORT</code> — WebSocket で双方向セッション</li>
  </ul>
  <h2>例</h2>
  <ul>
    <li><a href="/stream">/stream</a>（デフォルト公告）</li>
    <li><a href="/stream?server={DEFAULT_SERVER}&amp;port={DEFAULT_PORT}">/stream?server={DEFAULT_SERVER}&amp;port={DEFAULT_PORT}</a></li>
  </ul>
</body>
</html>"#
    )
}

#[event(fetch, respond_with_errors)]
pub async fn main(req: Request, env: worker::Env, ctx: Context) -> Result<Response> {
    if req.path() == "/ws" {
        if !is_websocket_upgrade(&req) {
            return Response::error(
                "WebSocket upgrade required. Connect with Upgrade: websocket",
                426,
            );
        }
        return handle_websocket(req, ctx).await;
    }

    let router = Router::new();

    router
        .get("/", |_req, _ctx| Response::from_html(help_html()))
        .get_async("/stream", |req, _ctx| async move {
            let url = req.url()?;
            let server = query_param(&url, "server", DEFAULT_SERVER);
            let port = query_port(&url, "port", DEFAULT_PORT);
            stream_telnet(server, port).await
        })
        .run(req, env)
        .await
}