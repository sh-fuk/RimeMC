#!/bin/sh
# Builds crates/rime-server/assets/bukkit/rime-bukkit-api.jar from bukkit-api/src (needs a JDK 17+).
set -e
cd "$(dirname "$0")/.."
OUT=$(mktemp -d)
find bukkit-api/src -name '*.java' > "$OUT/sources.txt"
javac --release 17 -nowarn -d "$OUT/classes" @"$OUT/sources.txt"
mkdir -p crates/rime-server/assets/bukkit
jar --create --file crates/rime-server/assets/bukkit/rime-bukkit-api.jar -C "$OUT/classes" .
rm -r "${OUT:?}"
echo "built crates/rime-server/assets/bukkit/rime-bukkit-api.jar"
