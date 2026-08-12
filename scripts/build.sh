#!/usr/bin/env bash
# Build palws.dll and deploy to the Palworld Workshop-UE4SS mod dir.
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

if [ "${1:-}" != "--only-lua" ]; then
  cargo build --release
fi

mkdir -p "$MOD_DIR/Scripts"
cp target/release/palws.dll "$MOD_DIR/Scripts/palws.dll"
cp lua/main.lua "$MOD_DIR/Scripts/main.lua"
touch "$MOD_DIR/enabled.txt"

echo "deployed -> $MOD_DIR"
ls -la "$MOD_DIR" "$MOD_DIR/Scripts"
