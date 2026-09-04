#!/usr/bin/env python3
"""Serves `web/dist` for a machine you are sitting at, and proxies the one thing that is not a file.

The page fetches `net-config` from its own origin — see web.md §2, and `index.html`, which explains
why it has to happen before the wasm module starts. On a deployment that path is a reverse-proxy
rule; here it is this, so that the local page and the deployed one are the same page.

    ./web/serve.py                 port 8000, metadata from 127.0.0.1:5001
    NOOB_TUBE_PORT=6100 ./web/serve.py

`http://localhost` is a secure context as far as a browser is concerned, so WebTransport works
against a page served over plain HTTP here. Nothing else would.
"""

import http.server
import os
import socketserver
import sys
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.join(HERE, "dist")

# The game port and the metadata port move together — see `follow_the_game_port` in tuning.rs.
GAME_PORT = int(os.environ.get("NOOB_TUBE_PORT", "5000"))
META_PORT = int(os.environ.get("NOOB_TUBE_META_PORT", GAME_PORT + 1))
PORT = int(os.environ.get("NOOB_TUBE_WEB_PORT", "8000"))


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=ROOT, **kwargs)

    def do_GET(self):
        if self.path.split("?")[0].rstrip("/").endswith("net-config"):
            self.serve_config()
            return
        super().do_GET()

    def serve_config(self):
        url = f"http://127.0.0.1:{META_PORT}/"
        try:
            with urllib.request.urlopen(url, timeout=2) as answer:
                body = answer.read()
        except (urllib.error.URLError, OSError) as trouble:
            # 503 rather than 404: the page tells the difference between "this server has no
            # config endpoint" and "the game server is not running", and only one of them is
            # something to fix by starting a process.
            self.send_error(503, f"no metadata from {url}: {trouble}")
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def end_headers(self):
        # Nothing here may be cached: this is a build directory being reloaded after every change.
        if not self.path.split("?")[0].rstrip("/").endswith("net-config"):
            self.send_header("Cache-Control", "no-store")
        super().end_headers()


def main():
    if not os.path.isdir(ROOT):
        sys.exit(f"{ROOT} is not there yet — run ./web/build.sh first")
    socketserver.TCPServer.allow_reuse_address = True
    with socketserver.ThreadingTCPServer(("127.0.0.1", PORT), Handler) as server:
        print(f"serving {ROOT} on http://localhost:{PORT}")
        print(f"net-config proxied from http://127.0.0.1:{META_PORT}/")
        server.serve_forever()


if __name__ == "__main__":
    main()
