//! palws: UE4SS Lua native module (Rust cdylib).
//!
//! Production v1 transport:
//!  * WebSocket + JSON protocol server on 127.0.0.1 (no static file hosting)
//!  * Lua -> Rust via `palws.broadcast(json)` (typed, versioned envelopes)
//!  * Rust -> Lua via a bounded command queue (`palws.take_command`)
//!  * Rust never touches UObjects and never calls back into Lua from network threads
//!
//! Hot-reload hardening:
//!  * every exported entry point is wrapped in catch_unwind; a panic becomes
//!    an error string pushed to Lua, never an abort
//!  * all one-time global state is idempotent (OnceLock::get checks)
//!  * no lua_State pointer is ever cached; every call uses the passed L

use mlua_sys::*;
use std::collections::VecDeque;
use std::ffi::{c_char, c_int};
use std::net::SocketAddr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;
use tokio::sync::broadcast;

// ---------------------------------------------------------------------------
// constants
// ---------------------------------------------------------------------------

const DEFAULT_PORT: i64 = 32123;
const PROTOCOL_NAME: &str = "palws";
const PROTOCOL_VERSION: u64 = 1;
/// Outbound snapshot upper bound (initial suggestion from the plan).
const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
/// Bounded command queue capacity.
const CMD_QUEUE_CAP: usize = 16;
/// Minimum interval between two accepted `snapshot.request` commands.
const SNAPSHOT_COOLDOWN: Duration = Duration::from_secs(15);

const SERVER_VERSION: &str = concat!("palws-", env!("CARGO_PKG_VERSION"));

static RT: OnceLock<Runtime> = OnceLock::new();
static STATE: OnceLock<Arc<AppState>> = OnceLock::new();
static LOAD_COUNT: AtomicUsize = AtomicUsize::new(0);

const ROOT_PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>palws</title></head>
<body style="font-family:sans-serif;background:#111;color:#eee;padding:2em">
<h1>palws is running</h1>
<p>WebSocket endpoint: <code>ws://127.0.0.1:32123/ws</code></p>
<p>Health: <code>http://127.0.0.1:32123/health</code></p>
</body></html>"#;

// ---------------------------------------------------------------------------
// app state
// ---------------------------------------------------------------------------

/// A command handed from a web client to Lua through the command pump.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CommandEnvelope {
    #[serde(rename = "type")]
    r#type: String,
    #[serde(default)]
    id: String,
}

struct AppState {
    /// Outbound server -> clients broadcast channel.
    outbound: broadcast::Sender<Arc<str>>,
    /// Last snapshot (replayed to newly connected clients).
    latest_snapshot: RwLock<Option<Arc<str>>>,
    /// Bounded queue of pending client commands (consumed by Lua).
    command_queue: Mutex<VecDeque<CommandEnvelope>>,
    /// Current connected WebSocket client count.
    clients: AtomicUsize,
    /// Monotonic event sequence (stamped server-side, used to ignore stale frames).
    event_seq: AtomicU64,
    /// Last known Lua sync phase (refreshed from `sync.status` broadcasts).
    sync_state: RwLock<String>,
    /// Last accepted `snapshot.request` time (cooldown).
    last_request: Mutex<Instant>,
}

