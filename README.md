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

`enabled.txt` 已包含在压缩包中，UE4SS 会在启动时自动扫描它。无需把 `Palws : 1` 手工添加到 `mods.txt`，也无需修改 `mods.json` 或 `UE4SS-settings.ini`。唯一需要在游戏中启用的是前置的 **UE4SS Experimental (Palworld)**。

若不确定 Palworld 的安装位置，可在 Steam 中打开“库 → Palworld → 管理 → 浏览本地文件”。

### 使用

#### 推荐流程

1. 启动 Palworld，进入要同步的存档，并等待角色可以正常操作。Palws 会随游戏启动，不要求先打开网页。
2. 打开[在线网页](https://azurice.github.io/pal-companion/)。页脚显示“游戏同步：已连接”表示网页已经连接到本机 Palws，但不代表当前存档的数据已经同步。
3. 使用下面任一方式显式触发同步：
   - 在游戏中按 `F7`；即使网页尚未打开也可以使用，Palws 会缓存生成的快照，网页随后连接时会自动收到。
   - 网页显示“已连接”后，点击页脚中的“刷新”。
4. 等待页脚依次显示“已排队”“请求中”“采集中”和“已同步”。请求阶段会显示页数和进度条，整个过程通常需要数秒；同步进行中不要重复触发。
5. 显示“已同步”后，“我的帕鲁”会按队伍、盒子和据点更新。再次同步会替换上一次从游戏同步的条目，手工添加的条目不会被删除。

游戏和网页的启动顺序没有强制要求。网页先打开时会自动重试本机连接；如果游戏启动后仍显示“未连接”，可以等待下一次重试，或刷新网页立即重连。同一游戏进程内，Palws 会向新连接的网页补发最近一次成功快照；退出并重新启动游戏后，需要重新触发同步。

Palws 不会因打开帕鲁终端、切换地图或创建对象而自动同步。只有 `F7` 和网页“刷新”会触发同步，两者进入同一条 Lua 同步流程。为避免连续请求，同步触发后约 15 秒内的新请求会被拒绝。

页脚最右侧的“日志”按钮可展开同步控制台。控制台会显示最近的同步结果和错误，日志区域支持滚动、文本选择和一键复制。据点筛选按游戏中的据点容器分组；筛选项较多时可以横向滚动。

#### 故障排查

**网页一直显示“未连接”**

- 确认 Palworld 正在运行，且 Mod 文件位于上面的准确目录，没有多套一层 `Palws` 文件夹。
- 查看 `Palworld\Mods\NativeMods\UE4SS\UE4SS.log`，正常启动应包含：

  ```text
  [Palws] require 'palws': ok=true
  [Palws] start_server: ok=true -> started on 127.0.0.1:32123
  [Palws] loaded. F7 = request snapshot
  ```

- `start_server: ok=true` 只表示 Lua 调用本身没有抛错，还要确认箭头后是 `started on 127.0.0.1:32123`。如果显示 `bind failed on 127.0.0.1:32123`，说明端口已被其他进程占用；关闭占用程序后重新启动游戏。
- 网页如果早于游戏打开，自动重连间隔会逐步延长；游戏启动后刷新网页可以立即重新连接。

**已经连接但无法同步**

- 确认已经进入存档并可以正常控制角色；主菜单阶段没有可用的本地 `PlayerState`。
- 如果页脚提示任务进行中或触发过于频繁，等待当前同步结束及冷却时间过去后再试。
- 打开页脚最右侧的“日志”查看错误；需要反馈问题时可直接复制其中的诊断内容，并同时检查 `UE4SS.log`。
- 游戏或 UE4SS 更新后，如果 Mod 无法加载，请从 [Releases](https://github.com/AzurIce/pal-companion/releases) 下载最新 Palws 构建。

每个 Mod 压缩包都以构建日期和提交短 SHA 命名，例如 `palws-dev-YYYYMMDD-短SHA.zip`。这是持续开发构建，出现兼容性问题时可从 Releases 下载较早构建回退。

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
