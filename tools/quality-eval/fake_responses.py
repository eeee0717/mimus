#!/usr/bin/env python3
"""Loopback-only deterministic Responses server for offline quality evaluation."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


HAN_SAMPLE = "模型数据证论文翻译语结果保持结构程稳定缓存重试诊断排版字体"
PLACEHOLDER_PREFIXES = ("<b", "</b", "{v", "{l")
UNITS = (
    "GHz", "MHz", "kHz", "dpi", "bits", "bytes", "bit", "byte", "km", "cm", "mm",
    "ms", "Hz", "KB", "MB", "GB", "TB", "mV", "mA", "kW", "dB", "px", "pt", "°C",
    "min", "s", "h", "m", "K", "V", "A", "W",
)
NUMBER = re.compile(r"[+\-−]?\d+(?:\.\d+)?(?:[eE][+\-−]?\d+)?%?")


def deterministic_translation(value: str) -> str:
    """Legacy fake translation. Keep this function byte-for-byte behavior compatible."""
    seed = 2_166_136_261
    for byte in value.encode("utf-8"):
        seed = ((seed ^ byte) * 16_777_619) & ((1 << 64) - 1)

    output: list[str] = []
    rest = value
    emitted_text = False
    segment_index = 0
    while rest:
        marker = None
        for prefix, suffix in (("<b", ">"), ("</b", ">"), ("{v", "}"), ("{l", "}")):
            if not rest.startswith(prefix):
                continue
            end = rest.find(suffix)
            if end < 0:
                continue
            candidate = rest[: end + 1]
            index = candidate[len(prefix) : -1]
            if index.isdigit() and int(index) > 0:
                marker = candidate
                break
        if marker is not None:
            output.append(marker)
            rest = rest[len(marker) :]
            continue

        positions = [
            position
            for prefix in PLACEHOLDER_PREFIXES
            if (position := rest.find(prefix)) >= 0
        ]
        next_marker = min(positions, default=len(rest))
        segment = rest[: max(next_marker, 1)]
        source_characters = sum(not character.isspace() for character in segment)
        if source_characters:
            output_characters = min(max(source_characters // 2, 1), 6)
            for offset in range(output_characters):
                output.append(HAN_SAMPLE[(seed + segment_index * 7 + offset * 5) % len(HAN_SAMPLE)])
            emitted_text = True
            segment_index += 1
        rest = rest[len(segment) :]

    if not emitted_text and not output:
        output.append(HAN_SAMPLE[seed % len(HAN_SAMPLE)])
    return "".join(output)


def conserving_translation(value: str) -> str:
    """Replace prose letters while preserving every mechanically conserved token."""
    output: list[str] = []
    index = 0
    while index < len(value):
        marker = _placeholder_at(value, index)
        if marker is not None:
            output.append(marker)
            index += len(marker)
            continue
        number = _number_at(value, index)
        if number is not None:
            output.append(number)
            index += len(number)
            continue
        unit = _unit_at(value, index)
        if unit is not None:
            output.append(unit)
            index += len(unit)
            continue
        character = value[index]
        if character.isalpha() and not _is_han(character):
            output.append(HAN_SAMPLE[ord(character) % len(HAN_SAMPLE)])
        else:
            output.append(character)
        index += 1
    return "".join(output)


def _number_at(value: str, index: int) -> str | None:
    match = NUMBER.match(value, index)
    return match.group(0) if match is not None else None


def _placeholder_at(value: str, index: int) -> str | None:
    rest = value[index:]
    for prefix, suffix in (("<b", ">"), ("</b", ">"), ("{v", "}"), ("{l", "}")):
        if not rest.startswith(prefix):
            continue
        end = rest.find(suffix)
        if end < 0:
            continue
        candidate = rest[: end + 1]
        number = candidate[len(prefix) : -1]
        if number.isdigit() and int(number) > 0:
            return candidate
    return None


def _unit_at(value: str, index: int) -> str | None:
    if index > 0 and value[index - 1].isalpha():
        return None
    previous = index - 1
    while previous >= 0 and value[previous].isspace():
        previous -= 1
    if previous < 0 or not (value[previous].isdigit() or value[previous] in ".%"):
        return None
    rest = value[index:]
    for unit in UNITS:
        if rest.startswith(unit) and (len(rest) == len(unit) or not rest[len(unit)].isalpha()):
            return unit
    return None


def _is_han(character: str) -> bool:
    return "\u3400" <= character <= "\u4dbf" or "\u4e00" <= character <= "\u9fff"


class State:
    def __init__(self, log_path: Path, mode: str) -> None:
        self.log_path = log_path
        self.mode = mode
        self.lock = threading.Lock()
        self.calls = 0
        self.term_calls = 0

    def record(self, payload: dict[str, object], output: str, term_call: bool) -> None:
        request_input = str(payload.get("input", ""))
        with self.lock:
            self.calls += 1
            self.term_calls += int(term_call)
            entry = {
                "call": self.calls,
                "input_characters": len(request_input),
                "input_sha256": hashlib.sha256(request_input.encode()).hexdigest(),
                "model": str(payload.get("model", "")),
                "output_characters": len(output),
                "output_sha256": hashlib.sha256(output.encode()).hexdigest(),
                "term_extraction": term_call,
            }
            with self.log_path.open("a", encoding="ascii") as handle:
                handle.write(json.dumps(entry, sort_keys=True, separators=(",", ":")) + "\n")
                handle.flush()
                os.fsync(handle.fileno())

    def snapshot(self) -> bytes:
        with self.lock:
            value = {"calls": self.calls, "term_calls": self.term_calls}
        return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("ascii")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    state: State

    def log_message(self, _format: str, *_args: object) -> None:
        return

    def send_bytes(self, status: int, value: bytes, content_type: str) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(value)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(value)

    def do_GET(self) -> None:
        if self.path == "/count":
            self.send_bytes(200, self.state.snapshot(), "application/json")
        else:
            self.send_bytes(404, b"not found\n", "text/plain")

    def do_POST(self) -> None:
        if self.path != "/v1/responses":
            self.send_bytes(404, b"not found\n", "text/plain")
            return
        length = int(self.headers.get("Content-Length", "0"))
        payload = json.loads(self.rfile.read(length))
        instructions = str(payload.get("instructions", ""))
        request_input = str(payload.get("input", ""))
        term_call = "Extract important technical terms" in instructions
        if term_call:
            output = '{"terms":[]}'
        elif self.state.mode == "conserving":
            output = conserving_translation(request_input)
        else:
            output = deterministic_translation(request_input)
        self.state.record(payload, output, term_call)
        body = json.dumps({"output_text": output}, ensure_ascii=False, separators=(",", ":")).encode()
        self.send_bytes(200, body, "application/json")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--log", type=Path, required=True)
    parser.add_argument("--port-file", type=Path, required=True)
    parser.add_argument("--mode", choices=("legacy", "conserving"), default="legacy")
    args = parser.parse_args()
    args.log.unlink(missing_ok=True)
    state = State(args.log, args.mode)
    Handler.state = state
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    args.port_file.write_text(f"{server.server_port}\n", encoding="ascii")
    os.chmod(args.port_file, 0o600)
    print(f"READY {server.server_port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