impl AppState {
    fn new() -> Arc<Self> {
        let (outbound, _) = broadcast::channel::<Arc<str>>(256);
        Arc::new(Self {
            outbound,
            latest_snapshot: RwLock::new(None),
            command_queue: Mutex::new(VecDeque::new()),
            clients: AtomicUsize::new(0),
            event_seq: AtomicU64::new(0),
            sync_state: RwLock::new("idle".to_string()),
            last_request: Mutex::new(Instant::now() - SNAPSHOT_COOLDOWN),
        })
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Build a server-originated event envelope (stamps seq + timestamp).
fn make_event(
    state: &AppState,
    mtype: &str,
    request_id: Option<&str>,
    payload: serde_json::Value,
) -> String {
    let seq = state.event_seq.fetch_add(1, Ordering::SeqCst) + 1;
    let mut obj = serde_json::Map::new();
    obj.insert("protocol".into(), PROTOCOL_NAME.into());
    obj.insert("version".into(), PROTOCOL_VERSION.into());
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

fn error_event(
    state: &AppState,
    request_id: Option<&str>,
    code: &str,
    message: &str,
    retryable: bool,
) -> String {
    make_event(
        state,
        "error",
        request_id,
        serde_json::json!({
            "code": code,
            "message": message,
            "retryable": retryable,
        }),
    )
}

// ---------------------------------------------------------------------------
// ws/http server
// ---------------------------------------------------------------------------

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

fn build_hello(state: &AppState) -> String {
    make_event(
        state,
        "server.hello",
        None,
        serde_json::json!({
            "server_version": SERVER_VERSION,
            "capabilities": ["snapshot", "snapshot.request", "log", "heartbeat"],
            "clients": state.clients.load(Ordering::SeqCst),
            "sync_state": *state.sync_state.read().unwrap_or_else(|e| e.into_inner()),
        }),
    )
}

/// Try to enqueue a `snapshot.request` command. Returns a stable error code.
fn enqueue_snapshot_request(state: &AppState, id: &str) -> Result<(), &'static str> {
    let mut q = state
        .command_queue
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if q.iter().any(|c| c.r#type == "snapshot.request") {
        return Err("busy");
    }
    if q.len() >= CMD_QUEUE_CAP {
        return Err("queue_full");
    }
    let mut last = state
        .last_request
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if now.duration_since(*last) < SNAPSHOT_COOLDOWN {
        return Err("rate_limited");
    }
    *last = now;
    q.push_back(CommandEnvelope {
        r#type: "snapshot.request".to_string(),
        id: id.to_string(),
    });
    Ok(())
}

/// Handle one inbound text frame. Returns an optional direct reply to this client
/// (error / pong). Accepted commands are queued and produce no direct reply.
fn handle_client_message(state: &AppState, text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let obj = v.as_object()?;

    let protocol = obj.get("protocol").and_then(|p| p.as_str());
    let version = obj.get("version").and_then(|v| v.as_u64());
    let mtype = obj.get("type").and_then(|t| t.as_str())?;

    if protocol != Some(PROTOCOL_NAME) {
        return Some(error_event(
            state,
            None,
            "unsupported_protocol",
            "protocol must be 'palws'",
            false,
        ));
    }
    if version != Some(PROTOCOL_VERSION) {
        return Some(error_event(
            state,
            None,
            "unsupported_version",
            &format!("protocol version {version:?} not supported"),
            false,
        ));
    }

    match mtype {
        "client.hello" => None,
        "ping" => {
            let id = obj.get("id").and_then(|i| i.as_str()).unwrap_or("ping");
            Some(make_event(
                state,
                "pong",
                None,
                serde_json::json!({ "echo_id": id }),
            ))
        }
        "snapshot.request" => {
            let id = obj.get("id").and_then(|i| i.as_str()).unwrap_or("");
            match enqueue_snapshot_request(state, id) {
                Ok(()) => None,
                Err(code) => {
                    let (msg, retryable) = match code {
                        "busy" => ("已有同步任务进行中", true),
                        "rate_limited" => ("触发过于频繁，请稍后再试", true),
                        "queue_full" => ("命令队列已满", true),
                        _ => ("请求被拒绝", false),
                    };
                    Some(error_event(state, Some(id), code, msg, retryable))
                }
            }
        }
        _ => Some(error_event(
            state,
            None,
            "unsupported_command",
            &format!("unknown command '{mtype}'"),
            false,
        )),
    }
}

async fn handle_ws(socket: WebSocket, state: Arc<AppState>) {
    state.clients.fetch_add(1, Ordering::SeqCst);
    // Subscribe BEFORE reading the cached snapshot to avoid missing a broadcast
    // between the two steps. Duplicate snapshots are tolerated by the client via seq.
    let mut rx = state.outbound.subscribe();
    let (mut sink, mut stream) = socket.split();

    if sink.send(Message::Text(build_hello(&state))).await.is_err() {
        state.clients.fetch_sub(1, Ordering::SeqCst);
        return;
    }
    let snap: Option<Arc<str>> = state
        .latest_snapshot
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some(snap) = snap {
        if sink.send(Message::Text(snap.to_string())).await.is_err() {
            state.clients.fetch_sub(1, Ordering::SeqCst);
            return;
        }
    }

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(s) => {
                        if sink.send(Message::Text(s.to_string())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Text(t))) => {
                        if let Some(reply) = handle_client_message(&state, &t) {
                            if sink.send(Message::Text(reply)).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {} // binary / ping frames ignored
                    Some(Err(_)) => break,
                }
            }
        }
    }

