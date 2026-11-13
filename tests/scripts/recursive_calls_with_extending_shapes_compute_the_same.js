// A recursive function whose parameter is an object of a shape or one
// extending it (`extra` added to some nodes, or another constructor's
// shape): its compiled entry takes `Ext` of the shape, so its calls to
// itself with loaded nodes are direct. The `extra` store, not compiled
// (a bail) in the copy of `build` inlined into itself, once made the
// inlined frame's registers look live at the function's entry, which
// then read leftovers from the stack as objects (a crash). The expected
// value is Node's.
// expect: 890162

        function Node(i) { this.value = i; this.kids = []; }
        function Other(i) { this.kids = []; this.value = i * 2; }
        function build(depth, i) {
          var n = depth % 3 == 0 ? new Other(i) : new Node(i);
          if (i % 5 == 0) n.extra = i;
          if (depth > 0) { n.kids.push(build(depth - 1, 2 * i)); n.kids.push(build(depth - 1, 2 * i + 1)); }
          return n;
        }
        function sum(n) {
          var s = n.value + (n.extra === undefined ? 0 : 1);
          for (var i = 0; i < n.kids.length; i++) s += sum(n.kids[i]);
          return s;
        }
        function main() {
          var t = 0;
          for (var r = 0; r < 30; r++) t = (t + sum(build(8, r + 1))) % 1000003;
          return t;
        }
        main();
