// Code compiled for an outer loop, entered at the inner loop's header
// (tiering up there): the stub that zeroes dead registers once per
// collection must go on to that header, not to the region's first
// instruction (`j = 0`), which ran the inner loop's iterations again.
// expect: 4162
var s = 0;
             for (var i = 0; i < 3; i++) for (var j = 0; j < 3; j++) { s = s * 3 + j + i; }
             s;