    state.clients.fetch_sub(1, Ordering::SeqCst);
}

async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "clients": state.clients.load(Ordering::SeqCst),
    }))
}

async fn root_page() -> Html<&'static str> {
    Html(ROOT_PAGE)
}

async fn run_server(port: u16, state: Arc<AppState>) {
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health))
        .fallback(get(root_page))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(_e) => {
            // e.g. hot reload while an old server still owns the port: ignore, don't die
            return;
        }
    };
    let _ = axum::serve(listener, app).await;
}

// ---------------------------------------------------------------------------
// Lua-facing implementations (all wrapped by guarded! at the entry points)
// ---------------------------------------------------------------------------

unsafe fn start_server_impl(l: *mut lua_State) -> c_int {
    if RT.get().is_some() {
        push_lstring(l, "already running");
        return 1;
    }
    let port = luaL_optinteger(l, 1, DEFAULT_PORT);
    match Runtime::new() {
        Ok(rt) => {
            let state = STATE.get().cloned().unwrap_or_else(AppState::new);
            let _ = STATE.set(state.clone());
            rt.spawn(async move { run_server(port as u16, state).await });
            if RT.set(rt).is_err() {
                // reload race: another runtime already owns the server; drop this one
            }
            push_lstring(l, &format!("started on 127.0.0.1:{port}"));
        }
        Err(e) => push_lstring(l, &format!("runtime create failed: {e}")),
    }
    1
}

unsafe fn broadcast_impl(l: *mut lua_State) -> c_int {
    let mut len: usize = 0;
    let ptr = luaL_checklstring(l, 1, &mut len);
    let s = if ptr.is_null() {
        String::new()
    } else {
        String::from_utf8_lossy(std::slice::from_raw_parts(ptr as *const u8, len)).into_owned()
    };

    let state = match STATE.get() {
        Some(s) => s,
        None => {
            lua_pushboolean(l, 0);
            push_lstring(l, "server not started");
            return 2;
        }
    };

    // Parse + validate the protocol envelope.
    let mut value: serde_json::Value = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(e) => {
            lua_pushboolean(l, 0);
            push_lstring(l, &format!("invalid json: {e}"));
            return 2;
        }
    };
    let obj = match value.as_object_mut() {
        Some(o) => o,
        None => {
            lua_pushboolean(l, 0);
            push_lstring(l, "envelope must be a json object");
            return 2;
        }
    };

    let protocol = obj.get("protocol").and_then(|p| p.as_str());
    let version = obj.get("version").and_then(|v| v.as_u64());
    let mtype: String = obj.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
    if protocol != Some(PROTOCOL_NAME) || version != Some(PROTOCOL_VERSION) || mtype.is_empty() {
        lua_pushboolean(l, 0);
        push_lstring(l, "invalid envelope: require protocol/version/type");
        return 2;
    }

    // Reject oversized snapshots before broadcasting/caching.
    if mtype == "snapshot" && len > MAX_SNAPSHOT_BYTES {
        lua_pushboolean(l, 0);
        push_lstring(l, "snapshot too large");
        return 2;
    }

    // Stamp server-side seq + timestamp (single monotonic ordering for all frames).
    let seq = state.event_seq.fetch_add(1, Ordering::SeqCst) + 1;
    obj.insert("seq".into(), seq.into());
    obj.insert("timestamp_ms".into(), now_ms().into());

    if mtype == "snapshot" {
        let out: Arc<str> = Arc::from(value.to_string());
        *state
            .latest_snapshot
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(out.clone());
    } else if mtype == "sync.status" {
        if let Some(phase) = value
            .get("payload")
            .and_then(|p| p.get("phase"))
            .and_then(|p| p.as_str())
        {
            *state
                .sync_state
                .write()
                .unwrap_or_else(|e| e.into_inner()) = phase.to_string();
        }
    }

    let _ = state.outbound.send(Arc::from(value.to_string()));
    let clients = state.clients.load(Ordering::SeqCst);
    lua_pushboolean(l, 1);
    lua_pushinteger(l, clients as lua_Integer);
    2
}

unsafe fn take_command_impl(l: *mut lua_State) -> c_int {
    match STATE.get() {
        Some(state) => {
            let mut q = state
                .command_queue
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match q.pop_front() {
                Some(cmd) => {
                    let json = serde_json::to_string(&cmd).unwrap_or_default();
                    push_lstring(l, &json);
                }
                None => lua_pushnil(l),
            }
        }
        None => lua_pushnil(l),
    }
    1
}

