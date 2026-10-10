//! JSON-RPC 2.0 over WebSocket. One task per connection; requests are parsed
//! into the typed `Request` enum from `fours-protocol` and dispatched to the
//! core. Events are pushed as `event` notifications to subscribed connections.

use crate::core::{RpcError, Shared, error_json};
use fours_engine::{offline, params};
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
        let local = addr.ip().to_canonical().is_loopback();
        tokio::spawn(handle_connection(core.clone(), daemon.clone(), stream, id, local, token.clone()));
    }
}

struct Conn {
    id: u64,
    name: String,
    /// Connected from the engine host itself.
    local: bool,
    /// Owner of this connection's undo history (None: the host user).
    user: Option<String>,
    authed: bool,
    subscription: Option<JoinHandle<()>>,
}

async fn handle_connection(
    core: Shared,
    daemon: Arc<Daemon>,
    stream: TcpStream,
    id: u64,
    local: bool,
    token: Option<String>,
) {
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

    let mut conn =
        Conn { id, name: format!("client-{id}"), local, user: None, authed: token.is_none(), subscription: None };
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
    // Notes this connection was holding end with it, and it leaves its seat.
    core.lock().unwrap().client_gone(&format!("conn:{}", conn.id));
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
        Request::Hello(p) => hello(core, conn, token, p),
        Request::EventsSubscribe(p) => Ok(subscribe(core, conn, tx, p)),
        Request::EventsUnsubscribe(_) => {
            if let Some(h) = conn.subscription.take() {
                h.abort();
            }
            Ok(json!({}))
        }
        Request::RenderOffline(p) => render(core, p, &format!("conn:{}", conn.id)).await,
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
            // Holder key: the connection id alone, which a renaming
            // `session.hello` cannot change.
            c.handle(other, &conn.name, &format!("conn:{}", conn.id), conn.user.as_deref())
        }
    };
    (reply(result), shutdown)
}

fn hello(core: &Shared, conn: &mut Conn, token: Option<&str>, p: HelloParams) -> Result<Value, RpcError> {
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
    conn.user = p.user.clone().map(|u| u.trim().to_string()).filter(|u| !u.is_empty());
    let client_id = format!("{}#{}", conn.name, conn.id);
    let (seat, choose_seat) = core.lock().unwrap().client_hello(
        &format!("conn:{}", conn.id),
        &client_id,
        p.user,
        p.seat,
        p.auto_seat.unwrap_or(true),
        conn.local,
        &conn.name,
    )?;
    Ok(serde_json::to_value(HelloResult {
        protocol_version: PROTOCOL_VERSION,
        server_version: env!("CARGO_PKG_VERSION").into(),
        client_id,
        role: Role::Engine,
        seat,
        choose_seat,
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

async fn render(core: &Shared, p: RenderParams, client: &str) -> Result<Value, RpcError> {
    let (mut spec, path, input) = {
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
        (c.render_spec(), path, c.render_input(&p, client)?)
    };
    // The click is heard only when asked for, whatever `metronome.on` is.
    spec.globals[params::METRONOME] = if p.metronome == Some(true) { 1.0 } else { 0.0 };
    if let Some(from) = p.from {
        if from >= MAX_SONG_TICKS {
            return Err(RpcError::invalid(format!("from must be under tick {MAX_SONG_TICKS}")));
        }
        spec.start = from;
    }
    let bars = p.bars.unwrap_or(1.0);
    if !(0.0..=64.0).contains(&bars) {
        return Err(RpcError::invalid("bars must be 0..64"));
    }
    let tail = p.tail.unwrap_or(0.0).clamp(0.0, 30.0);
    let sr = p.sample_rate.unwrap_or(48000).clamp(8000, 192000);
    tokio::task::spawn_blocking(move || {
        let r = offline::render_graph(&spec, sr, bars, tail, &input.notes);
        let recorded = input.record.map(|rec| rec.clips(&r.feedback)).unwrap_or_default();
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
            recorded,
        };
        Ok(serde_json::to_value(result).unwrap())
    })
    .await
    .map_err(|e| RpcError::failed(format!("render task failed: {e}")))?
}
