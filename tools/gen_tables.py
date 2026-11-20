#!/usr/bin/env python3
"""Generate RimeMC's compact protocol tables from the vanilla data-generator reports.

Usage:
    java -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --reports --server --output gen
    python3 tools/gen_tables.py gen/reports crates/rime-server/assets/<version>

Outputs (facts needed for protocol compatibility, no game content):
    blocks.tsv      name <TAB> first_state_id <TAB> default_state_id <TAB> prop=v1,v2;prop2=...
    registries.tsv  registry <TAB> entry names in protocol-id order (space separated)
"""
import json
import os
import sys


def main(reports, out):
    os.makedirs(out, exist_ok=True)
    blocks = json.load(open(os.path.join(reports, "blocks.json")))
    lines = []
    for name, b in blocks.items():
        props = b.get("properties", {})
        states = b["states"]
        # vanilla enumerates states with properties sorted by name
        # (last name varies fastest)
        names = sorted(props)
        first = min(s["id"] for s in states)
        default = next(s["id"] for s in states if s.get("default"))
        # verify the "last property varies fastest" formula for every state
        strides = []
        acc = 1
        for p in reversed(names):
            strides.insert(0, acc)
            acc *= len(props[p])
        assert acc == len(states), name
        for s in states:
            idx = sum(props[p].index(s["properties"][p]) * strides[i] for i, p in enumerate(names))
            assert first + idx == s["id"], (name, s)
        spec = ";".join(f"{p}={','.join(props[p])}" for p in names)
        lines.append(f"{name}\t{first}\t{default}\t{spec}")
    with open(os.path.join(out, "blocks.tsv"), "w") as f:
        f.write("\n".join(lines) + "\n")

    regs = json.load(open(os.path.join(reports, "registries.json")))
    rl = []
    for reg, body in sorted(regs.items()):
        entries = sorted(body["entries"].items(), key=lambda kv: kv[1]["protocol_id"])
        assert [e[1]["protocol_id"] for e in entries] == list(range(len(entries))), reg
        rl.append(reg + "\t" + " ".join(e[0] for e in entries))
    with open(os.path.join(out, "registries.tsv"), "w") as f:
        f.write("\n".join(rl) + "\n")

    packets = json.load(open(os.path.join(reports, "packets.json")))
    pl = []
    for state, dirs in sorted(packets.items()):
        for d, ps in sorted(dirs.items()):
            for n, v in sorted(ps.items(), key=lambda kv: kv[1]["protocol_id"]):
                pl.append(f"{state}\t{d}\t{v['protocol_id']}\t{n}")
    with open(os.path.join(out, "packets.tsv"), "w") as f:
        f.write("\n".join(pl) + "\n")
    print(f"{len(lines)} blocks, {len(rl)} registries, {len(pl)} packets")


# biomes.txt: sorted biome keys (network ids of the data-driven biome registry)
def write_biomes(data_dir, out_dir):
    import os
    d = os.path.join(data_dir, "minecraft", "worldgen", "biome")
    names = sorted("minecraft:" + f[:-5] for f in os.listdir(d) if f.endswith(".json"))
    with open(os.path.join(out_dir, "biomes.txt"), "w") as f:
        f.write("\n".join(names) + "\n")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
    # reports dir is gen/reports; the data pack is next to it in gen/data
    write_biomes(os.path.join(os.path.dirname(os.path.abspath(sys.argv[1])), "data"), sys.argv[2])

