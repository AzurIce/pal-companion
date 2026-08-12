//! palws: UE4SS Lua native module (Rust cdylib).
//! WebSocket broadcast server + static HTTP file server on 127.0.0.1.
//!
//! Hot-reload hardening:
//!  * every exported entry point is wrapped in catch_unwind; a panic becomes
//!    a palws.log line + an error string pushed to Lua, never an abort
//!  * all one-time global state is idempotent (OnceLock::get checks)
//!  * no lua_State pointer is ever cached; every call uses the passed L
//!  * step logging with a load counter pinpoints where a reload dies

use mlua_sys::*;
use std::ffi::{c_int, CString};
use std::net::SocketAddr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::Html,
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use tokio::runtime::Runtime;
use tokio::sync::broadcast;
use tower_http::services::ServeDir;

static RT: OnceLock<Runtime> = OnceLock::new();
static TX: OnceLock<broadcast::Sender<String>> = OnceLock::new();
static CLIENTS: AtomicUsize = AtomicUsize::new(0);
static LOAD_COUNT: AtomicUsize = AtomicUsize::new(0);

const DEFAULT_PORT: i64 = 32123;
/// Static root for the companion web app. Override with env var PALWS_DIST.
const DEFAULT_DIST: &str = r"E:\pal-companion-ws-sync\dist";
/// Payload file for the file-transport path (Lua writes, Rust reads).
const PAYLOAD_PATH: &str = r"C:\Users\xiaob\palworld-dump\palws-payload.json";

const HINT_PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>palws</title></head>
<body style="font-family:sans-serif;background:#111;color:#eee;padding:2em">
<h1>palws is running</h1>
<p>WebSocket endpoint: <code>ws://127.0.0.1:32123/ws</code></p>
<p>Static root not found. Set env var <code>PALWS_DIST</code> (default:
<code>E:\pal-companion-ws-sync\dist</code>), then restart the game.</p>
</body></html>"#;

// ---------------------------------------------------------------------------
// logging + panic guard
// ---------------------------------------------------------------------------

fn log_line(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(r"C:\Users\xiaob\palworld-dump\palws.log")
    {
        let _ = writeln!(f, "{}", msg);
    }
}

fn panic_payload(e: &(dyn std::any::Any + Send)) -> String {
    let s = e
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| e.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<unknown>".into());
    s.replace('\0', " ")
}

unsafe fn push_str(l: *mut lua_State, s: &str) {
    lua_pushstring(l, CString::new(s).unwrap().as_ptr());
}

/// Wrap a Lua C entry point: catch panics, log them, push an error string
/// instead of aborting the host process.
macro_rules! guarded {
    ($l:ident, $name:literal, $body:block) => {{
        log_line(concat!("[step] ", $name, " enter"));
        let r = catch_unwind(AssertUnwindSafe(move || -> c_int { $body }));
        match r {
            Ok(n) => {
                log_line(concat!("[step] ", $name, " ok"));
                n
            }
            Err(e) => {
                let msg = panic_payload(&*e);
                log_line(&format!("[panic-guard] {} panicked: {}", $name, msg));
                let fallback = format!("palws.{} panicked: {}", $name, msg);
                let rr = catch_unwind(AssertUnwindSafe(|| {
                    push_str($l, &fallback);
                    1
                }));
                rr.unwrap_or(0)
            }
        }
    }};
}

// ---------------------------------------------------------------------------
// ws/http server
// ---------------------------------------------------------------------------

async fn ws_handler(ws: WebSocketUpgrade) -> impl axum::response::IntoResponse {
    ws.on_upgrade(handle_ws)
}

async fn handle_ws(socket: WebSocket) {
    let id = CLIENTS.fetch_add(1, Ordering::SeqCst) + 1;
    log_line(&format!("[ws] client #{id} connected"));
    let mut rx = match TX.get() {
        Some(tx) => tx.subscribe(),
        None => {
            CLIENTS.fetch_sub(1, Ordering::SeqCst);
            return;
        }
    };
    let (mut sink, mut stream) = socket.split();
    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(s) => {
                        log_line(&format!("[ws] -> client #{id}: sending {} bytes", s.len()));
                        if let Err(e) = sink.send(Message::Text(s)).await {
                            log_line(&format!("[ws] client #{id} send error: {e}"));
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(_) => {}
                }
            }
        }
    }
    CLIENTS.fetch_sub(1, Ordering::SeqCst);
    log_line(&format!("[ws] client #{id} disconnected"));
}

