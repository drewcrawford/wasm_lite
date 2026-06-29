#!/usr/bin/env python3
"""Enumerate the wasm-bindgen-ecosystem crates that DIRECTLY own wasm threads
(depend on wasm_thread | wasm-bindgen-rayon | wasm_safe_thread), latest non-yanked
version, normal deps only. Prints each crate + which threading crate(s) it pulls in
+ whether it also requests web-sys Worker* features."""
import csv, os, sys, collections
csv.field_size_limit(1 << 30)
DATA = sys.argv[1]

ECO   = {"wasm-bindgen", "js-sys", "web-sys"}
OWNER = {"wasm_thread", "wasm-bindgen-rayon", "wasm_safe_thread"}
WORKER_FEATS = {"Worker", "DedicatedWorkerGlobalScope", "SharedWorker",
                "ServiceWorker", "WorkerGlobalScope"}

def rows(p):
    with open(os.path.join(DATA, p), newline='') as f:
        for r in csv.DictReader(f):
            yield r

id2name, name2id = {}, {}
for r in rows("crates.csv"):
    id2name[r["id"]] = r["name"]; name2id[r["name"]] = r["id"]
eco_ids   = {name2id[n] for n in ECO   if n in name2id}
owner_ids = {name2id[n]: n for n in OWNER if n in name2id}
websys_id = name2id.get("web-sys")

latest = {}
for r in rows("versions.csv"):
    if r.get("yanked") in ("t", "true", "1"):
        continue
    cid, vid, ca = r["crate_id"], r["id"], r["created_at"]
    if cid not in latest or ca > latest[cid][0]:
        latest[cid] = (ca, vid)
vid2crate = {vid: cid for cid, (_, vid) in latest.items()}

is_eco = collections.defaultdict(bool)
owners = collections.defaultdict(set)   # crate -> {owner names}
worker = collections.defaultdict(bool)

def feats(s):
    return [t.strip().strip('"') for t in (s or "").strip("{}").split(",") if t.strip()]

for r in rows("dependencies.csv"):
    if r["kind"] != "0":
        continue
    frm = vid2crate.get(r["version_id"])
    if frm is None:
        continue
    to = r["crate_id"]
    if to in eco_ids:
        is_eco[frm] = True
    if to in owner_ids:
        owners[frm].add(owner_ids[to])
    if to == websys_id and (set(feats(r.get("features", ""))) & WORKER_FEATS):
        worker[frm] = True

result = sorted((id2name[c] for c in owners if is_eco[c]), key=str.lower)
print(f"# Direct wasm-thread-owning wasm-bindgen-ecosystem crates: {len(result)}\n")
print(f"{'crate':<32} {'threading dep(s)':<40} worker-feat")
print("-"*86)
for c in result:
    cid = name2id[c]
    print(f"{c:<32} {','.join(sorted(owners[cid])):<40} {'yes' if worker[cid] else ''}")
