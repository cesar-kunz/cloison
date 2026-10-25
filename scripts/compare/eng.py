"""Engines for the comparison: argv prefix and environment, binaries run directly."""
import os, subprocess

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
# Results (kept), and the scripts the harness writes (wrappers, probes).
OUT = os.path.join(ROOT, "bench", "compare")
TMP = os.path.join(ROOT, "target", "compare")
os.makedirs(TMP, exist_ok=True)
E = os.path.expanduser("~/.jsvu/engines")
C = os.path.join(ROOT, "target", "release", "cloison")
# (The loader's --library-path, not LD_LIBRARY_PATH, which perf or time would load too.)
JSC = [f"{E}/javascriptcore/lib/ld-linux-x86-64.so.2", "--library-path", f"{E}/javascriptcore/lib", f"{E}/javascriptcore/javascriptcore"]
JSC_ENV = {}
SM = ["/lib64/ld-linux-x86-64.so.2", "--library-path", f"{E}/spidermonkey", f"{E}/spidermonkey/spidermonkey"]
SM_ENV = {}

ENGINES = {
    "cloison": ([C], {}),
    "cloison-nojit": ([C], {"CLOISON_NO_JIT": "1"}),
    "v8": ([f"{E}/v8/v8"], {}),
    "v8-jitless": ([f"{E}/v8/v8", "--jitless"], {}),
    "jsc": (JSC, JSC_ENV),
    "jsc-nojit": (JSC + ["--useJIT=false"], JSC_ENV),
    "sm": (SM, SM_ENV),
    "sm-nojit": (SM + ["--no-blinterp", "--no-baseline", "--no-ion", "--no-native-regexp"], SM_ENV),
    "qjs": ([f"{E}/quickjs/qjs"], {}),
}
JIT = ["cloison", "v8", "jsc", "sm"]
NOJIT = ["cloison-nojit", "v8-jitless", "jsc-nojit", "sm-nojit", "qjs"]


def cmd(name, extra_args=(), extra_env=None):
    argv, env = ENGINES[name]
    return argv + list(extra_args), {**os.environ, **env, **(extra_env or {})}


def run(name, script, args=(), env=None, wrap=(), **kw):
    """Run `script` on engine `name`, `wrap` prefixed (perf stat, /usr/bin/time)."""
    argv, e = cmd(name, args, env)
    return subprocess.run(list(wrap) + argv + [script], env=e, capture_output=True, text=True, **kw)
