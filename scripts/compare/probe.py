"""Run a probe script on an engine, sampling its RSS when it prints a marker.

A probe prints `@name` lines (after a full collection) and then spins for
SPIN ms, so the process's VmRSS can be read from /proc meanwhile; other
lines are results. Output through a pty, so markers come as printed.
"""
import os, pty, re, select, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from eng import cmd, TMP
SPIN = 150

# What each shell calls a full collection, and the prelude giving `gc`,
# `spin` and `mark` to probes.
GC = {
    "cloison": "$262.gc()", "cloison-nojit": "$262.gc()",
    "v8": "gc()", "v8-jitless": "gc()",
    "jsc": "gc()", "jsc-nojit": "gc()", "sm": "gc()", "sm-nojit": "gc()",
    "qjs": "std.gc()",
}
ARGS = {"v8": ["--expose-gc"], "v8-jitless": ["--expose-gc"]}


def prelude(e):
    p = 'import * as std from "qjs:std";\n' if e == "qjs" else ""
    return p + f"""
function fullgc() {{ for (var i = 0; i < 3; i++) {GC[e]}; }}
function spin(ms) {{ var t = Date.now(); while (Date.now() - t < ms) ; }}
function mark(name) {{ fullgc(); print("@" + name); spin({SPIN}); }}
"""


def rss_kb(pid):
    try:
        return int(re.search(r"VmRSS:\s*(\d+)", open(f"/proc/{pid}/status").read()).group(1))
    except (OSError, AttributeError):
        return None


def run_probe(e, body, timeout=600):
    """{marker: RSS KB} and the result lines of `body` run on `e`."""
    path = f"{TMP}/probes/{e}-{abs(hash(body)) % 10**8}" + (".mjs" if e == "qjs" else ".js")
    os.makedirs(f"{TMP}/probes", exist_ok=True)
    open(path, "w").write(prelude(e) + body)
    argv, env = cmd(e, ARGS.get(e, []))
    master, slave = pty.openpty()
    p = subprocess.Popen(argv + [path], stdout=slave, stderr=subprocess.STDOUT, env=env, close_fds=True)
    os.close(slave)
    marks, lines, buf, end = {}, [], b"", time.time() + timeout
    while time.time() < end:
        r, _, _ = select.select([master], [], [], 0.5)
        if not r:
            if p.poll() is not None:
                break
            continue
        try:
            chunk = os.read(master, 65536)
        except OSError:
            break
        if not chunk:
            break
        buf += chunk
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            line = line.decode(errors="replace").rstrip("\r")
            if line.startswith("@"):
                time.sleep(SPIN / 3000)  # (well inside the spin)
                marks[line[1:]] = rss_kb(p.pid)
            else:
                lines.append(line)
    p.kill()
    p.wait()
    os.close(master)
    return marks, lines


if __name__ == "__main__":
    e = sys.argv[1]
    print(run_probe(e, open(sys.argv[2]).read()))
