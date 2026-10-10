// GC: Splay's pattern in small: a binary search tree of 40,000 nodes,
// each with a tree of 63 objects, arrays and strings as its payload
// (about 170 MiB live), whose nodes are replaced at random by new ones.
// gc: nearly everything allocated stays alive for a while, so each minor
// collection traces most of its nursery; the replaced nodes and payloads
// die old and scattered, so the heap peaks at 2.7 times what is live;
// the final pause retraces about 670,000 cells (12 ms, against 7 ms for
// Octane's Splay); 19% of the time is spent collecting.
// features: loop, call, object, new, array, null
// expect: 128976

var seed = 12345;
function rand(n) {
  seed = (seed * 16807) % 2147483647;
  return seed % n;
}

function Node(key, left, right) {
  this.key = key;
  this.left = left;
  this.right = right;
  this.payload = payload(4, key);
}

// Octane's Splay hangs a small tree of arrays and strings off each node.
function payload(depth, key) {
  if (depth === 0) return { a: [key, key + 1, key + 2], s: "k" + (key & 1023) };
  return { left: payload(depth - 1, key), right: payload(depth - 1, key + 1) };
}

function insert(t, key) {
  if (t === null) return new Node(key, null, null);
  if (key < t.key) t.left = insert(t.left, key);
  else if (key > t.key) t.right = insert(t.right, key);
  return t;
}

function removeMin(t) {
  if (t.left === null) return t.right;
  t.left = removeMin(t.left);
  return t;
}

function remove(t, key) {
  if (t === null) return null;
  if (key < t.key) { t.left = remove(t.left, key); return t; }
  if (key > t.key) { t.right = remove(t.right, key); return t; }
  if (t.left === null) return t.right;
  if (t.right === null) return t.left;
  var m = t.right;
  while (m.left !== null) m = m.left;
  // A new node takes the removed one's place, so the old one dies.
  var n = new Node(m.key, t.left, removeMin(t.right));
  return n;
}

function run(size, n) {
  var keys = new Array(size);
  var t = null;
  for (var i = 0; i < size; i++) {
    keys[i] = rand(1 << 30);
    t = insert(t, keys[i]);
  }
  var sum = 0;
  for (var i = 0; i < n; i++) {
    var j = rand(size);
    t = remove(t, keys[j]);
    keys[j] = rand(1 << 30);
    t = insert(t, keys[j]);
    if ((i & 1023) === 0) sum = (sum + t.key % 1000) % 1000003;
  }
  return sum;
}

run(40000, 400000);
