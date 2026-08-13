//! Standalone mock WebSocket server speaking the palws protocol.
//!
//! Lets you exercise the frontend (dx serve) without a running game:
//!   cargo run --example mock_server -p palws
//! then open the web app and click 刷新 in the footer status bar.
//!
//! It implements the exact server -> client envelope and answers
//! `snapshot.request` with a fake full snapshot + status sequence.

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};

const PORT: u16 = 32123;
static SEQ: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn make_event(mtype: &str, request_id: Option<&str>, payload: serde_json::Value) -> String {
    let seq = SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    let mut obj = serde_json::Map::new();
    obj.insert("protocol".into(), "palws".into());
    obj.insert("version".into(), 1.into());
    obj.insert("type".into(), mtype.into());
    obj.insert("id".into(), format!("srv-{seq}").into());
    if let Some(rid) = request_id {
        obj.insert("request_id".into(), rid.into());
    }
    obj.insert("seq".into(), seq.into());
    obj.insert("timestamp_ms".into(), now_ms().into());
    obj.insert("payload".into(), payload);
    serde_json::Value::Object(obj).to_string()
}

fn hello() -> String {
    make_event(
        "server.hello",
        None,
        serde_json::json!({
            "server_version": "palws-mock",
            "capabilities": ["snapshot", "snapshot.request", "log", "heartbeat"],
            "clients": 1,
            "sync_state": "idle",
        }),
    )
}

fn status(phase: &str, request_id: &str, requested: u32, total: u32) -> String {
    make_event(
        "sync.status",
        Some(request_id),
        serde_json::json!({
            "phase": phase,
            "requested_pages": requested,
            "total_pages": total,
            "trigger": "web",
        }),
    )
}

fn snapshot(request_id: &str) -> String {
    make_event(
        "snapshot",
        Some(request_id),
        serde_json::json!({
            "mode": "replace",
            "pals": [
                {"species":"BadCatgirl","gender":"female","passives":["WorldTree_ATK"],"nickname":"测试猫","level":30,"favorite":2,"lucky":false,"group":"party"},
                {"species":"BlueberryFairy","gender":"male","passives":["CraftSpeed_up3"],"nickname":null,"level":41,"favorite":0,"lucky":true,"group":"box"},
                {"species":"BrownRabbit","gender":"female","passives":[],"nickname":"兔兔","level":12,"favorite":0,"lucky":false,"group":"box"}
            ],
            "stats": {"total": 3, "requested_pages": 32, "request_errors": 0, "containers": 2}
        }),
    )
}

fn pong(echo_id: &str) -> String {
    make_event("pong", None, serde_json::json!({ "echo_id": echo_id }))
}

async fn handle_ws(socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    if sink.send(Message::Text(hello())).await.is_err() {
        return;
    }
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                let v: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let mtype = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match mtype {
                    "ping" => {
                        let id = v.get("id").and_then(|i| i.as_str()).unwrap_or("ping");
                        if sink.send(Message::Text(pong(id))).await.is_err() {
                            break;
                        }
                    }
                    "snapshot.request" => {
                        let rid = v.get("id").and_then(|i| i.as_str()).unwrap_or("req-1");
                        let seq = [
                            status("queued", rid, 0, 32),
                            status("requesting", rid, 6, 32),
                            status("collecting", rid, 32, 32),
                            status("broadcasting", rid, 32, 32),
                        ];
                        for s in seq {
                            if sink.send(Message::Text(s)).await.is_err() {
                                return;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                        }
                        if sink.send(Message::Text(snapshot(rid))).await.is_err() {
                            return;
                        }
                        if sink
                            .send(Message::Text(status("complete", rid, 32, 32)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
}

#[tokio::main]
async fn main() {
    let app = Router::new().route("/ws", get(
        |ws: WebSocketUpgrade| async move { ws.on_upgrade(handle_ws) },
    ));
    let addr = SocketAddr::from(([127, 0, 0, 1], PORT));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("mock palws server on ws://127.0.0.1:{PORT}/ws");
    axum::serve(listener, app).await.unwrap();
}
