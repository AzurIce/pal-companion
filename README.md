# Pal Companion

Pal Companion 是一个《幻兽帕鲁》配种规划网页，并可通过可选的 Palws Mod 从正在运行的游戏中同步队伍、帕鲁终端和据点帕鲁。

- 在线网页：<https://azurice.github.io/pal-companion/>
- Mod 下载：<https://github.com/AzurIce/pal-companion/releases>
- 网页数据保存在浏览器本地；网页本身不使用独立版本号。

## 功能

- 查询两只帕鲁的配种结果和指定帕鲁的亲本组合。
- 根据已持有帕鲁和目标被动规划配种链。
- 记录队伍、帕鲁终端和据点中的帕鲁。
- 通过 Palws 从游戏同步物种、性别、等级、昵称、被动、头领、幸运和最爱状态。

## Palws Mod：安装与使用

Palws 只支持 Windows 版 Palworld。它通过 UE4SS 读取本地游戏状态，并在 `127.0.0.1:32123` 提供仅本机可访问的 WebSocket；不会向外部服务器上传存档。

### 安装

1. 在 Steam 创意工坊订阅并启用 **UE4SS Experimental (Palworld)**。
2. 启动一次游戏，让 UE4SS 创建目录，然后正常退出游戏。
3. 从 [Releases](https://github.com/AzurIce/pal-companion/releases) 下载最新的 `palws-dev-*.zip`。
4. 将压缩包内的 `Palws` 文件夹解压到：

   ```text
   <Palworld>\Mods\NativeMods\UE4SS\Mods\
   ```

   安装后应为：

   ```text
   <Palworld>\Mods\NativeMods\UE4SS\Mods\Palws\
   ├── enabled.txt
   └── Scripts\
       ├── main.lua
       └── palws.dll
   ```

5. 正常启动游戏。不要使用 UE4SS 的 `Ctrl+R` 热重载 Palws；它会同时重载其他 Lua Mod，可能导致游戏长时间卡住或 Mod 处于半卸载状态。

若不确定 Palworld 的安装位置，可在 Steam 中打开“库 → Palworld → 管理 → 浏览本地文件”。

### 使用

1. 打开在线网页，页脚“游戏同步”应从“未连接”变为“已连接”。
2. 进入存档并打开一次帕鲁终端。Palws 会向游戏服务器请求全部 32 页数据，等待同步后一次性发送给网页，通常需要约 10 秒；无需手动逐页翻动终端。
3. 网页出现“从游戏同步”提示后：
   - 选择“合并”可将本次结果加入现有列表；
   - 打开“自动同步”后，游戏数据会作为权威数据覆盖网页中的整个已持有列表，包括手动添加的条目。
4. 需要重新同步时按 `F7`，再等待约 10 秒。

如果网页一直显示“未连接”，检查以下位置：

- Mod 文件是否位于上面的准确目录，而不是多套了一层文件夹；
- `Palworld\Mods\NativeMods\UE4SS\UE4SS.log` 中是否有 `[Palws] start_server: ok=true`；
- 本机的 `32123` 端口是否被其他程序占用；
- 游戏更新或 UE4SS 更新后，是否需要下载新的 Palws dev build。

每个 Mod 压缩包都以构建日期和提交短 SHA 命名，例如 `palws-dev-20260813-c2cdc04.zip`。这是持续开发构建，出现兼容性问题时可从 Releases 下载较早构建回退。

## 本地开发

网页：

```powershell
dx serve
```

Palws 本地编译并部署使用 `crates/palws/scripts/build.sh`。默认游戏目录写在脚本中；仅更新 Lua 时可执行：

```bash
crates/palws/scripts/build.sh --only-lua
```

运行测试：

```powershell
cargo test --workspace
```

本仓库通过相邻目录 `../dioxus-flow` 引用 [AzurIce/dioxus-flow](https://github.com/AzurIce/dioxus-flow)。本地开发时需将两个仓库并排放置。

## 分支与发布

- `dev` 是日常开发和集成分支。
- `main` 只接收已经验证、可以部署的改动；建议通过 PR 从 `dev` 合入。
- 推送到 `main` 后，网页 workflow 独立部署 GitHub Pages，不创建网页版本。
- 当提交修改 `crates/palws/**`、Palws 的 Cargo 构建输入或 Mod workflow 时，Windows CI 会创建一个新的 GitHub prerelease：`palws-dev-YYYYMMDD-短SHA`。
- Mod release 只包含 `Palws` 安装目录和 SHA-256 校验文件，不包含网页构建产物。

这种模型让网页保持连续部署，同时让每个 Mod 二进制都能追溯到唯一提交。`main` 上纯网页改动不会产生无意义的 Mod release。
