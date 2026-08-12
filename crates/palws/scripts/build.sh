#!/usr/bin/env bash
# Build palws.dll and deploy to the Palworld Workshop-UE4SS mod dir.
# Run from the workspace root (E:\pal-companion) or crates/palws/:
#
#   scripts/build.sh            # release build + deploy
#   scripts/build.sh --only-lua # skip cargo, just copy main.lua
#
# Target layout (Workshop UE4SS Experimental loader):
#   Palworld\Mods\NativeMods\UE4SS\Mods\Palws\
#     enabled.txt          <- UE4SS enabled.txt scan; survives mods.txt regeneration
#     Scripts\main.lua
#     Scripts\palws.dll
set -euo pipefail

GAME_ROOT="/g/SteamLibrary/steamapps/common/Palworld"
MOD_DIR="$GAME_ROOT/Mods/NativeMods/UE4SS/Mods/Palws"
# workspace layout: crates/palws/scripts/build.sh -> WS root is two up
CRATE_DIR="$(cd "$(dirname "$0")/.." && pwd)"
WS_ROOT="$(cd "$CRATE_DIR/../.." && pwd)"

if [ "${1:-}" != "--only-lua" ]; then
  (cd "$WS_ROOT" && cargo build --release -p palws)
fi

mkdir -p "$MOD_DIR/Scripts"
cp "$WS_ROOT/target/release/palws.dll" "$MOD_DIR/Scripts/palws.dll"
cp "$CRATE_DIR/lua/main.lua" "$MOD_DIR/Scripts/main.lua"
touch "$MOD_DIR/enabled.txt"

echo "deployed -> $MOD_DIR"
ls -la "$MOD_DIR" "$MOD_DIR/Scripts"
