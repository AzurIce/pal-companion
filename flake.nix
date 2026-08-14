{
  description = "Pal Companion 开发环境：Dioxus Web 应用（幻兽帕鲁配种计算器）+ Palws Mod";

  inputs = {
    # 与用户主 flake 相同的 nixos-unstable，固定 rev 保证可复现并复用本机缓存
    nixpkgs.url = "github:nixos/nixpkgs/0e251e24a4f24e036a084b6b4b2d2491af4167f4";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      crane,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        # ---- Rust 工具链：稳定版 + wasm32-unknown-unknown 目标（Dioxus Web 构建需要）----
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          targets = [ "wasm32-unknown-unknown" ];
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # ---- wasm-bindgen-cli 0.2.127 ----
        # 仓库 Cargo.lock 锁定 wasm-bindgen 0.2.127；nixpkgs 自带最高 0.2.126，
        # 而 dx build 要求 CLI 与 wasm 模块版本严格一致，因此按锁版本自建。
        wbcSrc = pkgs.fetchCrate {
          pname = "wasm-bindgen-cli";
          version = "0.2.127";
          hash = "sha256-di+qBAdd7pENLiIB9CoZoab+W5xeDoByMREcCGTSzWo=";
        };

        wasm-bindgen-cli = pkgs.rustPlatform.buildRustPackage {
          pname = "wasm-bindgen-cli";
          version = "0.2.127";

          src = wbcSrc;

          cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
            src = wbcSrc;
            pname = "wasm-bindgen-cli";
            version = "0.2.127";
            hash = "sha256-FTv2GZIAQs0ePdIZXIXil7JbZ6kIT05VG6vqC1qNFxQ=";
          };

          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.openssl ];
          doCheck = false;
          meta.mainProgram = "wasm-bindgen";
        };

        # ---- 构建用源码树 ----
        # dioxus-flow 现在是 git 依赖（见 Cargo.toml），无需兄弟目录/补丁。
        # 仅剔除本地 rsproxy 镜像配置：Nix 的 vendored 离线构建用不到它，
        # 且项目级 .cargo/config.toml 会覆盖 crane 注入的 vendored 源配置。
        palSrc = pkgs.runCommand "pal-companion-src" { } ''
          cp -r ${pkgs.lib.cleanSource ./.} $out
          chmod -R u+w $out
          rm -f $out/.cargo/config.toml
        '';

        commonArgs = {
          pname = "pal-companion";
          version = "0.1.0";
          src = palSrc;
        };

        # 先缓存 wasm 目标的依赖（dx build 的 cargo 调用可直接复用）
        cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
          pname = "pal-companion-deps";
          # crane 的 buildDepsOnly 默认以 release profile 运行（CARGO_PROFILE=release），
          # 不要重复传 --release
          cargoExtraArgs = "--target wasm32-unknown-unknown -p pal-companion";
          doCheck = false;
        });

        # GitHub Pages 网页产物（等价于 CI 的 dx build）
        web = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          # dx build --release 需要 wasm-opt（binaryen）
          nativeBuildInputs = [ pkgs.dioxus-cli wasm-bindgen-cli pkgs.binaryen ];
          doCheck = false;
          # 自定义 dx 构建步骤，不使用 crane 基于 cargo build 日志的二进制安装
          doNotPostBuildInstallCargoBinaries = true;
          buildPhaseCargoCommand = ''
            dx build --platform web --release --base-path /pal-companion/
          '';
          installPhaseCommand = ''
            mkdir -p $out
            dist="$(find target -type d -path '*/dx/pal-companion/release/web/public' 2>/dev/null | head -n1)"
            [ -n "$dist" ] || dist="$(find "''${CARGO_TARGET_DIR:-target}" -type d -path '*/dx/pal-companion/release/web/public' 2>/dev/null | head -n1)"
            if [ -z "$dist" ]; then
              echo "error: dx 输出目录未找到" >&2
              exit 1
            fi
            cp -r "$dist"/. $out/
            # SPA 回退（与 CI 一致）
            cp "$out/index.html" "$out/404.html"
          '';
        });

        devShell = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.dioxus-cli
            wasm-bindgen-cli
            pkgs.binaryen # dx build --release 需要 wasm-opt
          ];

          shellHook = ''
            echo "[pal-companion] 开发环境就绪。常用命令："
            echo "  dx serve                                   # 本地开发服务器"
            echo "  dx build --platform web --release          # 构建网页（GitHub Pages）"
            echo "  cargo test --workspace                     # 运行测试"
            echo "  cargo build --release -p palws             # 构建 Palws Mod（需 Windows 目标）"
            echo "  dioxus-flow 为 git 依赖；本地调试可临时改 path = \"../dioxus-flow\"，push 前须切回（见 AGENTS.md）"
          '';
        };
      in
      {
        packages = {
          default = web;
          inherit web wasm-bindgen-cli;
        };

        devShells.default = devShell;
      }
    );
}
