#!/usr/bin/env python3
"""Measure thread-ownership among wasm-bindgen-ecosystem crates from the crates.io db-dump.

Edge model: use each crate's LATEST non-yanked version's NORMAL (kind=0) deps.
- Ecosystem      = crates depending on wasm-bindgen | js-sys | web-sys
- Thread owners  = crates depending on wasm_thread | wasm-bindgen-rayon | wasm_safe_thread
                   (rayon reported separately — it's the ambiguous one on wasm)
Reports direct ownership and transitive (a thread-owner anywhere in the dep tree).
"""
import csv, os, sys, collections

csv.field_size_limit(1 << 30)
DATA = sys.argv[1]  # dir containing crates.csv, versions.csv, dependencies.csv

ECO_NAMES   = {"wasm-bindgen", "js-sys", "web-sys"}
OWNER_NAMES = {"wasm_thread", "wasm-bindgen-rayon", "wasm_safe_thread"}
RAYON_NAMES = {"rayon"}
WORKER_FEATS = {"Worker", "DedicatedWorkerGlobalScope", "SharedWorker",
                "ServiceWorker", "WorkerGlobalScope"}

def col(path):
    with open(os.path.join(DATA, path), newline='') as f:
        r = csv.DictReader(f)
        for row in r:
            yield row

print("reading crates.csv ...", flush=True)
id2name, name2id = {}, {}
for row in col("crates.csv"):
    id2name[row["id"]] = row["name"]
    name2id[row["name"]] = row["id"]
print(f"  crates: {len(id2name):,}", flush=True)

eco_ids   = {name2id[n] for n in ECO_NAMES   if n in name2id}
owner_ids = {name2id[n] for n in OWNER_NAMES if n in name2id}
rayon_ids = {name2id[n] for n in RAYON_NAMES if n in name2id}
print("  ecosystem anchors:", {id2name[i] for i in eco_ids})
print("  owner anchors:", {id2name[i] for i in owner_ids})

# latest non-yanked version per crate (by created_at)
print("reading versions.csv ...", flush=True)
latest = {}  # crate_id -> (created_at, version_id)
for row in col("versions.csv"):
    if row.get("yanked") in ("t", "true", "1"):
        continue
    cid, vid, ca = row["crate_id"], row["id"], row["created_at"]
    cur = latest.get(cid)
    if cur is None or ca > cur[0]:
        latest[cid] = (ca, vid)
latest_vid2crate = {vid: cid for cid, (_, vid) in latest.items()}
print(f"  crates with a non-yanked version: {len(latest):,}", flush=True)

def parse_feats(s):
    if not s:
        return []
    return [t.strip().strip('"') for t in s.strip("{}").split(",") if t.strip()]

# stream dependencies, keep only edges from latest versions, normal kind
print("reading dependencies.csv (streaming) ...", flush=True)
deps = collections.defaultdict(set)        # from_crate -> set(to_crate)   normal only
worker_feat_users = set()                  # crates requesting web-sys Worker* features
websys_id = name2id.get("web-sys")
n_rows = 0
for row in col("dependencies.csv"):
    n_rows += 1
    if n_rows % 5_000_000 == 0:
        print(f"    ...{n_rows:,} dep rows", flush=True)
    if row["kind"] != "0":                 # 0=normal, 1=build, 2=dev
        continue
    frm = latest_vid2crate.get(row["version_id"])
    if frm is None:
        continue
    to = row["crate_id"]
    deps[frm].add(to)
    if to == websys_id:
        if set(parse_feats(row.get("features", ""))) & WORKER_FEATS:
            worker_feat_users.add(frm)
print(f"  total dep rows: {n_rows:,}; latest-version normal edges from {len(deps):,} crates", flush=True)

# direct sets
ecosystem = {c for c, tos in deps.items() if tos & eco_ids}
direct_owners = {c for c, tos in deps.items() if tos & owner_ids}
direct_rayon  = {c for c, tos in deps.items() if tos & rayon_ids}

# transitive: any thread-owner anywhere in the dep tree (reverse-reachability from owners)
rev = collections.defaultdict(set)
for frm, tos in deps.items():
    for to in tos:
        rev[to].add(frm)
def ancestors(seeds):
    seen, stack = set(), list(seeds)
    while stack:
        n = stack.pop()
        for p in rev.get(n, ()):
            if p not in seen:
                seen.add(p); stack.append(p)
    return seen
trans_owners = ancestors(owner_ids)
trans_owners_with_rayon = ancestors(owner_ids | rayon_ids)

def pct(a, b):
    return f"{(100.0*a/b):.2f}%" if b else "n/a"

E = len(ecosystem)
print("\n" + "="*70)
print("RESULT — thread ownership among the wasm-bindgen ecosystem")
print("="*70)
print(f"wasm-bindgen ecosystem size (deps on wasm-bindgen|js-sys|web-sys): {E:,}")
print()
for nm in OWNER_NAMES | {"rayon"}:
    i = name2id.get(nm)
    tot = len(rev.get(i, ())) if i else 0
    print(f"  reverse-deps of {nm:<20} total={tot:,}")
print()
print(f"ecosystem crates that DIRECTLY own threads "
      f"(wasm_thread|wasm-bindgen-rayon|wasm_safe_thread): "
      f"{len(ecosystem & direct_owners):,}  ({pct(len(ecosystem & direct_owners), E)})")
print(f"  ... including rayon as an owner: "
      f"{len(ecosystem & (direct_owners | direct_rayon)):,}  "
      f"({pct(len(ecosystem & (direct_owners | direct_rayon)), E)})")
print(f"ecosystem crates with a thread-owner ANYWHERE in tree (transitive): "
      f"{len(ecosystem & trans_owners):,}  ({pct(len(ecosystem & trans_owners), E)})")
print(f"  ... including rayon: {len(ecosystem & trans_owners_with_rayon):,}  "
      f"({pct(len(ecosystem & trans_owners_with_rayon), E)})")
print(f"ecosystem crates requesting web-sys Worker* features: "
      f"{len(ecosystem & worker_feat_users):,}  ({pct(len(ecosystem & worker_feat_users), E)})")
print()
print("Direct thread-owning ecosystem crates (sample, up to 60):")
for c in sorted({id2name[c] for c in (ecosystem & (direct_owners | direct_rayon))})[:60]:
    print("   ", c)
