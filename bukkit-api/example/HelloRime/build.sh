#!/bin/sh
# Builds HelloRime.jar against RimeMC's Bukkit API jar (needs a JDK 17+).
set -e
cd "$(dirname "$0")"
API=../../../crates/rime-server/assets/bukkit/rime-bukkit-api.jar
OUT=$(mktemp -d)
javac --release 17 -cp "$API" -d "$OUT" src/com/example/hello/HelloRime.java
cp src/plugin.yml src/config.yml "$OUT"/
jar --create --file HelloRime.jar -C "$OUT" .
rm -r "${OUT:?}"
echo "built HelloRime.jar — copy it into the server's plugins/ folder"
