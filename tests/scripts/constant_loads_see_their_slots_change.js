// Loads of a global function and of `C.parent.prototype.get.call` compile
// to constants behind checks of their slots (or none, for a prototype's):
// the global reassigned, `C.parent` switched, the prototype's method
// replaced and `call` shadowed by an own property, each midway, are seen.
// The expected value is Node's.
// expect: 1027500

        function A() { this.v = 1; }
        A.prototype.get = function (k) { return this.v + k; };
        function B() { this.v = 2; }
        B.prototype.get = function (k) { return this.v * k; };
        function C() {}
        C.parent = A;
        var helper = function (x) { return x + 1; };
        function main() {
          var t = 0, i, o = new A();
          for (i = 0; i < 4000; i++) {
            if (i === 1000) helper = function (x) { return x + 2; };
            if (i === 2000) C.parent = B;
            if (i === 2500) A.prototype.get = function (k) { return this.v - k; };
            if (i === 3000) B.prototype.get.call = function (that, k) { return 1000; };
            t += helper(i & 7);
            t += C.parent.prototype.get.call(o, i & 3);
          }
          return t;
        }
        main();
