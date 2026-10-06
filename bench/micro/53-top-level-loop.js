// Loop at script top level: every variable is a global property.
// features: loop, global
// expect: 2050177040
var sum = 0;
for (var i = 0; i < 300000; i++) sum = (sum + i) | 0;
sum;
