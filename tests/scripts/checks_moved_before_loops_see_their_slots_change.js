// Checks of constant loads moved before their loops (`jit::hoist_guards`):
// a `for` entered at its header, a `while` entered by a jump to its
// condition inside another loop, and a `while` alone. Between runs of the
// loops the checked slots change (a property, the global, a field turning
// double): the checks before the loops fail and the loads are compiled
// again. The expected value is Node's.
// expect: 10000,735000,6000;10000,735000,6000;8000,735000,24000;7000,735000,-3000;7000,224997,-3000;10000,612500,6000;

        var Dir = new Object();
        Dir.NONE = 0; Dir.FORWARD = 1; Dir.BACKWARD = -1;
        var K = { scale: 3 };
        function count(n) {
          var sum = 0;
          for (var i = 0; i < n; i++) {
            var d = (i % 3) - 1;
            if (d == Dir.FORWARD) sum += 2;
            else if (d == Dir.BACKWARD) sum += 3;
            else if (d == Dir.NONE) sum += 5;
          }
          return sum;
        }
        function nested(n) {
          var t = 0;
          for (var r = 0; r < n; r++) {
            var j = 0;
            while (j < 50) { t += K.scale * j; j++; }
            t = t % 1000003;
          }
          return t;
        }
        function viaWhile(n) {
          var t = 0, i = 0;
          while (i < n) { t += Dir.FORWARD - Dir.BACKWARD; i++; }
          return t;
        }
        function main() {
          var out = "";
          for (var round = 0; round < 6; round++) {
            out = out + count(3000) + "," + nested(200) + "," + viaWhile(3000) + ";";
            if (round == 1) Dir.FORWARD = 7;
            if (round == 2) { Dir = { NONE: 0, FORWARD: 1, BACKWARD: 2 }; }
            if (round == 3) K.scale = 5;
            if (round == 4) { K = { scale: 2.5 }; Dir.BACKWARD = -1; }
          }
          return out;
        }
        main();
