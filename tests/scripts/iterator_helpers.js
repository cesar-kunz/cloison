// `Iterator` and its helpers: the lazy ones (closing the underlying
// iterator on `return()`, and when an argument is bad), the eager ones,
// `Iterator.from`, the abstract constructor, the accessors on
// `Iterator.prototype`; `Iterator.concat`; and the proposals' `zip`,
// `zipKeyed`, `chunks`, `windows`, `includes` and `join`.
// expect: 0-6-12-18 | 12 | 1,10,2,20,3,30 | true,true,8 | false,1,1 | TypeError | 0 | [object X] | TypeError | 2{"done":true} | TypeError | RangeError | [1,2,3] | [[1,"a",0],[2,"b",1]] | [[1,4],[2,"y"],[3,"y"]] | TypeError | [{"a":1,"b":3},{"a":2,"b":4}] | [[0,1,2],[3,4,5],[6]][[0,1,2],[1,2,3]] | [[0,1]] | true,true,false | 1,,a,|1--2

var r = [];
function t(f) { try { r.push(String(f())); } catch (e) { r.push(e.name); } }
function* nat() { let i = 0; while (true) yield i++; }
t(() => nat().map(x => x * 2).filter(x => x % 3 == 0).take(4).toArray().join("-"));
t(() => nat().drop(3).take(3).reduce((a, b) => a + b));
t(() => [1, 2, 3].values().flatMap(x => [x, x * 10]).toArray().join());
t(() => nat().some(x => x > 5) + "," + nat().take(5).every(x => x < 5) + "," + nat().find(x => x * x > 50));
t(() => { var it = Iterator.from({ next() { return { value: 1, done: false }; } }); return [Object.getPrototypeOf(it) === Iterator.prototype, it.take(2).toArray().join()].join(); });
t(() => new Iterator());
t(() => { class MyIt extends Iterator { next() { return { done: true }; } } return new MyIt().toArray().length; });
t(() => { var o = Object.create(Iterator.prototype); o[Symbol.toStringTag] = "X"; return Object.prototype.toString.call(o); });
t(() => { Iterator.prototype[Symbol.toStringTag] = "Y"; });
t(() => { var closed = 0; var it = { __proto__: Iterator.prototype, next() { return { value: 1, done: false }; }, return() { closed++; return {}; } }; var h = it.map(x => x); h.next(); h.return(); h.return(); try { it.map(1); } catch (e) {} return closed + JSON.stringify(h.next()); });
t(() => { var h = nat().map(x => h.next()); return h.next(); });
t(() => nat().take(-1));
t(() => JSON.stringify(Iterator.concat([1, 2], new Set([3])).toArray()));
t(() => JSON.stringify(Iterator.zip([[1, 2, 3], "ab".split(""), nat()]).toArray()));
t(() => JSON.stringify(Iterator.zip([[1, 2, 3], [4]], { mode: "longest", padding: ["x", "y"] }).toArray()));
t(() => JSON.stringify(Iterator.zip([[1, 2], [3]], { mode: "strict" }).toArray()));
t(() => JSON.stringify(Iterator.zipKeyed({ a: [1, 2], b: [3, 4, 5], c: undefined }).toArray()));
t(() => JSON.stringify(nat().take(7).chunks(3).toArray()) + JSON.stringify(nat().take(4).windows(3).toArray()));
t(() => JSON.stringify(nat().take(2).windows(3, "allow-partial").toArray()));
t(() => [nat().includes(4), [1, NaN].values().includes(NaN), [1, 2, 3].values().includes(1, 1)].join());
t(() => [1, null, "a", undefined].values().join() + "|" + [1, 2].values().join("--"));
r.join(" | ");
