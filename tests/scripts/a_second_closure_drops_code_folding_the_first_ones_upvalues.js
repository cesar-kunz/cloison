// The closure's loop compiled while its code had made one closure,
// `list` taken as a constant: `g` pushed to `a`.
// expect: 6000 3000
function mk(list) {
               return function (n) {
                 for (var i = 0; i < n; i++) list.push(i);
                 return list.length;
               };
             }
             var a = [];
             var f = mk(a);
             f(3000);
             f(3000);
             var b = [];
             var g = mk(b);
             g(3000);
             a.length + ' ' + b.length
