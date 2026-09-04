#!/usr/bin/env python3
"""Integration tests for the M4 credential-isolating proxy."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import threading
import unittest
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


SCRIPT = Path(__file__).with_name("counting-proxy.py")
SPEC = importlib.util.spec_from_file_location("mimus_counting_proxy", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
PROXY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROXY)


class UpstreamHandler(BaseHTTPRequestHandler):
    authorization = ""
    model = ""

    def log_message(self, _format: str, *_args: object) -> None:
        return

    def do_POST(self) -> None:
        length = int(self.headers["Content-Length"])
        request = json.loads(self.rfile.read(length))
        type(self).authorization = self.headers.get("Authorization", "")
        type(self).model = request["model"]
        body = b'{"id":"response-test","object":"response","output":[]}'
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class CountingProxyTest(unittest.TestCase):
    def test_authentication_rewrite_cap_and_redaction(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            upstream = ThreadingHTTPServer(("127.0.0.1", 0), UpstreamHandler)
            upstream_thread = threading.Thread(target=upstream.serve_forever, daemon=True)
            upstream_thread.start()

            parsed = urllib.parse.urlparse(f"http://127.0.0.1:{upstream.server_port}/v1")
            state = PROXY.State(
                target=(parsed, "/v1/responses"),
                api_key="real-test-credential",
                client_key="ephemeral-client-key",
                model="upstream-model",
                limit=1,
                log_path=root / "proxy.ndjson",
                scan_roots=[root],
            )
            PROXY.Handler.state = state
            server = ThreadingHTTPServer(("127.0.0.1", 0), PROXY.Handler)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            url = f"http://127.0.0.1:{server.server_port}/v1/responses"
            body = json.dumps({"model": "m35-proxy-model", "input": "hello"}).encode()

            try:
                unauthorized = urllib.request.Request(url, data=body, method="POST")
                with self.assertRaises(urllib.error.HTTPError) as rejected:
                    urllib.request.urlopen(unauthorized, timeout=2)
                self.assertEqual(rejected.exception.code, 401)
                rejected.exception.close()
                self.assertEqual(state.forwarded, 0)

                authorized = urllib.request.Request(
                    url,
                    data=body,
                    method="POST",
                    headers={
                        "Authorization": "Bearer ephemeral-client-key",
                        "Content-Type": "application/json",
                    },
                )
                with urllib.request.urlopen(authorized, timeout=2) as response:
                    self.assertEqual(response.status, 200)
                self.assertEqual(UpstreamHandler.authorization, "Bearer real-test-credential")
                self.assertEqual(UpstreamHandler.model, "upstream-model")

                with self.assertRaises(urllib.error.HTTPError) as capped:
                    urllib.request.urlopen(authorized, timeout=2)
                self.assertEqual(capped.exception.code, 429)
                capped.exception.close()
                self.assertEqual(json.loads(state.counts()), {"forwarded": 1, "limit": 1, "rejected": 1})

                log = (root / "proxy.ndjson").read_text(encoding="ascii")
                self.assertNotIn("real-test-credential", log)
                self.assertNotIn("upstream-model", log)
                self.assertTrue(json.loads(state.leak_check())["clean"])
            finally:
                server.shutdown()
                upstream.shutdown()
                thread.join(timeout=2)
                upstream_thread.join(timeout=2)
                server.server_close()
                upstream.server_close()

    def test_upstream_url_validation(self) -> None:
        parsed, target = PROXY.upstream_target("https://example.test/api/v1/responses")
        self.assertEqual(parsed.hostname, "example.test")
        self.assertEqual(target, "/api/v1/responses")
        with self.assertRaises(ValueError):
            PROXY.upstream_target("https://user:secret@example.test/v1")


if __name__ == "__main__":
    unittest.main()
