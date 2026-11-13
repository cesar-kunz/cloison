// Inlined callees read parameters they never write from the caller's
// argument registers; an exit inside them (here int32 overflows in `add`,
// nested in `both`, and a shape miss in `scale`) copies those into the
// callee's frame for the interpreter. The expected value is Node's.
// expect: 955175

        function P(x) { this.x = x; }
        P.prototype.add = function (a, b) { return this.x + a + b; };
        P.prototype.both = function (a, b) { return this.add(a, b) * 2 + a; };
        function Q(x) { this.x = x; this.y = 1; }
        Q.prototype.add = function (a, b) { return this.x - a + b; };
        Q.prototype.both = function (a, b) { return this.add(b, a) + b; };
        function scale(o, k) { return o.x * k + k; }
        function main() {
          var t = 0, i, ps = [new P(1), new Q(2)], p = new P(3);
          for (i = 0; i < 3000; i++) {
            var big = i < 2500 ? i : 2147483644;
            t = (t + p.both(big, i & 7)) % 1000003;
            t = (t + ps[i & 1].both(i, big)) % 1000003;
            var o = i < 2900 ? p : ps[1];
            t = (t + scale(o, i & 3)) % 1000003;
          }
          return t;
        }
        main();
