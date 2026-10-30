#!/usr/bin/env python3
"""Simulates the Android client's wire traffic against a live relay + agent.

The Kotlin app in `android/` builds JSON exactly like this script
(see `data/Protocol.kt`). Running this in CI verifies that the two
implementations agree on the protocol without needing an emulator.

Usage:
    xvfb-run -a tests/android_client_sim.py ./target/debug/reaven
"""

import base64
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import uuid

BIN = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "./target/debug/reaven")
PORT = int(os.environ.get("REAVEN_TEST_PORT", "17332"))
SERVER = ("127.0.0.1", PORT)
PASSWORD = "sim-secret"

FAILS = []


def check(name, ok, detail=""):
    print(f"  {'PASS' if ok else 'FAIL'} {name}" + (f" — {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


class Client:
    """Mirrors dev.reaven.control.data.ReavenClient."""

    def __init__(self, host, port, password, name="android"):
        self.sock = socket.create_connection((host, port), timeout=10)
        self.sock.settimeout(20)
        self.buf = b""
        self.send({"type": "hello", "name": name, "password": password,
                   "kind": "operator", "info": None})
        frame = self.recv()
        assert frame.get("type") == "welcome", f"expected welcome, got {frame}"

    def send(self, obj):
        self.sock.sendall(json.dumps(obj).encode() + b"\n")

    def recv(self):
        while b"\n" not in self.buf:
            chunk = self.sock.recv(1 << 16)
            if not chunk:
                raise ConnectionError("relay closed the connection")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def recv_matching(self, predicate, timeout=15):
        deadline = time.time() + timeout
        while time.time() < deadline:
            frame = self.recv()
            if predicate(frame):
                return frame
        raise TimeoutError("no matching frame")

    # ---- mirror of Protocol.kt builders ----

    def list_peers(self):
        rid = str(uuid.uuid4())
        self.send({"type": "list_peers", "id": rid, "from": ""})
        return self.recv_matching(
            lambda f: f.get("type") == "peer_list" and f.get("id") == rid
        )["peers"]

    def request(self, target, op):
        rid = str(uuid.uuid4())
        self.send({"type": "request", "id": rid, "from": "",
                   "target": target, "op": op})
        frame = self.recv_matching(
            lambda f: f.get("type") == "response" and f.get("id") == rid
        )
        result = frame.get("result")
        if not isinstance(result, dict):
            raise RuntimeError(f"remote error: {result}")
        return result

    def start_video(self, target, quality=70, fps=12, max_width=1600, camera=False):
        sid = str(uuid.uuid4())
        source = {"kind": "camera", "index": 0} if camera else {"kind": "screen", "monitor": 0}
        self.send({"type": "video_start", "id": sid, "from": "", "target": target,
                   "source": source, "max_width": max_width,
                   "quality": quality, "fps": fps})
        return sid

    def stop_video(self, sid, target):
        self.send({"type": "video_stop", "id": sid, "from": "", "to": target})

    def control(self, target, stream, events):
        cid = str(uuid.uuid4())
        self.send({"type": "control", "id": cid, "from": "", "target": target,
                   "stream": stream, "events": events})
        return self.recv_matching(
            lambda f: f.get("type") == "response" and f.get("id") == cid
        )


def main():
    workdir = tempfile.mkdtemp()
    procs = []
    try:
        print("starting relay + agent")
        procs.append(subprocess.Popen(
            [BIN, "server", "--listen", f"127.0.0.1:{PORT}", "--password", PASSWORD],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        wait_port()
        procs.append(subprocess.Popen(
            [BIN, "connect", "--server", f"127.0.0.1:{PORT}", "--name", "testpc"],
            env={**os.environ, "REAVEN_PASSWORD": PASSWORD},
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        time.sleep(1.2)

        print("\nhandshake")
        client = Client(*SERVER, PASSWORD)
        check("hello/welcome handshake", True)

        print("\ndiscovery")
        peers = client.list_peers()
        names = [p["name"] for p in peers]
        check("peer_list returns the agent", "testpc" in names, str(names))
        check("peer_list excludes operators",
              not any(n.startswith("operator") for n in names), str(names))
        check("peer_list carries metadata",
              bool(peers[0].get("hostname")) and bool(peers[0].get("version")),
              str(peers[0]))

        print("\nscreenshot (one-shot request/response)")
        payload = client.request("testpc", {"op": "screenshot", "monitor": 0,
                                            "quality": 85, "max_width": 800})
        check("payload kind is image", payload.get("kind") == "image", payload.get("kind"))
        jpeg = base64.b64decode(payload["data"])
        check("decodes to a JPEG", jpeg[:2] == b"\xff\xd8", f"{len(jpeg)} bytes")
        check("reports dimensions", payload.get("width") == 800 and payload.get("height", 0) > 0,
              f"{payload.get('width')}x{payload.get('height')}")

        print("\nscreen enumeration")
        screens = json.loads(client.request("testpc", {"op": "list_screens"})["data"])
        check("list_screens returns a monitor", len(screens) >= 1,
              f"{screens[0]['width']}x{screens[0]['height']}" if screens else "none")

        print("\nlive stream")
        sid = client.start_video("testpc", quality=60, fps=15, max_width=960)
        frames = []
        deadline = time.time() + 10
        while len(frames) < 4 and time.time() < deadline:
            frame = client.recv()
            if frame.get("type") == "video_frame" and frame.get("id") == sid:
                frames.append(frame)
        check("received >=4 video frames", len(frames) >= 4, f"{len(frames)} frames")
        if frames:
            first = frames[0]
            check("frame decodes to JPEG",
                  base64.b64decode(first["data"])[:2] == b"\xff\xd8")
            check("frame honours max_width", first["width"] == 960, str(first["width"]))
            check("seq increments",
                  [f["seq"] for f in frames] == sorted(f["seq"] for f in frames))

        print("\ninput injection (the phone's tap)")
        resp = client.control("testpc", sid, [
            {"event": "click", "button": "left", "x": 0.5, "y": 0.5}
        ])
        check("control acknowledged", isinstance(resp.get("result"), dict), str(resp.get("result")))
        time.sleep(0.4)
        loc = subprocess.run(["xdotool", "getmouselocation"],
                             capture_output=True, text=True).stdout
        check("pointer landed mid-screen", "x:720" in loc or "x:719" in loc, loc.strip())

        resp = client.control("testpc", sid, [
            {"event": "down", "button": "left", "x": 0.1, "y": 0.1},
            {"event": "move", "x": 0.9, "y": 0.9},
            {"event": "up", "button": "left", "x": 0.9, "y": 0.9},
        ])
        check("drag sequence accepted", isinstance(resp.get("result"), dict))
        resp = client.control("testpc", sid, [{"event": "combo", "keys": ["ctrl", "alt", "t"]}])
        check("key combo accepted", isinstance(resp.get("result"), dict))
        resp = client.control("testpc", sid, [{"event": "text", "text": "hello"}])
        check("text event accepted", isinstance(resp.get("result"), dict))
        resp = client.control("testpc", sid, [{"event": "scroll", "x": 0.5, "y": 0.5,
                                               "dx": 0, "dy": 2}])
        check("scroll accepted", isinstance(resp.get("result"), dict))

        print("\nbad input is rejected, not crashed on")
        resp = client.control("testpc", sid, [{"event": "key", "key": "notakey", "pressed": True}])
        check("unknown key rejected cleanly", isinstance(resp.get("result"), str),
              str(resp.get("result"))[:60])

        print("\nstop stream")
        client.stop_video(sid, "testpc")
        stop = client.recv_matching(
            lambda f: f.get("type") in ("video_stop", "video_error") and f.get("id") == sid
        )
        check("agent confirms the stop", stop.get("type") == "video_stop", str(stop.get("type")))

        print("\nconsole command")
        out = client.request("testpc", {"op": "exec", "command": "echo sim-$((1+1))"})
        check("exec returns stdout", out.get("stdout", "").strip() == "sim-2", repr(out.get("stdout")))

        print("\ncamera when none is attached")
        cams = json.loads(client.request("testpc", {"op": "list_cameras"})["data"])
        check("empty camera list handled", isinstance(cams, list), f"{len(cams)} cameras")

    finally:
        for p in procs:
            p.terminate()

    print()
    if FAILS:
        print(f"\033[31m{len(FAILS)} check(s) failed: {FAILS}\033[0m")
        return 1
    print("\033[32mAndroid client protocol simulation: all checks passed\033[0m")
    return 0


def wait_port(timeout=15):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            socket.create_connection(SERVER, timeout=0.4).close()
            return
        except OSError:
            time.sleep(0.2)
    raise TimeoutError("relay never opened its port")


if __name__ == "__main__":
    sys.exit(main())
