//! Simulates UE4SS hot reload: three fresh lua_States in one process,
//! each `require "palws"` (same dll) and calling start_server twice.
//! If the dll mis-handles reload (non-idempotent globals, panics), this
//! crashes or errors here instead of inside the game.
use mlua_sys::*;
use std::ffi::CString;

unsafe fn run_session(tag: &str) {
    let l = luaL_newstate();
    assert!(!l.is_null(), "luaL_newstate failed");
    luaL_openlibs(l);
    let code = format!(
        r#"
package.cpath = [[C:\Users\xiaob\palworld-dump\palws-spike\target\release\?.dll]]
local m = require "palws"
print("[{tag}] backend:", m.backend)
print("[{tag}] start_server #1:", m.start_server(32123))
print("[{tag}] ping:", m.ping())
print("[{tag}] version:", m.version())
print("[{tag}] start_server #2:", m.start_server(32123))
print("[{tag}] echo:", m.echo("hello from {tag}"))
"#,
        tag = tag
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
    unsafe {
        run_session("one");
        run_session("two");
        run_session("three");
    }
    println!("HARNESS DONE");
}
