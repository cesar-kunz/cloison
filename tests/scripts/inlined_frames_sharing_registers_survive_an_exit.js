// Two calls inlined into one loop share their frames' registers: `at`'s
// int32 parameter and the array `size` loads into the same register. The
// `length` load after it was compiled as an exit to the interpreter
// (a bail), which reads that register: it must not have been kept as an
// unboxed int32 (which truncated the array).
// expect: 3675000

        function Coll() { this.elms = []; }
        Coll.prototype.add = function (e) { this.elms.push(e); };
        Coll.prototype.at = function (i) { return this.elms[i]; };
        Coll.prototype.size = function () { return this.elms.length; };
        function collect(n) { var out = new Coll(); for (var i = 0; i < n; i++) out.add(i); return out; }
        function main() {
          var sum = 0;
          for (var r = 0; r < 3000; r++) {
            var c = collect(50);
            for (var i = 0; i < c.size(); i++) sum += c.at(i);
          }
          return sum;
        }
        main();
