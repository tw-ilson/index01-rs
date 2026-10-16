#!/usr/bin/env python3
"""Example --backend command: transcribe the WAV in argv[1] with faster-whisper.

    index01 watch <addr> --backend command --command "python3 examples/backends/fw_transcribe.py"

Env: FW_MODEL (default "medium"), FW_DEVICE (default "auto"), FW_LANGUAGE (optional).
"""
import os
import sys

from faster_whisper import WhisperModel

model = WhisperModel(os.environ.get("FW_MODEL", "medium"), device=os.environ.get("FW_DEVICE", "auto"))
segments, _ = model.transcribe(sys.argv[1], language=os.environ.get("FW_LANGUAGE") or None, vad_filter=True)
print(" ".join(s.text.strip() for s in segments))
