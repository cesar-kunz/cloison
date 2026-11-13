// A local date past any time value (a year past 2^53 in the making) is
// an invalid date, not a search for its year that never ends; dates in
// range keep their fields, whatever the zone.
// expect: NaN NaN NaN NaN NaN NaN NaN NaN 2024 1 29 12 34

var r = [new Date(2000, 0, 1, 1e20).getTime(), new Date(2000, 0, 1e20).getTime(), new Date(0).setDate(1e20),
  new Date(1e300, 0).getTime(), new Date(-1e20, 0).getTime(), new Date(2000, 0, 1).setHours(1e20),
  new Date(2000, 0, 1).setFullYear(1e17), new Date(2000, 0, 1).setMonth(1e19)];
var d = new Date(2024, 1, 29, 12, 34);
r.push(d.getFullYear(), d.getMonth(), d.getDate(), d.getHours(), d.getMinutes());
r.join(" ");
