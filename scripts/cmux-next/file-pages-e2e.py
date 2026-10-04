#!/usr/bin/env python3
"""The markdown and code editor pages on a running no-activate tagged build (diff-host S6, S7).

Opens a .md file and a .ts file through Open File... (the command palette, the cmux picker in
file mode, a typed path, Return), edits each page with `debug.key`, saves with Cmd-S (the app's
key dispatcher sends the page's `save` command) and checks the bytes on disk; changes the .ts
file on disk while the page has an unsaved edit and checks the conflict banner; opens a file
above the large-file threshold. Screenshots are `debug.window_snapshot` of the app's window and
`debug.page snapshot` of the page. The app quits through `debug.quit`; only the PID launched here
is ever killed. Run on cmux-lawrence-2, never the laptop.

Usage: file-pages-e2e.py --tag <tag> [--out DIR]
"""
import argparse, glob, hashlib, json, os, signal, socket, subprocess, sys, tempfile, time

parser = argparse.ArgumentParser()
parser.add_argument("--tag", required=True)
parser.add_argument("--out", default=os.environ.get("NX_ARTIFACTS") or tempfile.mkdtemp(prefix="file-pages-e2e-"))
opts = parser.parse_args()
os.makedirs(opts.out, exist_ok=True)
SOCKET = f"/tmp/cmux-debug-{opts.tag}.sock"
APP = next(iter(sorted(glob.glob(os.path.expanduser(
    f"~/Library/Developer/Xcode/DerivedData/*/Build/Products/Debug/cmux DEV {opts.tag}.app")))), None)
if not APP:
    sys.exit(f"no tagged app for {opts.tag}")
BINARY = os.path.join(APP, "Contents/MacOS/cmux DEV")
SCRATCH = tempfile.mkdtemp(prefix=f"file-pages-{opts.tag}-")
FIXTURE = "/tmp/hq48fp-fixture"
CONFIG = os.path.join(SCRATCH, "cmux.json")
GHOSTTY = os.path.join(SCRATCH, "ghostty")
open(GHOSTTY, "w").write("")
open(CONFIG, "w").write("{}")
failures = []
MD = os.path.join(FIXTURE, "README.md")
TS = os.path.join(FIXTURE, "src", "app.ts")
BIG = os.path.join(FIXTURE, "src", "big.ts")
MD_BYTES = b"# File pages\n\nFirst paragraph stays.\n\nSecond paragraph.\n"
TS_BYTES = "﻿const a = 1;\r\nconst b = 2;\nconst c = 3;".encode("utf-8")


def rpc(method, params=None, timeout=30):
    try:
        conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        conn.settimeout(timeout)
        conn.connect(SOCKET)
        conn.sendall((json.dumps({"id": 1, "method": method, "params": params or {}}) + "\n").encode())
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = conn.recv(1 << 20)
            if not chunk:
                break
            buf += chunk
        conn.close()
        reply = json.loads(buf)
        return reply.get("result") if reply.get("ok") else {"error": reply.get("error")}
    except (OSError, ValueError) as error:
        return {"error": str(error)}


def wait(label, predicate, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.1)  # test harness wait, not app code
    return None


def expect(label, condition, detail=""):
    print(("ok   " if condition else "FAIL ") + label + (f": {detail}" if detail and not condition else ""), flush=True)
    if not condition:
        failures.append(label)


def key(name, modifiers=None, target=None):
    params = {"key": name, "modifiers": modifiers or []}
    if target:
        params["target"] = target
    reply = rpc("debug.key", params) or {}
    time.sleep(0.12)  # test harness: let the key land
    return reply


def type_text(text, target=None):
    for char in text:
        key(char, target=target)


def palette_open():
    windows = (rpc("debug.window_list") or {}).get("windows", [])
    mains = {w["id"] for w in windows if w.get("kind") == "main"}
    return any(w.get("visible") and w.get("kind") in ("panel", "palette") and w.get("parent") in mains for w in windows)


