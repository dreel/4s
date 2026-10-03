//! JSON-RPC 2.0 over WebSocket. One task per connection; requests are parsed
//! into the typed `Request` enum from `fours-protocol` and dispatched to the
//! core. Events are pushed as `event` notifications to subscribed connections.

use crate::core::{RpcError, Shared, error_json};
use fours_engine::offline;
use fours_protocol::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

pub async fn serve(core: Shared, listener: TcpListener, token: Option<String>) {
    let ids = Arc::new(AtomicU64::new(1));
    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!("accept failed: {e}");
                continue;
            }
        };
        let id = ids.fetch_add(1, Ordering::Relaxed);
        tracing::debug!("connection {id} from {addr}");
        tokio::spawn(handle_connection(core.clone(), stream, id, token.clone()));
    }
}

struct Conn {
    id: u64,
    name: String,
    authed: bool,
    subscription: Option<JoinHandle<()>>,
}

async fn handle_connection(core: Shared, stream: TcpStream, id: u64, token: Option<String>) {
    let ws = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            tracing::debug!("websocket handshake failed: {e}");
            return;
        }
    };
    let (mut sink, mut source) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink.send(Message::text(m)).await.is_err() {
                break;
            }
        }
    });

    let mut conn = Conn { id, name: format!("client-{id}"), authed: token.is_none(), subscription: None };
    while let Some(msg) = source.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                if let Some(resp) = handle_text(&core, &mut conn, &tx, token.as_deref(), text.as_str()).await {
                    let _ = tx.send(resp);
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }
    if let Some(h) = conn.subscription.take() {
        h.abort();
    }
    drop(tx);
    let _ = writer.await;
    tracing::debug!("connection {id} closed");
}

fn response(id: &Value, result: Result<Value, RpcError>) -> String {
    let body = match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": error_json(e.code, &e.message) }),
    };
    body.to_string()
}

async fn handle_text(
    core: &Shared,
    conn: &mut Conn,
    tx: &mpsc::UnboundedSender<String>,
    token: Option<&str>,
    text: &str,
) -> Option<String> {
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return Some(response(&Value::Null, Err(RpcError { code: -32700, message: e.to_string() }))),
    };
    let id = v.get("id").cloned();
    let reply = |r: Result<Value, RpcError>| id.as_ref().map(|id| response(id, r));
    let Some(method) = v.get("method").and_then(Value::as_str) else {
        return reply(Err(RpcError { code: -32600, message: "missing method".into() }));
    };
    let req = match parse_request(method, v.get("params").cloned()) {
        Ok(r) => r,
        Err(e) => {
            let code = if METHODS.contains(&method) { -32602 } else { -32601 };
            return reply(Err(RpcError { code, message: e }));
        }
    };
    if !conn.authed && !matches!(req, Request::Hello(_)) {
        return reply(Err(RpcError { code: -32001, message: "unauthorized: call session.hello with a token first".into() }));
    }

    let result = match req {
        Request::Hello(p) => hello(conn, token, p),
        Request::EventsSubscribe(p) => Ok(subscribe(core, conn, tx, p)),
        Request::EventsUnsubscribe(_) => {
            if let Some(h) = conn.subscription.take() {
                h.abort();
            }
            Ok(json!({}))
        }
        Request::RenderOffline(p) => render(core, p).await,
        other => {
            let mut c = core.lock().unwrap();
            c.handle(other, &conn.name)
        }
    };
    reply(result)
}

fn hello(conn: &mut Conn, token: Option<&str>, p: HelloParams) -> Result<Value, RpcError> {
    if p.protocol_version != PROTOCOL_VERSION {
        return Err(RpcError::failed(format!(
            "protocol version mismatch: client {} vs daemon {PROTOCOL_VERSION}",
            p.protocol_version
        )));
    }
    if let Some(t) = token
        && p.token.as_deref() != Some(t)
    {
        return Err(RpcError { code: -32001, message: "unauthorized: bad token".into() });
    }
    conn.authed = true;
    if !p.client_name.trim().is_empty() {
        conn.name = p.client_name.trim().to_string();
    }
    Ok(serde_json::to_value(HelloResult {
        protocol_version: PROTOCOL_VERSION,
        server_version: env!("CARGO_PKG_VERSION").into(),
        client_id: format!("{}#{}", conn.name, conn.id),
        role: Role::Engine,
    })
    .unwrap())
}

fn subscribe(core: &Shared, conn: &mut Conn, tx: &mpsc::UnboundedSender<String>, p: SubscribeParams) -> Value {
    if let Some(h) = conn.subscription.take() {
        h.abort();
    }
    let (mut rx, seq) = core.lock().unwrap().subscribe();
    let tx = tx.clone();
    let types = p.types;
    conn.subscription = Some(tokio::spawn(async move {
        loop {
            let env = match rx.recv().await {
                Ok(env) => env,
                Err(broadcast::error::RecvError::Lagged(n)) => Arc::new(EventEnvelope {
                    seq: 0,
                    origin: "engine".into(),
                    event: Event::Lagged { missed: n as u32 },
                }),
                Err(broadcast::error::RecvError::Closed) => break,
            };
            if let Some(types) = &types
                && !matches!(env.event, Event::Lagged { .. })
                && !types.iter().any(|t| t == env.event.type_name())
            {
                continue;
            }
            let msg = json!({ "jsonrpc": "2.0", "method": EVENT_NOTIFICATION, "params": &*env });
            if tx.send(msg.to_string()).is_err() {
                break;
            }
        }
    }));
    serde_json::to_value(SubscribeResult { seq }).unwrap()
}

async fn render(core: &Shared, p: RenderParams) -> Result<Value, RpcError> {
    let (values, pattern, path) = {
        let c = core.lock().unwrap();
        let path = match &p.path {
            Some(path) => c.resolve_data_path(path),
            None => {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                c.data_dir().join("renders").join(format!("render-{ts}.wav"))
            }
        };
        (c.engine_values(), c.pattern(), path)
    };
    let bars = p.bars.unwrap_or(1.0);
    if !(0.0..=64.0).contains(&bars) {
        return Err(RpcError::invalid("bars must be 0..64"));
    }
    let tail = p.tail.unwrap_or(0.0).clamp(0.0, 30.0);
    let sr = p.sample_rate.unwrap_or(48000).clamp(8000, 192000);
    tokio::task::spawn_blocking(move || {
        let r = offline::render_pattern(&values, &pattern, sr, bars, tail);
        let a = offline::analyze(&r.samples, sr);
        offline::write_wav(&path, &r.samples, sr).map_err(|e| RpcError::failed(format!("write wav: {e}")))?;
        let result = RenderResult {
            path: path.to_string_lossy().into_owned(),
            sample_rate: sr,
            duration: r.samples.len() as f64 / 2.0 / sr as f64,
            peak: a.peak,
            rms: a.rms,
            onsets: a.onsets,
            triggers: r.triggers,
        };
        Ok(serde_json::to_value(result).unwrap())
    })
    .await
    .map_err(|e| RpcError::failed(format!("render task failed: {e}")))?
}