unsafe fn client_count_impl(l: *mut lua_State) -> c_int {
    let n = STATE
        .get()
        .map(|s| s.clients.load(Ordering::SeqCst))
        .unwrap_or(0);
    lua_pushinteger(l, n as lua_Integer);
    1
}

unsafe fn version_impl(l: *mut lua_State) -> c_int {
    let v = lua_version(l);
    push_lstring(l, &format!("palws {SERVER_VERSION}, host lua core = {v:.0}"));
    1
}

// ---------------------------------------------------------------------------
// helpers / guards / exports
// ---------------------------------------------------------------------------

fn panic_payload(e: &(dyn std::any::Any + Send)) -> String {
    let s = e
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| e.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<unknown>".into());
    s.replace('\0', " ")
}

unsafe fn push_lstring(l: *mut lua_State, s: &str) {
    lua_pushlstring(l, s.as_ptr() as *const c_char, s.len());
}

/// Wrap a Lua C entry point: catch panics and push an error string instead of aborting.
macro_rules! guarded {
    ($l:ident, $name:literal, $body:block) => {{
        let r = catch_unwind(AssertUnwindSafe(move || -> c_int { $body }));
        match r {
            Ok(n) => n,
            Err(e) => {
                let msg = panic_payload(&*e);
                let fallback = format!("palws.{} panicked: {}", $name, msg);
                let rr = catch_unwind(AssertUnwindSafe(|| {
                    push_lstring($l, &fallback);
                    1
                }));
                rr.unwrap_or(0)
            }
        }
    }};
}

macro_rules! export_fn {
    ($fname:ident, $lname:literal, $implf:ident) => {
        unsafe extern "C-unwind" fn $fname(l: *mut lua_State) -> c_int {
            guarded!(l, $lname, { $implf(l) })
        }
    };
}

export_fn!(start_server, "start_server", start_server_impl);
export_fn!(broadcast_lua, "broadcast", broadcast_impl);
export_fn!(take_command_lua, "take_command", take_command_impl);
export_fn!(client_count, "client_count", client_count_impl);
export_fn!(version, "version", version_impl);

unsafe fn luaopen_impl(l: *mut lua_State) -> c_int {
    lua_createtable(l, 0, 8);
    let funcs: &[(&str, unsafe extern "C-unwind" fn(*mut lua_State) -> c_int)] = &[
        ("start_server", start_server),
        ("broadcast", broadcast_lua),
        ("take_command", take_command_lua),
        ("client_count", client_count),
        ("version", version),
    ];
    for (name, f) in funcs {
        push_lstring(l, name);
        lua_pushcclosure(l, *f, 0);
        lua_settable(l, -3);
    }
    push_lstring(l, "backend");
    push_lstring(l, "rust-cdylib-vendored-lua54+tokio+axum");
    lua_settable(l, -3);
    1
}

/// Pin this dll in the host process forever. UE4SS hot reload destroys the
/// mod's lua_State; Lua's package.loadlib handle then gets GC'd and the dll
/// would be FreeLibrary'd while our tokio worker threads still execute its
/// code -> crash on (or right after) the next require.
#[cfg(windows)]
unsafe fn pin_module() {
    extern "system" {
        fn GetModuleHandleExW(dwFlags: u32, lpModuleName: *const u16, phModule: *mut isize) -> i32;
    }
    const GET_MODULE_HANDLE_EX_FLAG_PIN: u32 = 0x00000001;
    const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x00000004;
    let mut h: isize = 0;
    let _ = GetModuleHandleExW(
        GET_MODULE_HANDLE_EX_FLAG_PIN | GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        luaopen_palws as *const u16,
        &mut h,
    );
}

#[cfg(not(windows))]
unsafe fn pin_module() {}

#[no_mangle]
pub extern "C" fn luaopen_palws(l: *mut lua_State) -> c_int {
    LOAD_COUNT.fetch_add(1, Ordering::SeqCst);
    unsafe { pin_module(); }
    let r = catch_unwind(AssertUnwindSafe(|| unsafe { luaopen_impl(l) }));
    match r {
        Ok(rc) => rc,
        Err(_) => 0,
    }
}
