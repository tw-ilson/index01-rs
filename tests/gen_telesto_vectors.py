"""Generate tests/telesto_vectors.json from the REAL libhaversinesatellitelibrary.so.

    cd <repo> && LD_LIBRARY_PATH=shim/lib:aar/jni/x86_64 .venv/bin/python index01-rs/tests/gen_telesto_vectors.py

Covers: TelestoController request encoding + receive state machine (random event scripts),
HaversineAdvertisementData_parseManufacturedData (random + edge inputs), and the
operation-error description table.
"""
import json
import os
import random
import struct
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, ROOT)
from oracle.telesto_oracle import error_descriptions, parse_advertisement, run_telesto  # noqa: E402

rng = random.Random(0x1D01)


def split(b: bytes, max_chunk: int):
    out, i = [], 0
    while i < len(b):
        n = rng.randint(1, max_chunk)
        out.append(b[i:i + n])
        i += n
    return out


def scenario(i):
    op = rng.choice([1, 2, 3, 3, 3, 3])
    addr = rng.choice([0x40030005, 0x40030006, 0x40020000 | rng.randrange(0x10000), 0x40000000,
                       rng.getrandbits(32)])
    offset = rng.choice([0, 0, rng.getrandbits(32)])
    length = rng.choice([0, 4, 6, rng.randrange(0, 300), rng.getrandbits(32)])
    payload = None
    if op == 2:
        length = rng.randrange(0, 200)
        payload = bytes(rng.getrandbits(8) for _ in range(length))
    # Only codes in the description tables: HaversineError_fromOperationCode indexes its table
    # with the raw status and segfaults on anything else (a library bug, not modelled).
    status = rng.choice([0, 0, 0, 0x43, rng.choice(list(range(0, 9)) + list(range(0x40, 0x45)))])
    dlen = rng.choice([0, length & 0xFFFF, rng.randrange(0, 120), length + rng.randrange(-3, 4)]) & 0xFFFFFFFF
    resp = struct.pack("<III", status, rng.getrandbits(32), dlen)
    before = bytes(rng.getrandbits(8) for _ in range(rng.choice([0, 0, rng.randrange(0, 80)])))
    after = bytes(rng.getrandbits(8) for _ in range(rng.choice([0, dlen if dlen < 400 else 0, (dlen + rng.randint(1, 40)) if dlen < 400 else 0, rng.randrange(0, 160)])))
    events = [("data", c) for c in split(before, 20)]
    events += [("ctrl", c) for c in (split(resp, 12) if rng.random() < 0.3 else [resp])]
    events += [("data", c) for c in split(after, 20)]
    if rng.random() < 0.1:  # junk after/around completion
        events.insert(rng.randrange(len(events) + 1), (rng.choice(["ctrl", "data"]), b"\xAA" * rng.randint(1, 13)))
    r = run_telesto(op, addr, offset, length, events, payload)
    return {
        "note": f"scenario {i}", "op": op, "address": addr, "offset": offset, "length": length,
        "payload": payload.hex() if payload is not None else None,
        "events": [[k, d.hex()] for k, d in events],
        "request": r["request"].hex(), "payload_sent": r["payload_sent"].hex(),
        "data": r["data"].hex(), "status": r["status"], "errors": r["errors"],
    }


def adv_vectors():
    raws = [b"", b"\x00", bytes(5), bytes(6), bytes(7), bytes(8), bytes(9), b"\xff" * 6, b"\xff" * 8,
            bytes.fromhex("9f71ee790078"), bytes.fromhex("ea0e9f71ee7903ff")]
    for _ in range(300):
        raws.append(bytes(rng.getrandbits(8) for _ in range(rng.choice([6, 8, 6, 8, rng.randrange(0, 12)]))))
    return [{"raw": r.hex(), **parse_advertisement(r)} for r in raws]


if __name__ == "__main__":
    out = {
        "telesto": [scenario(i) for i in range(600)],
        "advertisement": adv_vectors(),
        "operation_errors": {str(k): v for k, v in error_descriptions().items()},
    }
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "telesto_vectors.json")
    with open(path, "w") as f:
        json.dump(out, f, indent=0)
    t = out["telesto"]
    print(f"wrote {path}: {len(t)} telesto ({sum(v['status'] is not None for v in t)} completed, "
          f"{sum(bool(v['errors']) for v in t)} with controller errors), {len(out['advertisement'])} adverts")
