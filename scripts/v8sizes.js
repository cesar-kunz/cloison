// Run one script in V8's main context (as scripts/octane-tiers.sh does)
// and report what V8 holds for it at the end: bytecode and machine code
// with their metadata (`v8.getHeapCodeStatistics`), and the run's time.
// Node's own JavaScript is in those numbers too: scripts/octane-sizes.sh
// subtracts an empty script's.
//
//   node scripts/v8sizes.js file.js   # the completion value; `v8sizes …` on stderr
const fs = require("fs"), vm = require("vm"), v8 = require("v8");
const src = fs.readFileSync(process.argv[2], "utf8");
// (Global, as in `node -e`, where scripts/octane-tiers.sh runs them: zlib's
// Emscripten code takes Node for a shell without it, and calls `read`.)
globalThis.require = require;
const t0 = process.hrtime.bigint();
const r = vm.runInThisContext(src, { filename: process.argv[2] });
const ms = Number(process.hrtime.bigint() - t0) / 1e6;
const c = v8.getHeapCodeStatistics();
console.log(String(r));
console.error(`v8sizes bytecode=${c.bytecode_and_metadata_size} code=${c.code_and_metadata_size} ms=${ms.toFixed(1)}`);
