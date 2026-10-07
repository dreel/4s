//! Minimal JSON-RPC-over-WebSocket client for the 4S daemon.

use anyhow::{Context, Result, anyhow, bail};
use fours_protocol::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::VecDeque;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub struct Client {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    events: VecDeque<EventEnvelope>,
}

impl Client {
    /// Connect and say hello. `user` owns this connection's undo history
    /// (None: the daemon host's user).
    pub async fn connect(url: &str, token: Option<String>, name: &str, user: Option<String>) -> Result<Client> {
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .with_context(|| format!("could not connect to 4sd at {url} (is it running?)"))?;
        let mut c = Client { ws, next_id: 1, events: VecDeque::new() };
        c.call(&Request::Hello(HelloParams {
            client_name: name.into(),
            protocol_version: PROTOCOL_VERSION,
            token,
            user,
        }))
        .await?;
        Ok(c)
    }

    pub async fn call(&mut self, req: &Request) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let wire = serde_json::to_value(req)?;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": req.method(), "params": wire["params"] });
        self.ws.send(Message::text(msg.to_string())).await?;
        loop {
            let v = self.read().await?;
            if v.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = v.get("error") {
                    let message = err.get("message").and_then(Value::as_str).unwrap_or("unknown error");
                    bail!("{}: {message}", req.method());
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            self.stash_event(v);
        }
    }

    pub async fn next_event(&mut self) -> Result<EventEnvelope> {
        loop {
            if let Some(e) = self.events.pop_front() {
                return Ok(e);
            }
            let v = self.read().await?;
            self.stash_event(v);
        }
    }

    fn stash_event(&mut self, v: Value) {
        if v.get("method").and_then(Value::as_str) == Some(EVENT_NOTIFICATION)
            && let Some(p) = v.get("params")
            && let Ok(env) = serde_json::from_value::<EventEnvelope>(p.clone())
        {
            self.events.push_back(env);
        }
    }

    async fn read(&mut self) -> Result<Value> {
        loop {
            match self.ws.next().await {
                Some(Ok(Message::Text(t))) => return Ok(serde_json::from_str(t.as_str())?),
                Some(Ok(Message::Close(_))) | None => return Err(anyhow!("connection closed by daemon")),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e.into()),
            }
        }
    }
}
