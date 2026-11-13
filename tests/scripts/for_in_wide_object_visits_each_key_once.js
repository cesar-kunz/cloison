// A wide object is a common `for … in` input (notably in SheetJS).  Keep
// this large enough that the key de-duplication path is exercised without
// baking a machine-dependent timing assertion into the suite.
// expect: 3000:key0:key2999

var o = {}, n = 3000, count = 0, first = "", last = "", k;
for (var i = 0; i < n; i++) o["key" + i] = i;
for (k in o) { if (!first) first = k; last = k; count++; }
[count, first, last].join(":");
