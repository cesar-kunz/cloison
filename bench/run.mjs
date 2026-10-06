#!/usr/bin/env node
// Runs the JS microbenchmarks: checks results, records expectations, times them.
//
//   node bench/run.mjs [options] [files...]      (default: bench/micro/*.js)
//
//   --update            write each file's `// expect:` line from the reference run
//   --time              time each file (median of --reps fresh runs)
//   --reps N            runs per file for --time (default 11)
//   --features a,b,...  only files whose features are all in this list
//   --engine CMD        run `CMD file` instead of Node; its stdout must print
//                       the completion value (e.g. --engine "target/release/toi")
//
// A benchmark is a plain script with no host calls. Its result is the
// script's completion value (the value of its last expression statement),
// which must be a number. Header comments:
//
//   // features: loop, call, ...      language features it needs (see FEATURES)
//   // expect: 12345                  the result, as printed by String(value)

import { readFileSync, writeFileSync, readdirSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const FEATURES = new Set([
  "loop", "double", "string", "switch", "call", "recursion", "closure",
  "object", "new", "proto", "call-method", "array", "global", "null",
  "define-property", "for-in",
]);

const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const opt = { update: false, time: false, reps: 11, features: null, engine: null };
const files = [];
for (let i = 0; i < args.length; i++) {
  const a = args[i];
  if (a === "--update") opt.update = true;
  else if (a === "--time") opt.time = true;
  else if (a === "--reps") opt.reps = +args[++i];
  else if (a === "--features") opt.features = new Set(args[++i].split(","));
  else if (a === "--engine") opt.engine = args[++i];
  else if (a.startsWith("--")) throw new Error(`unknown option ${a}`);
  else files.push(a);
}
if (files.length === 0) {
  const dir = join(here, "micro");
  for (const f of readdirSync(dir).sort()) if (f.endsWith(".js")) files.push(join(dir, f));
}

function header(src, key) {
  const m = src.match(new RegExp(`^// ${key}: (.*)$`, "m"));
  return m ? m[1].trim() : null;
}

function runNode(src, file) {
  return vm.runInNewContext(src, {}, { filename: file });
}

function runEngine(file) {
  const [cmd, ...pre] = opt.engine.split(/\s+/);
  return Number(execFileSync(cmd, [...pre, file], { encoding: "utf8" }).trim());
}

function median(xs) {
  const s = [...xs].sort((a, b) => a - b);
  return s[s.length >> 1];
}

let failures = 0;
for (const file of files) {
  const name = relative(process.cwd(), file);
  let src = readFileSync(file, "utf8");
  const feats = (header(src, "features") ?? "").split(",").map((s) => s.trim()).filter(Boolean);
  for (const f of feats) if (!FEATURES.has(f)) throw new Error(`${name}: unknown feature ${f}`);
  if (opt.features && !feats.every((f) => opt.features.has(f))) continue;

  const run = () => (opt.engine ? runEngine(file) : runNode(src, file));
  const t0 = performance.now();
  const value = run();
  const first = performance.now() - t0;
  if (typeof value !== "number") throw new Error(`${name}: completion value ${value} is not a number`);

  if (opt.update) {
    src = src.replace(/^\/\/ expect: .*$/m, `// expect: ${String(value)}`);
    writeFileSync(file, src);
  }
  const expect = header(src, "expect");
  const ok = expect !== null && Object.is(Number(expect), value);
  if (!ok) failures++;

  let line = `${ok ? "ok  " : "FAIL"} ${name.padEnd(48)} ${String(value)}`;
  if (!ok) line += `  (expected ${expect})`;
  if (opt.time) {
    const ts = [first];
    for (let i = 1; i < opt.reps; i++) {
      const t = performance.now();
      run();
      ts.push(performance.now() - t);
    }
    line += `  ${median(ts).toFixed(2)} ms`;
  }
  console.log(line);
}

if (failures) {
  console.log(`${failures} failed`);
  process.exit(1);
}
