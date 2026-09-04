#!/usr/bin/env python3
"""Serves `web/dist` for a machine you are sitting at, and proxies the one thing that is not a file.

The page fetches `net-config` from its own origin — see `index.html`, which explains
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
    # `WebAssembly.instantiateStreaming` refuses anything that is not `application/wasm`, and
    # Python's table does not know the type. The generated glue falls back to fetching the whole
    # body and compiling that, so the wrong type is a warning and a slower load rather than a
    # failure — which is exactly the kind of thing that is never noticed and always paid for.
    extensions_map = {
        **http.server.SimpleHTTPRequestHandler.extensions_map,
        ".wasm": "application/wasm",
    }

    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=ROOT, **kwargs)

    def do_POST(self):
        """Takes the console lines a `?log` page posts back, and prints them.

        Nothing else on this server accepts a POST, and nothing on a deployment's static host
        would either — the page treats a refusal as "nobody is listening" and carries on. See
        `index.html`.
        """
        if not self.path.split("?")[0].rstrip("/").endswith("log"):
            self.send_error(404)
            return
        length = int(self.headers.get("Content-Length", "0"))
        for line in self.rfile.read(length).decode("utf-8", "replace").splitlines():
            print(f"  browser | {line}", flush=True)
        self.send_response(204)
        self.end_headers()

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

    def log_message(self, format, *args):
        # One line per asset request is hundreds of lines of nothing while the game loads, and it
        # buries what the page is posting back. Failures still say so.
        if isinstance(args[0], str) and args[0].startswith(("GET", "POST")) and "20" in str(args[1]):
            return
        super().log_message(format, *args)

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
