#!/usr/bin/env python3
"""Generate tests/vectors.json for the Rust crate from the REAL libppcommon.so.

Every expected value comes from the closed library via oracle/ppcommon_oracle.py,
except `wav_hex`, which comes from the Python port's to_wav (the WAV writer is app-side
Kotlin logic, not part of the library).

Run from the haversine repo root:
    LD_LIBRARY_PATH=shim/lib .venv/bin/python index01-rs/tests/gen_vectors.py
"""
import json
import os
import random
import struct
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, ROOT)
sys.path.insert(0, os.path.join(ROOT, "tests"))

from index01.collection import to_wav  # noqa: E402
from oracle.ppcommon_oracle import OracleCollection, rice_encode  # noqa: E402
from test_collection import SIMPLE_TYPES, _oracle_fields, _rand_payload, blob, rec  # noqa: E402


def entry(data, note):
    with OracleCollection(data) as oc:
        e = {"note": note, "blob": data.hex(), "parse_err": oc.parse_err}
        if oc.parse_err in (0, 1):
            f = _oracle_fields(oc)
            f["app_data"] = f["app_data"].hex()
            e["fields"] = f
            st, a = oc.audio()
            e["audio"] = a if st == 0 else None
    if e.get("audio") and e["audio"]["samples"]:
        with tempfile.NamedTemporaryFile(suffix=".wav") as t:
            to_wav(e["audio"]["samples"], e["audio"]["sample_rate_hz"] or 16000, t.name)
            e["wav_hex"] = open(t.name, "rb").read().hex()
    return e


def main():
    out = []
    # header / error cases
    body = rec(0x37, struct.pack("<H", 3700))
    for v in ("le", "be", "ff"):
        out.append(entry(blob([body], v), "header " + v))
    out.append(entry(b"\x00\x00", "too short"))
    out.append(entry(b"\x00\x00\x00", "3-byte zero"))
    out.append(entry(b"\x00\x00\x05", "3-byte bad"))
    out.append(entry(b"\x09\x00\x00\x00" + body[:3] + b"\x00\x00", "le mismatch"))
    out.append(entry(blob([rec(0x40, b"ab")]), "unknown type"))
    out.append(entry(blob([body, rec(0x23, b"x"), rec(0x54, b"\x01\x00\x00\x00")]), "terminator"))

    rng = random.Random(1234)
    for i in range(200):
        chosen = rng.sample(SIMPLE_TYPES, rng.randrange(1, 6))
        recs = [rec(t, p) for t in chosen if (p := _rand_payload(rng, t)) is not None]
        if recs:
            out.append(entry(blob(recs, rng.choice(["le", "be", "ff"])), "random %d" % i))

    rng = random.Random(2)
    for i in range(60):
        n = rng.randrange(1, 500)
        style = rng.randrange(3)
        if style == 0:
            samples = [rng.randrange(0x10000) for _ in range(n)]
        elif style == 1:
            samples = [(1000 + j * rng.randrange(1, 9)) & 0xFFFF for j in range(n)]
        else:
            samples = [rng.randrange(0x7F00, 0x8100) for _ in range(n)]
        param = rng.choice([0x00, 0x10, 0x12, 0x21, 0x34, 0x43, 0x51, 0x83, 0x94, 0xEF])
        data, bits = rice_encode(samples, param)
        junk = b"\xff\xff\xff" if i % 5 == 0 else b""
        payload = bytes([param]) + struct.pack("<II", bits, rng.choice([8000, 16000])) + data + junk
        recs = [rec(0x51, payload)]
        if i % 3 == 0:
            recs.append(rec(0x52, struct.pack("<I", i) + bytes([1, i % 2])))
            recs.append(rec(0x53, struct.pack("<II", 0b101, 3)))
        out.append(entry(blob(recs), "rice %d param %#x" % (i, param)))

    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "vectors.json")
    with open(path, "w") as f:
        json.dump(out, f, separators=(",", ":"))
    print("wrote", len(out), "vectors to", path)


main()
