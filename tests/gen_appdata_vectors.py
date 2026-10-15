"""Generate tests/appdata_vectors.json from the REAL libppcommon (oracle/appdata.py).

    cd <repo> && LD_LIBRARY_PATH=shim/lib .venv/bin/python index01-rs/tests/gen_appdata_vectors.py
"""
import json
import os
import random
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, ROOT)
from oracle.appdata import build  # noqa: E402

rng = random.Random(7)
ALPHA = "abcdefghijklmnopqrstuvwxyz0123456789-_@.ABC"
ids = ["", "a", "tom-pc", "x" * 128] + [
    "".join(rng.choice(ALPHA) for _ in range(rng.randint(1, 128))) for _ in range(60)
]
out = []
for u in ids:
    ts = rng.randrange(1 << 32)
    blob, fp = build(u, ts)
    out.append({"user_id": u, "unix_time": ts, "fingerprint": fp, "blob": blob.hex()})
json.dump(out, open(os.path.join(os.path.dirname(__file__), "appdata_vectors.json"), "w"), indent=0)
print(len(out), "vectors")