async fn hint() -> Html<&'static str> {
    Html(HINT_PAGE)
}

async fn run_server(port: u16) {
    let dist = std::env::var("PALWS_DIST").unwrap_or_else(|_| DEFAULT_DIST.to_string());
    let dist_exists = std::path::Path::new(&dist).is_dir();

    let app = if dist_exists {
        Router::new()
            .route("/ws", get(ws_handler))
            .fallback_service(ServeDir::new(&dist).append_index_html_on_directories(true))
    } else {
        Router::new()
            .route("/ws", get(ws_handler))
            .fallback(get(hint))
    };

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            // e.g. hot reload while an old server still owns the port: log, don't die
            log_line(&format!("[server] bind {addr} failed: {e}"));
            return;
        }
    };
    log_line(&format!("[server] listening on {addr} (dist exists: {dist_exists})"));
    let _ = axum::serve(listener, app).await;
}

// ---------------------------------------------------------------------------
// Lua-facing implementations (all wrapped by guarded! at the entry points)
// ---------------------------------------------------------------------------

unsafe fn start_server_impl(l: *mut lua_State) -> c_int {
    if RT.get().is_some() {
        log_line("[step] start_server: already running, idempotent ok");
        push_str(l, "already running");
        return 1;
    }
    let port = luaL_optinteger(l, 1, DEFAULT_PORT);
    match Runtime::new() {
        Ok(rt) => {
            let (tx, _) = broadcast::channel::<String>(256);
            if TX.set(tx).is_err() {
                log_line("[step] start_server: TX already set (reload race), reusing");
            }
            rt.spawn(async move { run_server(port as u16).await });
            if RT.set(rt).is_err() {
                log_line("[step] start_server: RT already set (reload race), dropping new runtime");
            }
            push_str(l, &format!("started on 127.0.0.1:{port}"));
        }
        Err(e) => push_str(l, &format!("runtime create failed: {e}")),
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
    log_line(&format!("[broadcast] lua->rust: {} bytes", s.len()));
    let sent = match TX.get() {
        Some(tx) if CLIENTS.load(Ordering::SeqCst) > 0 => tx.send(s).is_ok(),
        _ => false,
    };
    lua_pushboolean(l, if sent { 1 } else { 0 });
    1
}

unsafe fn notify_impl(l: *mut lua_State) -> c_int {
    let result = match std::fs::read(PAYLOAD_PATH) {
        Ok(bytes) => {
            let s = String::from_utf8_lossy(&bytes).into_owned();
            let nbytes = s.len();
            log_line(&format!("[notify] file->rust: {} bytes", nbytes));
            let sent = match TX.get() {
                Some(tx) if CLIENTS.load(Ordering::SeqCst) > 0 => tx.send(s).is_ok(),
                _ => false,
            };
            format!("read {} bytes, sent={}", nbytes, sent)
        }
        Err(e) => {
            log_line(&format!("[notify] read failed: {e}"));
            format!("read failed: {e}")
        }
    };
    push_str(l, &result);
    1
}

unsafe fn echo_impl(l: *mut lua_State) -> c_int {
    let t = lua_type(l, 1);
    let tn = lua_typename(l, t);
    let tn_str = if tn.is_null() {
        "?".to_string()
    } else {
        std::ffi::CStr::from_ptr(tn).to_string_lossy().into_owned()
    };
    let rawlen = lua_rawlen(l, 1);
    let mut slen: usize = 0;
    let p = lua_tolstring(l, 1, &mut slen);
    let preview = if p.is_null() {
        "<null>".to_string()
    } else {
        String::from_utf8_lossy(std::slice::from_raw_parts(p as *const u8, slen.min(80)))
            .into_owned()
    };
    let msg = format!(
        "type={}({}) rawlen={} tolstring_len={} preview=[{}]",
        t, tn_str, rawlen, slen, preview
    );
    log_line(&format!("[echo] {}", msg));
    push_str(l, &msg);
    1
}


// ---------------------------------------------------------------------------
// read_saveparam: direct memory read of FPalIndividualCharacterSaveParameter.
// Offsets computed from the kit header declaration order (natural x64
// alignment); Lua passes the struct address (sp:GetAddress()), numbers only.
// This bypasses UE4SS's broken enum/array property reads entirely.
// ---------------------------------------------------------------------------

const O_GENDER: usize = 0x10; // EPalGenderType : uint8 (0=None 1=Male 2=Female)
const O_LEVEL: usize = 0x20; // uint8
const O_RANK: usize = 0x21; // uint8
const O_RANK_HP: usize = 0x24; // uint8 x4
const O_RANK_ATTACK: usize = 0x25;
const O_RANK_DEFENCE: usize = 0x26;
const O_RANK_CRAFTSPEED: usize = 0x27;
const O_EXP: usize = 0x28; // int64
const O_NICKNAME: usize = 0x30; // FString
const O_FILTERED: usize = 0x40; // FString
const O_TALENT_HP: usize = 0x80; // uint8 x4 (individual values)
const O_TALENT_MELEE: usize = 0x81;
const O_TALENT_SHOT: usize = 0x82;
const O_TALENT_DEFENSE: usize = 0x83;
const O_PASSIVES: usize = 0x90; // TArray<FName>
const O_CRAFTSPEED: usize = 0xEC; // int32

// Offset of the FPalIndividualCharacterSaveParameter struct inside
// UPalIndividualCharacterParameter, calibrated empirically from hexdump
// anchors (Level=41 byte @ struct+0x20, Exp=517320 @ +0x28, talents in
// 0..=100 @ +0x80, FullStomach float ~90.45 @ +0x84 — all consistent).
// Lua passes the parameter OBJECT address (param:GetAddress()); the struct
// base is derived here. Version-specific (game v0.7.x).
const SAVEPARAM_OFF: usize = 0x3D0;

#[cfg(windows)]
fn is_readable(addr: usize, len: usize) -> bool {
    if addr < 0x1_0000 || addr > 0x0000_7FFF_FFFF_0000 || len > 0x10_0000 {
        return false;
    }
    extern "system" {
        fn VirtualQuery(
            lpAddress: *const core::ffi::c_void,
            lpBuffer: *mut core::ffi::c_void,
            dwLength: usize,
        ) -> usize;
    }
    // x64 MEMORY_BASIC_INFORMATION: BaseAddress@0 AllocationBase@8
    // AllocationProtect@16 PartitionId@20(+pad) RegionSize@24 State@32 Protect@36
    let mut buf = [0u8; 64];
    let got = unsafe { VirtualQuery(addr as *const _, buf.as_mut_ptr() as *mut _, buf.len()) };
    if got == 0 {
        return false;
    }
    let state = u32::from_le_bytes(buf[32..36].try_into().unwrap());
    let protect = u32::from_le_bytes(buf[36..40].try_into().unwrap()) & 0xFF;
    if state != 0x1000 {
        return false; // MEM_COMMIT
    }
    matches!(protect, 0x02 | 0x04 | 0x08 | 0x20 | 0x40 | 0x80)
}

#[cfg(not(windows))]
fn is_readable(_addr: usize, _len: usize) -> bool {
    false
}

fn ru8(base: usize, off: usize) -> Option<u8> {
    if is_readable(base + off, 1) {
        Some(unsafe { ((base + off) as *const u8).read_volatile() })
    } else {
        None
    }
}

fn ru32(base: usize, off: usize) -> Option<u32> {
    if is_readable(base + off, 4) {
        Some(unsafe { ((base + off) as *const u32).read_volatile() })
    } else {
        None
    }
}

fn ru64(base: usize, off: usize) -> Option<u64> {
    if is_readable(base + off, 8) {
        Some(unsafe { ((base + off) as *const u64).read_volatile() })
    } else {
        None
    }
}

fn rptr(base: usize, off: usize) -> Option<usize> {
    ru64(base, off).map(|v| v as usize)
}

fn read_fstring(base: usize, off: usize) -> Option<String> {
    let data = rptr(base, off)?;
    let num = ru32(base, off + 8)? as i32; // wchar count including NUL
    if data == 0 || num <= 1 {
        return Some(String::new());
    }
    if num > 512 {
        return None;
    }
    if !is_readable(data, num as usize * 2) {
        return None;
    }
    let slice = unsafe { std::slice::from_raw_parts(data as *const u16, (num - 1) as usize) };
    Some(String::from_utf16_lossy(slice))
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

fn json_str_or_null(s: &str) -> String {
    if s.is_empty() {
        "null".to_string()
    } else {
        format!("\"{}\"", json_escape(s))
    }
}

static MEMREAD_LOGGED: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// FName -> String via FName::ToString. UE4SS logs the resolved runtime
// address at init ("[PS] Found FName::ToString: 0x..."); we harvest it from
// UE4SS.log (located next to the game exe, three dirs up from our dll).
// ---------------------------------------------------------------------------

static FNAME_TOSTRING: AtomicUsize = AtomicUsize::new(usize::MAX); // MAX=unresolved, 0=failed

#[cfg(windows)]
fn dll_dir() -> Option<std::path::PathBuf> {
    extern "system" {
        fn GetModuleHandleExW(dwFlags: u32, lpModuleName: *const u16, phModule: *mut isize) -> i32;
        fn GetModuleFileNameW(hModule: isize, lpFilename: *mut u16, nSize: u32) -> u32;
    }
    const FROM_ADDRESS: u32 = 0x4;
    const UNCHANGED_REFCOUNT: u32 = 0x2;
    let mut h: isize = 0;
    let ok = unsafe {
        GetModuleHandleExW(FROM_ADDRESS | UNCHANGED_REFCOUNT, dll_dir as *const u16, &mut h)
    };
    if ok == 0 {
        return None;
    }
    let mut buf = [0u16; 1024];
    let n = unsafe { GetModuleFileNameW(h, buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..n as usize]);
    std::path::PathBuf::from(path).parent().map(|p| p.to_path_buf())
}

#[cfg(not(windows))]
fn dll_dir() -> Option<std::path::PathBuf> {
    None
}

fn resolve_fname_tostring() -> Option<usize> {
    let cur = FNAME_TOSTRING.load(Ordering::SeqCst);
    if cur != usize::MAX {
        return if cur == 0 { None } else { Some(cur) };
    }
    let mut found = 0usize;
    if let Some(dir) = dll_dir() {
        // scripts -> Palws -> Mods -> Win64
        if let Some(win64) = dir
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
        {
            let logp = win64.join("UE4SS.log");
            if let Ok(content) = std::fs::read_to_string(&logp) {
                for line in content.lines() {
                    if let Some(pos) = line.find("Found FName::ToString: 0x") {
                        let hex = line[pos + "Found FName::ToString: 0x".len()..].trim();
                        if let Ok(v) = usize::from_str_radix(hex, 16) {
                            found = v;
                        }
                    }
                }
            }
        }
    }
    log_line(&format!("[fname] FName::ToString resolve -> {:#x}", found));
    FNAME_TOSTRING.store(found, Ordering::SeqCst);
    if found == 0 { None } else { Some(found) }
}

#[repr(C)]
struct FStrOut {
    data: u64,
    num: i32,
    max: i32,
}

/// Resolve an FName (8 bytes at fname_addr) via the game's own
/// FName::ToString. NOTE: out.data is allocated by the game allocator and
/// intentionally leaked (a few bytes per call, only on terminal dumps).
fn fname_to_string(fname_addr: usize) -> Option<String> {
    let f = resolve_fname_tostring()?;
    if !is_readable(fname_addr, 8) {
        return None;
    }
    type ToStringFn = unsafe extern "C" fn(this_: usize, out: *mut FStrOut);
    let fnc: ToStringFn = unsafe { std::mem::transmute(f) };
    let mut out = FStrOut { data: 0, num: 0, max: 0 };
    unsafe { fnc(fname_addr, &mut out) };
    if out.data == 0 || out.num <= 1 {
        return Some(String::new());
    }
    if out.num > 256 || !is_readable(out.data as usize, out.num as usize * 2) {
        return None;
    }
    let slice =
        unsafe { std::slice::from_raw_parts(out.data as *const u16, (out.num - 1) as usize) };
    Some(String::from_utf16_lossy(slice))
}

fn read_passives(base: usize) -> Vec<String> {
    let mut out = Vec::new();
    if let (Some(data), Some(num)) = (rptr(base, O_PASSIVES), ru32(base, O_PASSIVES + 8)) {
        if data != 0 {
            for i in 0..num.min(16) as usize {
                if let Some(s) = fname_to_string(data + i * 8) {
                    if !s.is_empty() {
                        out.push(s);
                    }
                }
            }
        }
    }
    out
}

unsafe fn read_saveparam_impl(l: *mut lua_State) -> c_int {
    let mut isnum: c_int = 0;
    let addr = lua_tointegerx(l, 1, &mut isnum) as usize;
    if isnum == 0 || addr == 0 || !is_readable(addr, 0x100) {
        push_str(l, "");
        return 1;
    }
    let base = addr + SAVEPARAM_OFF;
    let mut parts: Vec<String> = Vec::new();
    if let Some(g) = ru8(base, O_GENDER) {
        parts.push(format!(
            "\"gender\":\"{}\"",
            match g {
                1 => "male",
                2 => "female",
                _ => "unknown",
            }
        ));
    }
    if let Some(nick) = read_fstring(base, O_NICKNAME) {
        parts.push(format!("\"nickname\":{}", json_str_or_null(&nick)));
    }
    if let Some(f) = read_fstring(base, O_FILTERED) {
        parts.push(format!("\"filtered_nickname\":{}", json_str_or_null(&f)));
    }
    if let Some(v) = ru8(base, O_LEVEL) {
        parts.push(format!("\"level_mem\":{}", v));
    }
    if let Some(v) = ru8(base, O_RANK) {
        parts.push(format!("\"rank\":{}", v));
    }
    if let (Some(h), Some(m), Some(s), Some(d)) = (
        ru8(base, O_TALENT_HP),
        ru8(base, O_TALENT_MELEE),
        ru8(base, O_TALENT_SHOT),
        ru8(base, O_TALENT_DEFENSE),
    ) {
        parts.push(format!(
            "\"talent\":{{\"hp\":{},\"melee\":{},\"shot\":{},\"defense\":{}}}",
            h, m, s, d
        ));
    }
    if let (Some(h), Some(a), Some(d), Some(c)) = (
        ru8(base, O_RANK_HP),
        ru8(base, O_RANK_ATTACK),
        ru8(base, O_RANK_DEFENCE),
        ru8(base, O_RANK_CRAFTSPEED),
    ) {
        parts.push(format!(
            "\"rank_stats\":{{\"hp\":{},\"attack\":{},\"defence\":{},\"craft_speed\":{}}}",
            h, a, d, c
        ));
    }
    if let Some(e) = ru64(base, O_EXP) {
        parts.push(format!("\"exp\":{}", e));
    }
    if let Some(c) = ru32(base, O_CRAFTSPEED) {
        parts.push(format!("\"craft_speed\":{}", c));
    }
    let passives = read_passives(base);
    parts.push(format!(
        "\"passives\":[{}]",
        passives
            .iter()
            .map(|p| format!("\"{}\"", json_escape(p)))
            .collect::<Vec<_>>()
            .join(",")
    ));
    let frag = parts.join(",");
    let n = MEMREAD_LOGGED.fetch_add(1, Ordering::SeqCst);
    if n < 5 {
        log_line(&format!("[read_saveparam] addr={:#x} frag=[{}]", addr, frag));
    }
    push_str(l, &frag);
    1
}

unsafe fn ping_impl(l: *mut lua_State) -> c_int {
    push_str(l, "pong from rust");
    1
}

// hexdump: dump process memory to palws.log as hex rows. Used to calibrate
// struct layouts empirically (anchor on known values like level).
unsafe fn hexdump_impl(l: *mut lua_State) -> c_int {
    let mut isnum: c_int = 0;
    let addr = lua_tointegerx(l, 1, &mut isnum) as usize;
    let mut isnum2: c_int = 0;
    let mut len = lua_tointegerx(l, 2, &mut isnum2) as usize;
    if isnum == 0 || addr == 0 {
        push_str(l, "bad addr");
        return 1;
    }
    if isnum2 == 0 || len == 0 {
        len = 0x100;
    }
    if len > 0x2000 {
        len = 0x2000;
    }
    log_line(&format!("[hexdump] addr={:#x} len={:#x}", addr, len));
    let mut off = 0usize;
    while off < len {
        let chunk = core::cmp::min(16usize, len - off);
        if !is_readable(addr + off, chunk) {
            log_line(&format!("+{:04x}: <unreadable>", off));
            break;
        }
        let mut row = format!("+{:04x}:", off);
        for i in 0..chunk {
            let b = unsafe { ((addr + off + i) as *const u8).read_volatile() };
            row.push_str(&format!(" {:02x}", b));
        }
        log_line(&row);
        off += chunk;
    }
    push_str(l, "ok");
    1
}

unsafe fn version_impl(l: *mut lua_State) -> c_int {
    let v = lua_version(l);
    push_str(l, &format!("palws ok, host lua core number = {:.0}", v));
    1
}

unsafe fn client_count_impl(l: *mut lua_State) -> c_int {
    lua_pushinteger(l, CLIENTS.load(Ordering::SeqCst) as lua_Integer);
    1
}

// ---------------------------------------------------------------------------
// exported entry points (panic-guarded)
// ---------------------------------------------------------------------------

macro_rules! export_fn {
    ($fname:ident, $lname:literal, $implf:ident) => {
        unsafe extern "C-unwind" fn $fname(l: *mut lua_State) -> c_int {
            guarded!(l, $lname, { $implf(l) })
        }
    };
}

export_fn!(start_server, "start_server", start_server_impl);
export_fn!(broadcast_lua, "broadcast", broadcast_impl);
export_fn!(notify, "notify", notify_impl);
export_fn!(echo, "echo", echo_impl);
export_fn!(read_saveparam, "read_saveparam", read_saveparam_impl);
export_fn!(hexdump, "hexdump", hexdump_impl);
export_fn!(ping, "ping", ping_impl);
export_fn!(version, "version", version_impl);
export_fn!(client_count, "client_count", client_count_impl);

unsafe fn luaopen_impl(l: *mut lua_State) -> c_int {
    log_line("[step] luaopen: createtable");
    lua_createtable(l, 0, 10);
    let funcs: &[(&str, unsafe extern "C-unwind" fn(*mut lua_State) -> c_int)] = &[
        ("start_server", start_server),
        ("broadcast", broadcast_lua),
        ("client_count", client_count),
        ("read_saveparam", read_saveparam),
        ("hexdump", hexdump),
        ("ping", ping),
        ("version", version),
        ("echo", echo),
        ("notify", notify),
    ];
    for (name, f) in funcs {
        push_str(l, name);
        lua_pushcclosure(l, *f, 0);
        lua_settable(l, -3);
    }
    log_line("[step] luaopen: functions registered");
    push_str(l, "backend");
    push_str(l, "rust-cdylib-vendored-lua54+tokio+axum");
    lua_settable(l, -3);
    1
}

/// Pin this dll in the host process forever. UE4SS hot reload destroys the
/// mod's lua_State; Lua's package.loadlib handle then gets GC'd and the dll
/// would be FreeLibrary'd while our tokio worker threads still execute its
/// code -> crash on (or right after) the next require. Pinning prevents the
/// unload; statics (runtime/channel) survive and start_server stays idempotent.
#[cfg(windows)]
unsafe fn pin_module() {
    extern "system" {
        fn GetModuleHandleExW(dwFlags: u32, lpModuleName: *const u16, phModule: *mut isize) -> i32;
    }
    const GET_MODULE_HANDLE_EX_FLAG_PIN: u32 = 0x00000001; // 0x2 is UNCHANGED_REFCOUNT!
    const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x00000004;
    let mut h: isize = 0;
    let ok = GetModuleHandleExW(
        GET_MODULE_HANDLE_EX_FLAG_PIN | GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        luaopen_palws as *const u16,
        &mut h,
    );
    log_line(&format!("[luaopen] module pin: ok={} h={:#x}", ok, h));
}

#[cfg(not(windows))]
unsafe fn pin_module() {}

#[no_mangle]
pub extern "C" fn luaopen_palws(l: *mut lua_State) -> c_int {
    let n = LOAD_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
    log_line(&format!(
        "[luaopen] load #{n} enter; LOAD_COUNT@{:#x} L={:?}",
        &LOAD_COUNT as *const _ as usize, l
    ));
    unsafe { pin_module(); }
    let r = catch_unwind(AssertUnwindSafe(|| unsafe { luaopen_impl(l) }));
    match r {
        Ok(rc) => {
            log_line(&format!("[luaopen] load #{n} ok, rc={rc}"));
            rc
        }
        Err(e) => {
            let msg = panic_payload(&*e);
            log_line(&format!("[luaopen] load #{n} PANIC: {msg}"));
            0
        }
    }
}
