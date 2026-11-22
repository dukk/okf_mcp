#!/usr/bin/python3
"""Synthetic owned child only: no provider, network, or Claude invocation."""
import json
import os
import sys
import time
import threading

root = os.path.dirname(os.path.realpath(__file__))
with open(root + "/scenario.json") as f:
    cfg = json.load(f)
if cfg.get("start_delay_ms"):
    # Models a slow interpreter start on a loaded machine: record.json (the
    # "started" signal) appears only after this delay.
    time.sleep(cfg["start_delay_ms"] / 1000)
def mark(name):
    """Publishes `name` (with this process's pid) once the state it names holds.

    Written to a temporary file and renamed, so a reader sees all of it or
    none. `record.json` only says the interpreter started; a marker says the
    child reached the point a scenario is about (issue #500).
    """
    with open(root + "/" + name + ".tmp", "w") as marker:
        json.dump({"pid": os.getpid()}, marker)
    os.rename(root + "/" + name + ".tmp", root + "/" + name)

with open(root + "/record.json", "w") as f:
    json.dump({"argv": sys.argv[1:], "env": dict(os.environ), "cwd": os.getcwd(),
               "mode": os.stat(os.getcwd()).st_mode & 0o777, "pid": os.getpid()}, f)
if "--version" in sys.argv:
    print(cfg.get("version", "2.1.319 (Claude Code)"))
    sys.exit(0)
if "--help" in sys.argv:
    print(cfg["help"])
    sys.exit(0)
if cfg.get("stall"):
    mark("reached.json")
    time.sleep(120)
prompt = sys.stdin.buffer.read()
with open(root + "/stdin", "wb") as f:
    f.write(prompt)
def stderr():
    sys.stderr.buffer.write(b"S" * cfg.get("stderr", 0))
    sys.stderr.buffer.flush()
t = threading.Thread(target=stderr)
t.start()
with open(root + "/success.json", "rb") as f:
    payload = f.read()
size = cfg.get("stdout", len(payload))
sys.stdout.buffer.write(payload + b" " * max(0, size - len(payload)))
sys.stdout.buffer.flush()
t.join()
if cfg.get("hold_pipes"):
    # An owned fake descendant keeps the pipes open after parent exit. It exits
    # naturally, when the test creates `release` (or its directory is gone, or
    # after two minutes); the adapter must never signal an unowned PID/group.
    if os.fork() == 0:
        # The marker comes first: it says the pipes are held, by this pid.
        mark("holder.json")
        for _ in range(2400):
            if os.path.exists(root + "/release") or not os.path.isdir(root):
                break
            time.sleep(0.05)
        os._exit(0)
if cfg.get("after_output_stall"):
    mark("reached.json")
    time.sleep(120)
sys.exit(cfg.get("exit", 0))
