#!/usr/bin/env python3
"""Local subprocess integration test; never contacts public test targets."""
import json
import os
from pathlib import Path
import selectors
import subprocess
import sys
import tempfile
import time
import urllib.request

root = Path(__file__).resolve().parent.parent
binary = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else root / "target/debug/spring-net"
with tempfile.TemporaryDirectory(prefix="aonet managed ") as temporary:
    data = Path(temporary)
    auth = data / "separate-auth.json"
    auth.write_text('{"version":1,"targets":[]}')
    env = dict(os.environ, AONET_ROOT=str(root), AONET_DATA_DIR=str(data / "data"),
               AONET_AUTH_FILE=str(auth), AONET_PORT="0", AONET_MANAGED="1",
               AONET_INSTANCE_ID="managed-integration-test")
    child = subprocess.Popen([str(binary)], cwd="/", env=env, stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        selector = selectors.DefaultSelector()
        selector.register(child.stdout, selectors.EVENT_READ)
        assert selector.select(10), "no readiness output"
        line = child.stdout.readline().decode()
        assert line.startswith("AONET_READY "), line
        ready = json.loads(line.removeprefix("AONET_READY "))
        assert ready["protocol"] == 1 and ready["port"] > 0
        origin = f'http://127.0.0.1:{ready["port"]}'
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        def fetch(path, body=None, token=None):
            headers = {}
            if body is not None:
                headers = {"Content-Type": "application/json", "Origin": origin, "X-AoNet-Token": token}
                body = json.dumps(body).encode()
            with opener.open(urllib.request.Request(origin + path, data=body, headers=headers), timeout=5) as response:
                return json.load(response)
        health = fetch("/api/health")
        assert health["ready"] and health["instance_id"] == env["AONET_INSTANCE_ID"]
        bootstrap = fetch("/api/bootstrap")
        assert sum(len(item["targets"]) for item in bootstrap["lists"] if item["builtin"]) == 55
        run = fetch("/api/runs", {"mode": "tcp", "list_ids": [], "target_ids": [], "authorized_ids": ["demo-tcp"],
                    "concurrency": 2, "rps": 10, "duration_secs": 30, "timeout_secs": 3, "ramp_secs": 0,
                    "fresh_connections": False}, bootstrap["token"])
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            run = fetch("/api/state")
            if run["connected"] == 2:
                break
            time.sleep(0.02)
        assert run["connected"] == 2, run
        child.stdin.close()  # Parent death/pipe EOF while a real TCP run is active.
        child.wait(timeout=6)
        assert child.returncode == 0, child.stderr.read().decode()
        print(json.dumps({"managed_ready": True, "dynamic_port": ready["port"], "instance_verified": True,
              "separate_auth_path": True, "targets": 55, "tcp_connected": 2,
              "parent_pipe_eof_exit": child.returncode, "cwd_independent": True}))
    finally:
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
