#!/usr/bin/env python3
"""GUI check (password lead): the "Allow Agents in This Tab…" sheet and an
openBrowser profile "agent" tab, on a tagged cmux-next build. Writes PNGs and
a JSON report to $NX_ARTIFACTS. Never touches other apps; kills only the PID
it starts. Usage: pw-gui-check.py --tag <tag>"""
import argparse, glob, json, os, subprocess, sys, time

p = argparse.ArgumentParser(); p.add_argument("--tag", required=True); p.add_argument("--app"); o = p.parse_args()
OUT = os.environ.get("NX_ARTIFACTS", "/tmp/pw-gui"); os.makedirs(OUT, exist_ok=True)
SOCKET = f"/tmp/cmux-debug-{o.tag}.sock"
apps = [o.app] if o.app else sorted(glob.glob(os.path.expanduser(f"~/Library/Developer/Xcode/DerivedData/cmux-{o.tag}/Build/Products/Debug/*.app")))
if not apps: sys.exit(f"no tagged app for {o.tag}")
APP = apps[0]
CLI = os.path.join(APP, "Contents/Resources/bin/cmux")
import plistlib
with open(os.path.join(APP, "Contents/Info.plist"), "rb") as f:
    exe = os.path.join(APP, "Contents/MacOS", plistlib.load(f)["CFBundleExecutable"])
ENV = {"HOME": os.environ["HOME"], "USER": os.environ.get("USER", ""), "PATH": "/usr/bin:/bin",
       "TMPDIR": os.environ.get("TMPDIR", "/tmp"), "CMUX_NEXT_NO_ACTIVATE": "1", "CMUX_NEXT_SOCKET_MODE": "automation"}
report = {}

import socket, itertools
_ids = itertools.count(1)

def rpc(method, params=None):
    """One JSON Lines request on the tagged app's control socket."""
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as c:
            c.settimeout(30)
            c.connect(SOCKET)
            c.sendall((json.dumps({"id": next(_ids), "method": method, "params": params or {}}) + "\n").encode())
            buf = b""
            while not buf.endswith(b"\n"):
                chunk = c.recv(65536)
                if not chunk: break
                buf += chunk
        reply = json.loads(buf.decode().splitlines()[0])
        return reply.get("result") if reply.get("ok") else {"error": reply.get("error")}
    except Exception as e:
        return {"raw": "", "err": repr(e)}


PROBE = """(() => { window.__cmuxProbe = {pending: true}; (async () => {
  const out = {origin: location.origin, PKC: typeof PublicKeyCredential};
  try { out.uvpaa = await PublicKeyCredential.isUserVerifyingPlatformAuthenticatorAvailable(); } catch (e) { out.uvpaa = 'throw ' + e.name; }
  try { out.conditional = await PublicKeyCredential.isConditionalMediationAvailable(); } catch (e) { out.conditional = 'throw ' + e.name; }
  try { out.caps = typeof PublicKeyCredential.getClientCapabilities === 'function' ? await PublicKeyCredential.getClientCapabilities() : 'missing'; } catch (e) { out.caps = 'throw ' + e.name; }
  out.parseJSON = typeof PublicKeyCredential.parseCreationOptionsFromJSON;
  out.signal = typeof PublicKeyCredential.signalUnknownCredential;
  out.ua = navigator.userAgent;
  window.__cmuxProbe = out; })(); return 'started'; })()"""

seen_github = set()

def probe(label, engine):
    r = {"open": rpc("action.run", {"action": "openBrowser", "args": {"url": "https://github.com/login", "engine": engine}})}
    time.sleep(8)
    # Select the new github tab as a person would: Ctrl-Tab until the focused page is github.
    r["focus"] = []
    for _ in range(12):
        st = rpc("browser.page.state", {})
        r["focus"].append(st)
        if isinstance(st, dict) and "github.com" in str(st.get("url", "")) and st.get("tab") not in seen_github:
            seen_github.add(st.get("tab"))
            break
        rpc("debug.key", {"key": "tab", "modifiers": ["control"]})
        time.sleep(1)
    r["start"] = rpc("browser.page.eval", {"script": PROBE})
    time.sleep(4)
    r["result"] = rpc("browser.page.eval", {"script": "JSON.stringify(window.__cmuxProbe)"})
    r["snap"] = rpc("debug.window_snapshot", {"kind": "main", "path": os.path.join(OUT, f"8-github-{label}.png")})
    report[f"github_{label}"] = r