def page_state(page):
    return rpc("debug.page", {"page": page, "action": "state"}) or {}


def snapshot(name, page=None):
    path = os.path.join(opts.out, f"{name}.png")
    if page:
        reply = rpc("debug.page", {"page": page, "action": "snapshot", "path": path}) or {}
    else:
        reply = rpc("debug.window_snapshot", {"path": path}) or {}
    print(f"snapshot {name}: {reply.get('path') or reply}", flush=True)


def open_file(path):
    """Open File... from the palette: the picker in file mode, a typed path, Return."""
    key("p", ["command", "shift"])
    if not wait("the palette opens", palette_open, 10):
        return False
    type_text("Open File", target="palette")
    key("return", target="palette")
    time.sleep(0.6)  # test harness: the picker's first listing
    type_text(path, target="palette")
    key("return", target="palette")
    return wait("the palette closes", lambda: not palette_open(), 10) is not None


def make_fixture():
    subprocess.run(["rm", "-rf", FIXTURE], check=False)
    os.makedirs(os.path.join(FIXTURE, ".git"), exist_ok=True)
    os.makedirs(os.path.join(FIXTURE, "src"), exist_ok=True)
    open(MD, "wb").write(MD_BYTES)
    open(TS, "wb").write(TS_BYTES)
    line = "export const value: number = 42; // a large TypeScript file for the large-file mode\n"
    with open(BIG, "w") as big:
        big.write(line * (10 * 1024 * 1024 // len(line) + 1))


def read(path):
    return open(path, "rb").read()


app = None
try:
    make_fixture()
    if os.path.exists(SOCKET):
        os.unlink(SOCKET)
    env = {"HOME": os.environ["HOME"], "USER": os.environ.get("USER", ""), "TMPDIR": os.environ.get("TMPDIR", "/tmp"),
           "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "CMUX_NEXT_NO_ACTIVATE": "1", "CMUX_NEXT_SOCKET_MODE": "automation",
           "CMUX_NEXT_TEST_WINDOW_SCREEN": "last", "CMUX_NEXT_CONFIG_FILE": CONFIG, "CMUX_NEXT_GHOSTTY_CONFIG": GHOSTTY,
           "CMUX_NEXT_TEST_WINDOW_FRAME": "40,40,1300,850"}
    app = subprocess.Popen([BINARY], env=env, stdout=open(os.path.join(SCRATCH, "app.log"), "a"),
                           stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, cwd=FIXTURE)
    print(f"launched pid {app.pid}", flush=True)
    if not wait("the tagged app comes up", lambda: os.path.exists(SOCKET) and (rpc("debug.surfaces") or {}).get("windows"), 90):
        sys.exit("FAIL the tagged app did not come up")
    time.sleep(2)  # test harness: let the window settle
    # A workspace with a terminal in the fixture (its git root is the workspace root, so saves are allowed).
    key("n", ["command"])
    focus = wait("a terminal has the keyboard", lambda: (lambda f: f if "terminal" in json.dumps(f) else None)(rpc("debug.focus") or {}), 20)
    expect("Cmd-N opens a workspace with a terminal", focus is not None, json.dumps(rpc("debug.focus"))[:300])
    time.sleep(2)  # test harness: the shell reports its cwd
    snapshot("workspace")

    # 1. Markdown: Open File..., the markdown page, an edit, Cmd-S.
    expect("Open File... opens README.md", open_file(MD))
    state = wait("the markdown page renders", lambda: (lambda s: s if "File pages" in (s.get("text") or "") else None)(page_state("cmux.markdown")), 20) or {}
    expect("the markdown page shows the file", "First paragraph stays." in (state.get("text") or ""), json.dumps(state)[:400])
    snapshot("markdown-open")
    snapshot("markdown-page", page="cmux.markdown")
    # The caret starts in the document; End then typing edits one block.
    key("down", ["command"])
    type_text(" Edited")
    key("s", ["command"])
    saved = wait("the markdown save lands", lambda: b"Edited" in read(MD), 10)
    body = read(MD)
    expect("Cmd-S saved the markdown edit", saved is not None, repr(body))
    expect("the untouched block is byte for byte", b"# File pages\n\nFirst paragraph stays.\n" in body, repr(body))
    snapshot("markdown-saved", page="cmux.markdown")

    # 2. Code: Open File..., the editor page, an edit, Cmd-S, byte exact.
    expect("Open File... opens app.ts", open_file(TS))
    state = wait("the editor renders", lambda: (lambda s: s if "const a" in (s.get("text") or "") else None)(page_state("cmux.editor")), 30) or {}
    expect("the editor shows the file", "const b = 2" in (state.get("text") or ""), json.dumps(state)[:400])
    snapshot("editor-open")
    key("down", ["command"])
    key("end", ["command"])
    type_text(";")
    key("s", ["command"])
    saved = wait("the editor save lands", lambda: read(TS) != TS_BYTES, 10)
    expect("Cmd-S saved the editor edit", saved is not None)
    expect("the save is byte exact (BOM, CRLF, LF, no final newline)", read(TS) == TS_BYTES + b";", repr(read(TS)))
    snapshot("editor-saved", page="cmux.editor")

    # 3. Conflict: an unsaved edit, then the file changes on disk, then Cmd-S.
    rpc("debug.page", {"page": "cmux.editor", "action": "command", "command": "editorAction", "text": "cursorBottom"})
    type_text("X")
    open(TS, "wb").write(b"changed elsewhere\n")
    key("s", ["command"])
    banner = wait("the conflict banner", lambda: (lambda s: s if ("Reload" in (s.get("text") or "") or "Keep" in (s.get("text") or "")) else None)(page_state("cmux.editor")), 10) or {}
    expect("the conflict banner offers Reload and Keep My Changes", bool(banner), json.dumps(page_state("cmux.editor"))[:400])
    expect("the other writer's bytes stay on disk", read(TS) == b"changed elsewhere\n", repr(read(TS)))
    snapshot("editor-conflict")
    snapshot("editor-conflict-page", page="cmux.editor")

    # 4. A large file opens in the large-file mode (the conflicted tab closes first, so the
    # editor page `debug.page` reads is the large file's).
    key("w", ["command"])
    time.sleep(1)  # test harness: the closed tab's flush
    started = time.time()
    expect("Open File... opens big.ts", open_file(BIG))
    state = wait("the large file renders", lambda: (lambda s: s if "export const value" in (s.get("text") or "") else None)(page_state("cmux.editor")), 60) or {}
    print(f"large file shown after {time.time() - started:.1f} s", flush=True)
    expect("the large file opens", bool(state), json.dumps(page_state("cmux.editor"))[:400])
    snapshot("editor-large")
    snapshot("editor-large-page", page="cmux.editor")

    print("FAILURES: " + ", ".join(failures) if failures else "ok: every step passed", flush=True)
finally:
    if app and app.poll() is None:
        print(f"debug.quit: {rpc('debug.quit', {'open': True})}", flush=True)
        time.sleep(1)  # test harness: the quit sheet, if any
        sheet = rpc("debug.quit") or {}
        if sheet.get("asking"):
            print(f"debug.quit press: {rpc('debug.quit', {'press': 'end-everything'})}", flush=True)
        try:
            app.wait(timeout=20)
            print(f"quit {app.pid} exit {app.returncode}", flush=True)
        except subprocess.TimeoutExpired:
            app.send_signal(signal.SIGKILL)  # the PID launched above, never a pattern
            print(f"killed {app.pid} (did not quit)", flush=True)
    if app and app.returncode not in (None, 0):
        print(open(os.path.join(SCRATCH, "app.log")).read()[-3000:])
sys.exit(1 if failures else 0)
