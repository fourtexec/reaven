#!/usr/bin/env python3
"""Browser check for the viewer page, driven over the Chrome DevTools Protocol.

Assumes the relay, an agent and `reaven screen --bind ...` are already running.
Verifies with a *real* browser and *real* time that:
  * the page renders and its JS executes
  * MJPEG frames actually arrive (frame count > 0)
  * the fps readout reports something non-zero
  * clicking the image posts to /input and the pointer moves on the remote side

Usage: python3 tests/viewer_browser_check.py http://127.0.0.1:8085/ [--display :97]
"""

import json
import os
import subprocess
import sys
import time
import urllib.request

import websocket

URL = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8085/"
DISPLAY = os.environ.get("DISPLAY", ":97")
PORT = 9333
FAILS = []


def check(name, ok, detail=""):
    print(f"  {'PASS' if ok else 'FAIL'} {name}" + (f" — {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


class CDP:
    def __init__(self):
        self.proc = subprocess.Popen(
            ["chromium", "--headless=new", "--no-sandbox", "--disable-gpu",
             f"--remote-debugging-port={PORT}", "--window-size=1280,860",
             "about:blank"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        target = None
        for _ in range(60):
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{PORT}/json") as r:
                    pages = json.load(r)
                target = next(p for p in pages if p["type"] == "page")
                break
            except Exception:
                time.sleep(0.25)
        if not target:
            raise RuntimeError("chromium never exposed a debug target")
        self.ws = websocket.create_connection(target["webSocketDebuggerUrl"], timeout=30)
        self.id = 0

    def call(self, method, params=None):
        self.id += 1
        self.ws.send(json.dumps({"id": self.id, "method": method, "params": params or {}}))
        while True:
            msg = json.loads(self.ws.recv())
            if msg.get("id") == self.id:
                return msg.get("result", {})

    def eval(self, expr):
        res = self.call("Runtime.evaluate",
                        {"expression": expr, "returnByValue": True, "awaitPromise": True})
        return res.get("result", {}).get("value")

    def close(self):
        try:
            self.ws.close()
        finally:
            self.proc.terminate()


def pointer():
    out = subprocess.run(["xdotool", "getmouselocation"], env={**os.environ, "DISPLAY": DISPLAY},
                         capture_output=True, text=True).stdout
    parts = dict(p.split(":") for p in out.split()[:2])
    return int(parts["x"]), int(parts["y"])


def main():
    cdp = CDP()
    try:
        cdp.call("Page.enable")
        cdp.call("Page.navigate", {"url": URL})
        time.sleep(4)  # let frames arrive in real time

        check("page title rendered", cdp.eval("document.title").startswith("pc")
              or "Reaven" in (cdp.eval("document.title") or ""),
              cdp.eval("document.title"))
        check("control button present", cdp.eval("!!document.getElementById('controlToggle')"))
        stats = cdp.eval("document.getElementById('stats').textContent") or ""
        check("stats text populated", "fps" in stats, stats)
        fps = float(stats.split()[0]) if stats and stats.split()[0].replace(".", "").isdigit() else 0.0
        check("fps readout is live", fps > 1.0, f"{fps} fps")
        middle = stats.split("·")[1].strip() if "·" in stats else ""
        check("native frame size detected", middle[:1].isdigit(), middle)
        check("live indicator lit", cdp.eval(
            "document.getElementById('statusDot').classList.contains('live')"))
        w = cdp.eval("document.getElementById('screen').width") or 0
        check("canvas holds a decoded frame", w > 0, f"canvas width {w}")

        # Enable control mode, then click the middle of the image and watch the pointer.
        cdp.eval("document.getElementById('controlToggle').click()")
        time.sleep(0.4)
        check("control mode toggles", cdp.eval(
            "document.getElementById('controlToggle').textContent").endswith("ON"))
        box = cdp.eval(
            "(()=>{const r=document.getElementById('screen').getBoundingClientRect();"
            "return JSON.stringify({x:r.x,y:r.y,w:r.width,h:r.height})})()")
        r = json.loads(box)
        x = r["x"] + r["w"] * 0.75
        y = r["y"] + r["h"] * 0.75
        cdp.call("Input.dispatchMouseEvent", {"type": "mouseMoved", "x": x, "y": y})
        cdp.call("Input.dispatchMouseEvent", {"type": "mousePressed", "x": x, "y": y,
                                              "button": "left", "clickCount": 1})
        cdp.call("Input.dispatchMouseEvent", {"type": "mouseReleased", "x": x, "y": y,
                                              "button": "left", "clickCount": 1})
        time.sleep(1.0)
        px, py = pointer()
        # 1200x700 remote display -> 0.75 maps to ~899,524
        check("browser click moved the remote pointer",
              abs(px - 899) < 25 and abs(py - 524) < 25, f"pointer at {px},{py} (expected ~899,524)")

        # And a keystroke through the page.
        cdp.eval("document.getElementById('textInput').value='hi';")
        cdp.eval("document.getElementById('sendText').click()")
        time.sleep(0.6)
        check("typing path did not throw", cdp.eval("document.getElementById('toast').textContent")
              .startswith("sent"), cdp.eval("document.getElementById('toast').textContent"))

        cdp.call("Page.captureScreenshot", {"format": "png"})
        shot = cdp.call("Page.captureScreenshot", {"format": "png"}).get("data")
        if shot:
            import base64
            with open("docs/viewer-screenshot.png", "wb") as fh:
                fh.write(base64.b64decode(shot))
            print("  saved docs/viewer-screenshot.png")
    finally:
        cdp.close()

    print()
    if FAILS:
        print(f"\033[31m{len(FAILS)} viewer check(s) failed: {FAILS}\033[0m")
        return 1
    print("\033[32mViewer browser check: all passed\033[0m")
    return 0


if __name__ == "__main__":
    sys.exit(main())