proc = subprocess.Popen([exe], env=ENV, stdout=open(os.path.join(OUT, "app.log"), "w"), stderr=subprocess.STDOUT)
print("pid", proc.pid, flush=True)
try:
    for _ in range(120):
        if os.path.exists(SOCKET) and "raw" not in (rpc("debug.windows") or {}): break
        time.sleep(0.5)
    report["open_default"] = rpc("action.run", {"action": "openBrowser", "args": {"url": "https://example.com/", "engine": "cef"}, "origin": "user"})
    if "error" in json.dumps(report["open_default"]).lower():
        report["open_default_cli"] = rpc("action.run", {"action": "openBrowser", "args": {"url": "https://example.com/", "engine": "cef"}})
    time.sleep(4)
    report["snap_main_default"] = rpc("debug.window_snapshot", {"kind": "main", "path": os.path.join(OUT, "1-default-tab.png")})
    report["open_agent_profile"] = rpc("action.run", {"action": "openBrowser", "args": {"url": "https://example.com/", "profile": "agent"}})
    time.sleep(4)
    report["bad_profile"] = rpc("action.run", {"action": "openBrowser", "args": {"url": "https://example.com/", "profile": "work"}})
    # Agent-opened tabs stay in the background (automation never moves focus): select them as a person would (Ctrl-Tab).
    for n in range(7):
        report[f"next_tab_{n}"] = rpc("debug.key", {"key": "tab", "modifiers": ["control"]})
        time.sleep(2)
        report[f"snap_tab_{n}"] = rpc("debug.window_snapshot", {"kind": "main", "path": os.path.join(OUT, f"4-tab-{n + 2}.png")})
    # The sheet on the selected (agent profile) tab: palette, type the title, Return.
    report["palette_open"] = rpc("debug.key", {"key": "p", "modifiers": ["command", "shift"]})
    time.sleep(1)
    for ch in "allow agents":
        rpc("debug.key", {"target": "palette", "key": ch})
    time.sleep(0.8)
    report["palette"] = rpc("debug.palette.capture", {"path": os.path.join(OUT, "5-palette.png")})
    report["palette_return"] = rpc("debug.key", {"target": "palette", "key": "return"})
    time.sleep(1.5)
    windows = (rpc("debug.window_list") or {}).get("windows", [])
    report["windows"] = windows
    sheet = next((w for w in windows if w.get("kind") == "sheet" and w.get("visible")), None)
    if sheet:
        report["snap_sheet"] = rpc("debug.window_snapshot", {"window": sheet["id"], "path": os.path.join(OUT, "6-allow-agents-sheet.png")})
        report["snap_main_with_sheet"] = rpc("debug.window_snapshot", {"kind": "main", "path": os.path.join(OUT, "7-main-with-sheet.png")})
        rpc("debug.key", {"key": "escape"})
        time.sleep(0.5)
    report["person_only_refused"] = rpc("action.run", {"action": "browser.allowAgentWithExtensions"})
    probe("cef", "cef")
    probe("webkit", "webkit")
    with open(os.path.join(APP, "Contents/Info.plist"), "rb") as f:
        report["bluetooth_usage"] = plistlib.load(f).get("NSBluetoothAlwaysUsageDescription")
finally:
    with open(os.path.join(OUT, "report.json"), "w") as f: json.dump(report, f, indent=1, default=str)
    proc.terminate()
    try: proc.wait(timeout=20)
    except Exception: proc.kill()
print(json.dumps({k: (v if k != "windows" else len(v)) for k, v in report.items()}, default=str)[:3000])
