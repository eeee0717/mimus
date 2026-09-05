#!/usr/bin/env python3
"""Credential-isolating, call-counting proxy for the M4 release rehearsal."""

from __future__ import annotations

import argparse
import hmac
import http.client
import json
import os
import secrets
import ssl
import threading
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


HOP_BY_HOP = {
    "authorization",
    "api-key",
    "connection",
    "content-length",
    "host",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
}
MAX_REQUEST_BYTES = 64 * 1024 * 1024


def load_credentials(path: Path) -> tuple[str, str, str]:
    values: dict[str, str] = {}
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        values[key.strip()] = value
    required = ("BASE_URL", "API_KEY", "MODEL_ID")
    if any(not values.get(key) for key in required):
        raise ValueError("credential file is missing a required value")
    return values["BASE_URL"], values["API_KEY"], values["MODEL_ID"]


def upstream_target(base_url: str) -> tuple[urllib.parse.ParseResult, str]:
    parsed = urllib.parse.urlparse(base_url)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise ValueError("BASE_URL must be an absolute HTTP(S) URL")
    if parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError("BASE_URL must not contain credentials, query, or fragment")
    path = parsed.path.rstrip("/")
    if path.endswith("/responses"):
        path = path[: -len("/responses")]
    if not path:
        path = "/v1"
    elif not path.endswith("/v1"):
        path += "/v1"
    return parsed, f"{path}/responses"


def file_contains(path: Path, needle: bytes) -> bool:
    overlap = max(0, len(needle) - 1)
    tail = b""
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            value = tail + chunk
            if needle in value:
                return True
            tail = value[-overlap:] if overlap else b""
    return False


