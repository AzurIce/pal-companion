# palws — Palworld WebSocket 同步 Mod（UE4SS）

UE4SS Lua Mod，加载一个 Rust 原生模块（`palws.dll`），在 `127.0.0.1` 提供
WebSocket 服务，供外部工具（pal-companion 等）显式同步游戏内的帕鲁数据。

## 布局

- `src/lib.rs` — Rust cdylib。内嵌 stock Lua 5.4（`mlua-sys` vendored），
  与 UE4SS 内嵌解释器 ABI 一致。Axum（tokio）提供 WebSocket + `/health` +
  根状态页。所有导出入口都经 `catch_unwind` 加固以支持热重载。
- `lua/main.lua` — UE4SS Mod 脚本；`require('palws')` 后启动服务，实现
  唯一的 `requestSnapshot` 同步状态机（F7 与网页刷新共用）。**此文件是
  权威工作副本；游戏目录下的副本是部署产物。**
- `examples/reload_harness.rs` — 热重载测试 harness。
- `scripts/build.sh` — 编译并部署到 Workshop-UE4SS Mod 目录。
- `scripts/package.ps1` — 打包发布产物（不含网页、不含构建脚本）。

## 生产 v1 行为

- 不自动同步：不根据 UI、Widget、地图加载或对象创建触发任何请求。
- 唯一入口 `requestSnapshot`：F7 键位与网页 `snapshot.request` 命令复用。
- Lua 状态机每步重新获取 `PlayerState`，不跨定时器持有任何 UObject。
- Lua → Rust 只走 `palws.broadcast(json)`，不经过文件系统。
- Rust 缓存最后一份快照，新连接立即补发；消息带单调 `seq`，网页忽略旧序号。
- WebSocket 只监听 `127.0.0.1`，不提供静态网页托管。

## 安装（Workshop UE4SS Experimental）

1. 在 Steam 创意工坊订阅并启用 **UE4SS Experimental (Palworld)**。
2. 把发布压缩包中的 `Palws` 目录解压到
   `Palworld\Mods\NativeMods\UE4SS\Mods\`。压缩包已含 `enabled.txt`，
   无需改动 `mods.txt`、`mods.json` 或 `UE4SS-settings.ini`。
3. 本地源码构建可用 `crates/palws/scripts/build.sh` 部署。
4. 启动游戏后在 `Palworld\Mods\NativeMods\UE4SS\UE4SS.log` 确认
   `[Palws] start_server: ok=true`。

## 协议

WebSocket 端点 `ws://127.0.0.1:32123/ws`，所有消息为 JSON text frame，
统一 envelope：

```json
{"protocol":"palws","version":1,"type":"snapshot","id":"...","request_id":"...",
 "seq":42,"timestamp_ms":1786595000000,"payload":{...}}
```

服务端 → 网页：`server.hello` / `sync.status` / `snapshot` / `log` / `error` / `pong`。
网页 → 服务端：`client.hello` / `snapshot.request` / `ping`。
入站上限 64 KiB；出站快照上限 8 MiB。

## 发布命名

`palws-dev-YYYYMMDD-短SHA`（由 Windows CI 生成 prerelease）。
