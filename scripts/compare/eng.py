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
QJS = os.path.expanduser(os.environ.get("QUICKJS", "~/git/cloison-fuzz/untracked/tools/quickjs-bellard/qjs"))
# (Octane's zlib reads a file with the shells' `read`, which Bellard's lacks.)
QJS_PRELUDE = f"{TMP}/qjs-prelude.js"
open(QJS_PRELUDE, "w").write("globalThis.read = std.loadFile;\n")
KETTE = os.path.expanduser(os.environ.get("KETTE", "~/git/KetteVM/target/release/kettevm"))
ARK = os.path.expanduser(os.environ.get("ARK", "~/ARK/out/x64.release"))
ARK_TREE = os.path.dirname(os.path.dirname(ARK))
ARK_ICU = f"{ARK_TREE}/third_party/icu/ohos_icu4j/data"
ARK_LD = ":".join([f"{ARK}/arkcompiler/ets_runtime", f"{ARK}/arkcompiler/runtime_core", f"{ARK}/thirdparty/icu",
                   f"{ARK}/thirdparty/zlib", f"{ARK}/thirdparty/bounds_checking_function",
                   f"{ARK_TREE}/prebuilts/clang/ohos/linux-x86_64/llvm/lib"])

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
    # Bellard's QuickJS, KetteVM (interpreter only) and OpenHarmony's ARK
    # (`ark_js_vm` on `.abc` files `es2abc` compiles; asm interpreter, and its JIT).
    "quickjs": ([QJS, "--std", "-I", QJS_PRELUDE], {}),
    "kette": ([KETTE], {}),
    "ark": ([f"{ARK}/arkcompiler/ets_runtime/ark_js_vm", f"--icu-data-path={ARK_ICU}"], {"LD_LIBRARY_PATH": ARK_LD}),
    "ark-jit": ([f"{ARK}/arkcompiler/ets_runtime/ark_js_vm", f"--icu-data-path={ARK_ICU}",
                 "--compiler-enable-jit=true"], {"LD_LIBRARY_PATH": ARK_LD}),
}
JIT = ["cloison", "v8", "jsc", "sm", "ark-jit"]
NOJIT = ["cloison-nojit", "v8-jitless", "jsc-nojit", "sm-nojit", "quickjs", "kette", "ark"]


def cmd(name, extra_args=(), extra_env=None):
    argv, env = ENGINES[name]
    return argv + list(extra_args), {**os.environ, **env, **(extra_env or {})}


def run(name, script, args=(), env=None, wrap=(), **kw):
    """Run `script` on engine `name`, `wrap` prefixed (perf stat, /usr/bin/time)."""
    argv, e = cmd(name, args, env)
    return subprocess.run(list(wrap) + argv + [script], env=e, capture_output=True, text=True, **kw)


def abc(js, prefix=""):
    """`js` compiled by ARK's `es2abc` (after `prefix`), cached by mtime; None if it fails."""
    out = f"{TMP}/abc/{os.path.basename(js)}.abc"
    os.makedirs(f"{TMP}/abc", exist_ok=True)
    if not os.path.exists(out) or os.path.getmtime(out) < os.path.getmtime(js):
        src = js
        if prefix:
            src = f"{TMP}/abc/{os.path.basename(js)}"
            open(src, "w").write(prefix + open(js).read())
        r = subprocess.run([f"{ARK}/arkcompiler/ets_frontend/es2abc", src, "--output", out],
                           env={**os.environ, "LD_LIBRARY_PATH": ARK_LD}, capture_output=True, text=True)
        if r.returncode or not os.path.exists(out):
            return None
    return out
