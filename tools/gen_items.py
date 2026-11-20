#!/usr/bin/env python3
"""Generate items.tsv (item name <TAB> max stack size, in protocol-id order).

Stack sizes come from minecraft-data (MIT licensed facts); item order is
checked against registries.tsv generated from the vanilla reports.

Usage: python3 tools/gen_items.py <minecraft-data>/data/pc/26.1/items.json crates/rime-server/assets/26.1
"""
import json
import os
import sys

items = json.load(open(sys.argv[1]))
out = sys.argv[2]
reg = None
for line in open(os.path.join(out, "registries.tsv")):
    k, v = line.rstrip("\n").split("\t")
    if k == "minecraft:item":
        reg = v.split(" ")
assert reg is not None and len(reg) == len(items), (len(reg or []), len(items))
items.sort(key=lambda i: i["id"])
for i, it in enumerate(items):
    assert it["id"] == i and reg[i] == "minecraft:" + it["name"], (i, it, reg[i])
with open(os.path.join(out, "items.tsv"), "w") as f:
    for it in items:
        f.write(f"{it['name']}\t{it['stackSize']}\n")
print(f"{len(items)} items")
