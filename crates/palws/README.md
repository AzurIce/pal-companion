# palws — Palworld WebSocket broadcast mod (UE4SS)

Lua mod for UE4SS that loads a Rust native module (`palws.dll`) providing a
WebSocket broadcast server + static HTTP file server on `127.0.0.1`, used by
external tools (pal-companion etc.).

## Layout

- `src/lib.rs` — Rust cdylib. Vendored stock Lua 5.4 (`mlua-sys`), so it talks
  the same Lua 5.4 ABI as UE4SS's embedded interpreter. Axum (tokio) for
  WebSocket broadcast + HTTP static file serving. All entry points are
  `catch_unwind`-hardened for hot reload.
- `lua/main.lua` — the UE4SS mod script; `require('palws')` and wires keybinds,
  hooks, and the WS/HTTP server lifecycle. **This file is the authoritative
  working copy; game-dir copies are deployments.**
- `examples/reload_harness.rs` — hot-reload test harness.
- `scripts/build.sh` — build + deploy to the Workshop-UE4SS mod dir.

## Install (Workshop UE4SS Experimental)

1. Subscribe to **UE4SS Experimental (Palworld)** on Steam Workshop, enable it
   in 选项 → Mod 管理, launch once so the loader deploys it.
2. `crates/palws/scripts/build.sh` deploys `palws.dll` + `main.lua` to
   `Palworld\Mods\NativeMods\UE4SS\Mods\Palws\` and touches `enabled.txt`
   (UE4SS's enabled.txt scan picks up the mod even if the loader regenerates
   `mods.txt`). Run it from the workspace root (`scripts/build.sh`) or from
   `crates/palws/`.
3. On game start, UE4SS loads Palws; verify in
   `Palworld\Mods\NativeMods\UE4SS\UE4SS.log`.

Runtime payload (save-param dumps) is written to
`C:\Users\xiaob\palworld-dump\palws-payload.json` — deliberately outside the
game tree so loader migrations never break the path.

## Maintenance checklist (version-sensitive bits, by fragility)

| Item | Location | Drift trigger | Fix |
|---|---|---|---|
| `SAVEPARAM_OFF` (struct offset) | `src/lib.rs` | every major game update | recalibrate with hexdump + level anchor (see below) |
| `WBP_PalStorageMenu_C` class name | `lua/main.lua` | class rename | rename; F6 wide-capture catch-all finds it fast |
| `FName::ToString` address | `lua/main.lua` | any | self-healing — re-read from UE4SS.log at each boot |
| Lua 5.4 ABI (vendored) | `src/lib.rs` | loader swap | re-run spike (ping/version/echo) after changing UE4SS build |
| `bUseUObjectArrayCache` ini tweak | manual-UE4SS only | — | do NOT carry over to Workshop UE4SS; it manages its own settings |

### Recalibrating SAVEPARAM_OFF with hexdump

The mod's built-in hexdump tool (F4 keybind) is the self-rescue tool for offset
drift: dump the level anchor struct, diff against the known-good layout, and
update `SAVEPARAM_OFF`. Keep it in the mod; it is gated behind keybinds and
silent otherwise.

## Release naming

`palws-<mod>.<game major>.<game minor>` (e.g. `palws-0.7.1-1.0`). Bump the mod
component on behavior changes, keep the game component pinned to the Palworld
version the offsets were calibrated against.

## Manual-UE4SS install (legacy)

Old layout: copy to `Pal\Binaries\Win64\ue4ss\Mods\Palws\` with `dwmapi.dll`
proxy loader. **Do not run manual + Workshop UE4SS simultaneously** — a manual
`dwmapi.dll` beside the Workshop loader crashes the game at startup.
