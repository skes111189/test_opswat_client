#!/usr/bin/env python3
"""Local integration fixture implementing the Core subset used by this workspace.

Files are spooled on disk. This is a protocol fixture, not a security scanner.
Names containing blocked/password/original select those outcomes; other names produce
a cleaned output prefixed with MOCK-CLEANED. Password 'demo' unlocks a password fixture.
"""
import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import tempfile
import threading
from urllib.parse import unquote, urlsplit
import uuid


class MockCore(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address, spool, token):
        super().__init__(address, Handler)
        self.spool = Path(spool)
        self.spool.mkdir(parents=True, exist_ok=True)
        self.token = token
        self.files = {}
        self.batches = {}
        self.lock = threading.RLock()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass  # Never log headers, passwords, or arbitrary URL queries.

    def reply(self, code=200, data=None):
        raw = json.dumps(data or {}).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(raw)

    def authenticated(self):
        if self.headers.get("apikey", "") != self.server.token:
            self.reply(401, {"err": "Unauthorized"})
            self.close_connection = True
            return False
        return True

    def consume(self, file, length):
        remaining = length
        while remaining:
            chunk = self.rfile.read(min(65536, remaining))
            if not chunk:
                raise ConnectionError("incomplete request")
            file.write(chunk)
            remaining -= len(chunk)

    def initialize(self, data_id):
        name = unquote(self.headers.get("filename", "file.bin"))
        batch = self.headers.get("batch")
        record = {"name": name, "batch": batch, "password_ok": unquote(self.headers.get("archivepwd", "")) == "demo",
                  "bytes": 0, "ranges": [], "complete": False}
        with self.server.lock:
            self.server.files[data_id] = record
            if batch in self.server.batches:
                self.server.batches[batch]["files"].append(data_id)
        return record

    def finish(self, data_id):
        record = self.server.files[data_id]
        original = self.server.spool / data_id
        with original.open("rb") as source:
            record["sha256"] = hashlib.file_digest(source, "sha256").hexdigest()
        lower = record["name"].lower()
        record["password"] = "password" in lower and not record["password_ok"]
        record["allowed"] = "blocked" not in lower and not record["password"]
        record["has_clean"] = record["allowed"] and "original" not in lower
        if record["has_clean"]:
            with original.open("rb") as src, (self.server.spool / (data_id + ".clean")).open("wb") as dst:
                dst.write(b"MOCK-CLEANED\n")
                shutil.copyfileobj(src, dst, length=65536)
        record["complete"] = True

    def do_POST(self):
        if not self.authenticated():
            return
        path = urlsplit(self.path).path
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if length < 0:
                return self.reply(400)
            if path == "/file/batch":
                data_id = uuid.uuid4().hex
                with self.server.lock:
                    self.server.batches[data_id] = {"files": [], "closed": False}
                return self.reply(data={"batch_id": data_id})
            if path.startswith("/file/batch/") and path.endswith("/close"):
                batch = self.server.batches.get(path.split("/")[3])
                if batch is None:
                    return self.reply(404)
                batch["closed"] = True
                return self.reply()
            if path == "/file/multipart":
                data_id = uuid.uuid4().hex
                record = self.initialize(data_id)
                record["bytes"] = int(self.headers["total-length"])
                (self.server.spool / data_id).touch()
                return self.reply(data={"data_id": data_id})
            if path.startswith("/file/multipart/"):
                data_id = path.rsplit("/", 1)[1]
                record = self.server.files.get(data_id)
                if record is None:
                    return self.reply(404)
                offset_size, total = self.headers["content-range"].split("/")
                offset, size = map(int, offset_size.split("-"))
                if int(total) != record["bytes"] or size != length or offset + size > record["bytes"]:
                    return self.reply(400)
                with self.server.lock:
                    with (self.server.spool / data_id).open("r+b") as output:
                        output.seek(offset)
                        self.consume(output, size)
                    record["ranges"].append((offset, offset + size))
                    covered = 0
                    for start, end in sorted(record["ranges"]):
                        if start > covered:
                            break
                        covered = max(covered, end)
                    if covered == record["bytes"] and not record["complete"]:
                        self.finish(data_id)
                return self.reply(data={"ranges_received": str(record["ranges"])})
            if path == "/file":
                data_id = uuid.uuid4().hex
                self.initialize(data_id)
                with (self.server.spool / data_id).open("wb") as output:
                    self.consume(output, length)
                self.finish(data_id)
                return self.reply(data={"data_id": data_id})
            self.reply(404)
        except (ValueError, KeyError, OSError, ConnectionError):
            self.reply(400, {"err": "Invalid fixture request"})
            self.close_connection = True

    def do_HEAD(self):
        self.do_GET()

    def do_GET(self):
        if not self.authenticated():
            return
        path = urlsplit(self.path).path
        if path == "/readyz":
            return self.reply(data={"status": "healthy"})
        if path == "/file/rules":
            return self.reply(data=[{"name": "Mock workflow"}])
        if path.startswith("/file/batch/"):
            data_id = path.rsplit("/", 1)[1]
            batch = self.server.batches.get(data_id)
            if batch is None:
                return self.reply(404)
            return self.reply(data={"batch_files": {"batch_count": len(batch["files"]),
                "current_finished_files": sum(self.server.files[i]["complete"] for i in batch["files"])}, "is_closed": batch["closed"]})
        if path.startswith("/file/multipart/"):
            record = self.server.files.get(path.rsplit("/", 1)[1])
            if record is None:
                return self.reply(404)
            return self.reply(data={"status": "completed" if record["complete"] else "uploading"})
        if path.startswith("/file/converted/"):
            data_id = path.rsplit("/", 1)[1]
            record = self.server.files.get(data_id)
            if not record or not record.get("has_clean"):
                return self.reply(404)
            path = self.server.spool / (data_id + ".clean")
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(path.stat().st_size))
            self.end_headers()
            if self.command != "HEAD":
                with path.open("rb") as src:
                    shutil.copyfileobj(src, self.wfile, length=65536)
            return
        if path.startswith("/file/"):
            record = self.server.files.get(path.rsplit("/", 1)[1])
            if record is None:
                return self.reply(404)
            return self.reply(data={"process_info": {"result": "Allowed" if record.get("allowed") else "Blocked",
                "progress_percentage": 100 if record["complete"] else 0,
                "post_processing": {"actions_ran": "Sanitized" if record.get("has_clean") else ""}},
                "scan_results": {"progress_percentage": 100 if record["complete"] else 0,
                    "scan_all_result_i": 12 if record.get("password") else 0 if record.get("allowed") else 1},
                "file_info": {"sha256": record.get("sha256", "")}})
        self.reply(404)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=8008)
    parser.add_argument("--spool", type=Path)
    args = parser.parse_args()
    token = os.environ.get("MD_CORE_API_KEY")
    if not token:
        parser.error("Set MD_CORE_API_KEY for the local fixture")
    with tempfile.TemporaryDirectory(prefix="mdcore-fixture-") as temporary:
        server = MockCore(("127.0.0.1", args.port), args.spool or temporary, token)
        print(f"Mock Core (not a scanner): http://127.0.0.1:{server.server_port}", flush=True)
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass
        finally:
            server.server_close()


if __name__ == "__main__":
    main()