class State:
    def __init__(
        self,
        *,
        target: tuple[urllib.parse.ParseResult, str],
        api_key: str,
        client_key: str,
        model: str,
        limit: int,
        log_path: Path,
        scan_roots: list[Path],
    ) -> None:
        self.target = target
        self.api_key = api_key
        self.client_key = client_key
        self.model = model
        self.limit = limit
        self.log_path = log_path
        self.scan_roots = [path.resolve() for path in scan_roots]
        self.lock = threading.Lock()
        self.forwarded = 0
        self.rejected = 0
        log_path.parent.mkdir(parents=True, exist_ok=True)
        log_path.unlink(missing_ok=True)

    def reserve(self) -> int | None:
        with self.lock:
            if self.forwarded >= self.limit:
                self.rejected += 1
                return None
            self.forwarded += 1
            return self.forwarded

    def record(self, entry: dict[str, int | str]) -> None:
        with self.lock, self.log_path.open("a", encoding="ascii") as handle:
            handle.write(json.dumps(entry, sort_keys=True, separators=(",", ":")) + "\n")
            handle.flush()
            os.fsync(handle.fileno())

    def counts(self) -> bytes:
        with self.lock:
            payload = {
                "forwarded": self.forwarded,
                "limit": self.limit,
                "rejected": self.rejected,
            }
        return json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("ascii")

    def leak_check(self) -> bytes:
        needle = self.api_key.encode("utf-8")
        matches: list[str] = []
        errors: list[str] = []
        files_scanned = 0
        for root in self.scan_roots:
            paths = [root] if root.is_file() else root.rglob("*")
            for path in paths:
                try:
                    if not path.is_file():
                        continue
                    files_scanned += 1
                    if file_contains(path, needle):
                        matches.append(str(path))
                except OSError:
                    errors.append(str(path))
        payload = {
            "clean": not matches and not errors,
            "errors": sorted(errors),
            "files_scanned": files_scanned,
            "matches": sorted(matches),
        }
        return json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("ascii")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    state: State

    def log_message(self, _format: str, *_args: object) -> None:
        return

    def send_bytes(self, status: int, body: bytes, content_type: str = "application/json") -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)

    def authorized(self) -> bool:
        expected = f"Bearer {self.state.client_key}"
        provided = self.headers.get("Authorization", "")
        return hmac.compare_digest(provided, expected)

    def do_GET(self) -> None:
        if not self.authorized():
            self.send_bytes(401, b'{"error":{"message":"unauthorized"}}')
            return
        if self.path == "/count":
            self.send_bytes(200, self.state.counts())
        elif self.path == "/leak-check":
            self.send_bytes(200, self.state.leak_check())
        elif self.path == "/healthz":
            self.send_bytes(200, b'{"ready":true}')
        else:
            self.send_bytes(404, b"not found\n", "text/plain")

    def do_POST(self) -> None:
        if not self.authorized():
            self.send_bytes(401, b'{"error":{"message":"unauthorized"}}')
            return
        if self.path.rstrip("/") != "/v1/responses":
            self.send_bytes(404, b"not found\n", "text/plain")
            return
        length = 0
        call = 0
        recorded = False
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if length <= 0 or length > MAX_REQUEST_BYTES:
                self.send_bytes(413, b'{"error":{"message":"invalid request size"}}')
                return
            request = json.loads(self.rfile.read(length))
            request["model"] = self.state.model
            body = json.dumps(request, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
            reserved = self.state.reserve()
            if reserved is None:
                self.send_bytes(429, b'{"error":{"message":"M4 call budget exhausted"}}')
                return
            call = reserved
            parsed, target = self.state.target
            headers = {
                key: value
                for key, value in self.headers.items()
                if key.lower() not in HOP_BY_HOP
            }
            headers["Authorization"] = f"Bearer {self.state.api_key}"
            connection_type = (
                http.client.HTTPSConnection
                if parsed.scheme == "https"
                else http.client.HTTPConnection
            )
            kwargs: dict[str, object] = {"timeout": 600}
            if parsed.scheme == "https":
                kwargs["context"] = ssl.create_default_context()
            connection = connection_type(parsed.hostname, parsed.port, **kwargs)
            connection.request("POST", target, body=body, headers=headers)
            response = connection.getresponse()
            response_body = response.read()
            response_headers = [
                (key, value)
                for key, value in response.getheaders()
                if key.lower() not in HOP_BY_HOP
            ]
            self.state.record(
                {
                    "call": call,
                    "request_bytes": len(body),
                    "response_bytes": len(response_body),
                    "status": response.status,
                }
            )
            recorded = True
            self.send_response(response.status)
            for key, value in response_headers:
                self.send_header(key, value)
            self.send_header("Content-Length", str(len(response_body)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(response_body)
            connection.close()
        except Exception:
            if call and not recorded:
                self.state.record(
                    {
                        "call": call,
                        "request_bytes": length,
                        "response_bytes": 0,
                        "status": "proxy_error",
                    }
                )
            try:
                self.send_bytes(502, b'{"error":{"message":"M4 upstream proxy failure"}}')
            except OSError:
                pass


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--env-file", type=Path, required=True)
    parser.add_argument("--limit", type=int, default=300)
    parser.add_argument("--log", type=Path, required=True)
    parser.add_argument("--port-file", type=Path, required=True)
    parser.add_argument("--client-key-file", type=Path, required=True)
    parser.add_argument("--scan-root", type=Path, action="append", default=[])
    parser.add_argument("--bind", default="127.0.0.1")
    args = parser.parse_args()
    if args.limit <= 0 or args.limit > 300:
        raise ValueError("limit must be from 1 through 300")
    base_url, api_key, model = load_credentials(args.env_file)
    client_key = secrets.token_urlsafe(32)
    state = State(
        target=upstream_target(base_url),
        api_key=api_key,
        client_key=client_key,
        model=model,
        limit=args.limit,
        log_path=args.log,
        scan_roots=args.scan_root,
    )
    Handler.state = state
    server = ThreadingHTTPServer((args.bind, 0), Handler)
    args.port_file.write_text(f"{server.server_port}\n", encoding="ascii")
    os.chmod(args.port_file, 0o600)
    args.client_key_file.write_text(f"{client_key}\n", encoding="ascii")
    os.chmod(args.client_key_file, 0o600)
    print(f"READY {server.server_port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
