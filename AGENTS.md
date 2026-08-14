# AGENTS.md — pal-companion 仓库约定

给 AI 编程助手与协作者的约定。修改代码前请先阅读本文件。

## 依赖：dioxus-flow（重要）

- **提交 / push 必须使用 git 依赖**：
  `Cargo.toml` 中 `dioxus-flow = { git = "https://github.com/AzurIce/dioxus-flow", rev = "7499541bfd61f7ad2043359338ae9e1e923caa76" }`。
  CI（GitHub Pages 部署、Palws release）和 `nix build` 都依赖这个声明及 `Cargo.lock` 里的 git 源。
- **本地快速调试**可临时改为 `path = "../dioxus-flow"`（需要本机存在 ~/Files/dioxus-flow 检出），
  便于直接改 dioxus-flow 源码即时生效。
- **push 前必须切回 git 依赖**，并确认 `Cargo.lock` 已重新解析（`cargo update`）——
  带着 path 依赖 push 会让 CI 和 Nix 构建失败。
- 依赖版本由 `rev` 固定；升级 = 更新 rev + `cargo update`。
- 旧方案是对 dioxus-flow 157de4f8 打 patch（`.github/patches/dioxus-flow-render-node.patch`），
  改动已合入上游 commit 7499541，该 patch 已删除，**不要再引入**。

## 构建与测试

- `nix develop` 进入开发环境（Rust 稳定版 + wasm32-unknown-unknown 目标 + dioxus-cli +
  wasm-bindgen-cli 0.2.127 + binaryen）。
- `cargo test --workspace` 必须保持通过。
- `dx build --platform web --release` 构建网页；部署时加 `--base-path /pal-companion/`
  （Dioxus.toml 不写 base_path，由 CI/Nix 构建传入）。
- `nix build` 产出等价 CI 的 GitHub Pages 网页包（含 404.html SPA 回退）。
- crates/palws 是 Windows UE4SS Mod（cdylib），仅 Windows 目标构建；改动后跑 `cargo test -p palws`。

## 其他

- 网页数据存浏览器本地；同步经 Palws 本机 WebSocket（127.0.0.1:32123），不向外部服务器上传存档。
- 保持现有模块划分（planner / sync / palws 等），改动先跑通测试再提交。
