//! Simulates UE4SS hot reload: three fresh lua_States in one process,
//! each `require "palws"` (same dll) and calling start_server twice.
//! If the dll mis-handles reload (non-idempotent globals, panics), this
//! crashes or errors here instead of inside the game.
//!
//! Build the dll first, then run:
//!   cargo build --release -p palws
//!   cargo run --example reload_harness -p palws
use mlua_sys::*;
use std::ffi::CString;

/// Absolute cpath to the workspace's release dll, with forward slashes for Lua.
fn dll_cpath() -> String {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("release");
    let s = dir.to_string_lossy().replace('\\', "/");
    format!("{s}/?.dll")
}

unsafe fn run_session(tag: &str, cpath: &str) {
    let l = luaL_newstate();
    assert!(!l.is_null(), "luaL_newstate failed");
    luaL_openlibs(l);
    let code = format!(
        r#"
package.cpath = [[{cpath}]] .. ";" .. package.cpath
local m = require "palws"
print("[{tag}] backend:", m.backend)
print("[{tag}] start_server #1:", m.start_server(32123))
print("[{tag}] version:", m.version())
print("[{tag}] client_count:", m.client_count())
print("[{tag}] broadcast valid:", m.broadcast([=[{{"protocol":"palws","version":1,"type":"log","id":"lua-1","payload":{{"level":"info","source":"lua","message":"hi"}}}}]=]))
print("[{tag}] broadcast invalid:", m.broadcast("not json"))
print("[{tag}] take_command (empty):", tostring(m.take_command()))
print("[{tag}] begin_session:", m.begin_session())
print("[{tag}] start_server #2:", m.start_server(32123))
"#,
        tag = tag,
        cpath = cpath,
    );
    let c = CString::new(code).unwrap();
    let rc = luaL_loadstring(l, c.as_ptr());
    assert_eq!(rc, 0, "loadstring failed");
    let prc = lua_pcallk(l, 0, LUA_MULTRET, 0, 0, None);
    if prc != 0 {
        let mut len: usize = 0;
        let err = lua_tolstring(l, -1, &mut len);
        if !err.is_null() {
            eprintln!(
                "[{tag}] pcall error: {}",
                String::from_utf8_lossy(std::slice::from_raw_parts(err as *const u8, len))
            );
        }
    }
    lua_close(l);
    println!("[{tag}] session closed ok");
}

fn main() {
    let cpath = dll_cpath();
    println!("using package.cpath = {cpath}");
    unsafe {
        run_session("one", &cpath);
        run_session("two", &cpath);
        run_session("three", &cpath);
    }
    println!("HARNESS DONE");
}
