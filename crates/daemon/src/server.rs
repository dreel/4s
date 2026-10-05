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
use tokio::sync::{Notify, broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

/// Process-level context for connection handlers.
pub struct Daemon {
    pub info: Arc<DaemonInfo>,
    pub shutdown: Arc<Notify>,
    pub started: std::time::Instant,
}

pub async fn serve(core: Shared, listener: TcpListener, token: Option<String>, info: Arc<DaemonInfo>, shutdown: Arc<Notify>) {
    let ids = Arc::new(AtomicU64::new(1));
    let daemon = Arc::new(Daemon { info, shutdown, started: std::time::Instant::now() });
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
        tokio::spawn(handle_connection(core.clone(), daemon.clone(), stream, id, token.clone()));
    }
}

struct Conn {
    id: u64,
    name: String,
    authed: bool,
    subscription: Option<JoinHandle<()>>,
}

async fn handle_connection(core: Shared, daemon: Arc<Daemon>, stream: TcpStream, id: u64, token: Option<String>) {
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
                let (resp, then_shutdown) = handle_text(&core, &daemon, &mut conn, &tx, token.as_deref(), text.as_str()).await;
                if let Some(resp) = resp {
                    let _ = tx.send(resp);
                }
                if then_shutdown {
                    // Let the writer flush the response before the process exits.
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    daemon.shutdown.notify_one();
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

/// Returns the response (if the request had an id) and whether the daemon
/// should shut down after sending it.
async fn handle_text(
    core: &Shared,
    daemon: &Daemon,
    conn: &mut Conn,
    tx: &mpsc::UnboundedSender<String>,
    token: Option<&str>,
    text: &str,
) -> (Option<String>, bool) {
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return (Some(response(&Value::Null, Err(RpcError { code: -32700, message: e.to_string() }))), false),
    };
    let id = v.get("id").cloned();
    let reply = |r: Result<Value, RpcError>| id.as_ref().map(|id| response(id, r));
    let Some(method) = v.get("method").and_then(Value::as_str) else {
        return (reply(Err(RpcError { code: -32600, message: "missing method".into() })), false);
    };
    let req = match parse_request(method, v.get("params").cloned()) {
        Ok(r) => r,
        Err(e) => {
            let code = if METHODS.contains(&method) { -32602 } else { -32601 };
            return (reply(Err(RpcError { code, message: e })), false);
        }
    };
    if !conn.authed && !matches!(req, Request::Hello(_)) {
        let err = RpcError { code: -32001, message: "unauthorized: call session.hello with a token first".into() };
        return (reply(Err(err)), false);
    }
    let shutdown = matches!(req, Request::DaemonShutdown(_));

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
        Request::DaemonInfo(_) => {
            let mut info = (*daemon.info).clone();
            info.uptime = daemon.started.elapsed().as_secs_f64();
            Ok(serde_json::to_value(info).unwrap())
        }
        Request::DaemonShutdown(_) => {
            tracing::info!("shutdown requested by {}", conn.name);
            Ok(json!({}))
        }
        other => {
            let mut c = core.lock().unwrap();
            c.handle(other, &conn.name)
        }
    };
    (reply(result), shutdown)
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
    let (spec, path) = {
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
        (c.render_spec(), path)
    };
    let bars = p.bars.unwrap_or(1.0);
    if !(0.0..=64.0).contains(&bars) {
        return Err(RpcError::invalid("bars must be 0..64"));
    }
    let tail = p.tail.unwrap_or(0.0).clamp(0.0, 30.0);
    let sr = p.sample_rate.unwrap_or(48000).clamp(8000, 192000);
    tokio::task::spawn_blocking(move || {
        let r = offline::render_graph(&spec, sr, bars, tail);
        let a = offline::analyze(&r.samples, sr);
        let (lp, lr) = offline::lane_level(&r.samples, 0);
        let (rp, rr) = offline::lane_level(&r.samples, 1);
        offline::write_wav(&path, &r.samples, sr).map_err(|e| RpcError::failed(format!("write wav: {e}")))?;
        let result = RenderResult {
            path: path.to_string_lossy().into_owned(),
            sample_rate: sr,
            duration: r.samples.len() as f64 / 2.0 / sr as f64,
            peak: a.peak,
            rms: a.rms,
            left: Level { peak: lp, rms: lr },
            right: Level { peak: rp, rms: rr },
            onsets: a.onsets,
            triggers: r.triggers,
        };
        Ok(serde_json::to_value(result).unwrap())
    })
    .await
    .map_err(|e| RpcError::failed(format!("render task failed: {e}")))?
}
