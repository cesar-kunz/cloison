// `%` of a parameter read only in a `catch` (#141): live at the start
// through the exception, so its sign counts. In a loop in the `catch`,
// and after the `try` from a register the `catch` set. The expected
// values are V8's, SpiderMonkey's and JSC's (ES2024 §6.1.6.1.6).
// expect: -1,-1,1

        function inCatch(x, o) {
          var r = 0;
          try { o.p; } catch (e) {
            for (var i = 0; i < 3000; i++) { r = x % 3; }
          }
          return r;
        }
        function afterTry(x, o) {
          var r = 0, y = 1;
          try { o.p; } catch (e) { y = x; }
          for (var i = 0; i < 3000; i++) { r = y % 3; }
          return r;
        }
        inCatch(-7, null) + "," + afterTry(-7, null) + "," + inCatch(7, null);
