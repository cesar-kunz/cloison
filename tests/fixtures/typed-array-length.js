(function () {
    function eq(actual, expected, label) {
        if (actual !== expected) throw new Error(label + ': ' + actual + ' !== ' + expected);
    }
    function typeError(f, label) {
        var caught = false;
        try { f(); } catch (e) { if (!(e instanceof TypeError)) throw e; caught = true; }
        eq(caught, true, label);
    }
    function read(o, expected) {
        for (var i = 0; i < 3000; i++) eq(o.length, expected, 'named');
    }
    function keyed(o, key, expected) {
        for (var i = 0; i < 3000; i++) eq(o[key], expected, 'keyed');
    }
    var proto = Object.getPrototypeOf(Uint8Array.prototype);
    var builtin = Object.getOwnPropertyDescriptor(proto, 'length').get;
    var ta = new Uint8Array(3);
    // One read site changes from array/string specialization to typed arrays.
    read([1, 2], 2);
    read('four', 4);
    var far = []; far.length = 2 ** 31;
    read(far, 2 ** 31);
    read(ta, 3);
    Object.defineProperty(ta, 'length', {value: 'own', writable: true, configurable: true});
    read(ta, 'own');
    keyed(ta, 'length', 'own');
    eq(builtin.call(ta), 3, 'internal length');
    ta.length = undefined;
    read(ta, undefined);
    var calls = 0;
    Object.defineProperty(ta, 'length', {get: function () {
        eq(this, ta, 'own getter receiver'); calls++; return 17;
    }, configurable: true});
    read(ta, 17);
    eq(calls, 3000, 'getter count');
    Object.defineProperty(ta, 'length', {get: function () { throw 'sentinel'; }, configurable: true});
    var caught = false;
    try { read(ta, 0); } catch (e) { caught = e === 'sentinel'; }
    eq(caught, true, 'getter throw');
    delete ta.length;
    Object.defineProperty(ta, 'length', {set: function (v) {}, configurable: true});
    read(ta, undefined);
    delete ta.length;
    read(ta, 3);

    // Mutations of an intermediate prototype must invalidate earlier results.
    var parent = Object.create(Object.getPrototypeOf(ta));
    Object.setPrototypeOf(ta, parent);
    read(ta, 3);
    Object.defineProperty(parent, 'length', {value: 21, writable: true, configurable: true});
    read(ta, 21);
    parent.length = 22;
    read(ta, 22);
    Object.defineProperty(parent, 'length', {get: function () {
        eq(this, ta, 'inherited receiver'); return 23;
    }, configurable: true});
    read(ta, 23);
    delete parent.length;
    read(ta, 3);

    // A typed-array grandparent does not supply internal slots to a child.
    var grandparent = new Uint8Array(5);
    var middle = Object.create(grandparent);
    var child = Object.create(middle);
    Object.defineProperty(middle, 'length', {value: 'middle', configurable: true});
    read(child, 'middle');
    delete middle.length;
    typeError(function () { return child.length; }, 'child receiver');
    eq(Reflect.get(child, 'length', grandparent), 5, 'explicit receiver');
    typeError(function () { return Reflect.get(grandparent, 'length', {}); }, 'bad explicit receiver');
    typeError(function () { return new Proxy(grandparent, {}).length; }, 'proxy receiver');

    // A virtual own property must win before a farther shape slot.
    var arr = [10, 20];
    Object.setPrototypeOf(arr, {length: 99});
    Object.setPrototypeOf(ta, arr);
    read(ta, 2);
    keyed(ta, 'length', 2);
    arr.length = 1; // No shape transition: do not cache the previous value.
    read(ta, 1);
    var ordinary = Object.create(arr);
    read(ordinary, 1);
    var fn = function (a, b) {};
    Object.setPrototypeOf(fn, {length: 99});
    Object.setPrototypeOf(ta, fn);
    read(ta, 2);
    delete fn.length;
    read(ta, 99);
    var str = new String('abc');
    Object.setPrototypeOf(str, {length: 99});
    Object.setPrototypeOf(ta, str);
    read(ta, 3);

    Object.setPrototypeOf(ta, null);
    read(ta, undefined);
    eq('length' in ta, false, 'no invented own length');
    eq(builtin.call(ta), 3, 'internal slots survive prototype removal');
    Object.setPrototypeOf(grandparent, null);
    read(child, undefined);
    eq('length' in child, false, 'no invented ancestor length');

    // Generic methods observe length properties; typed-array methods do not.
    var xs = new Uint8Array([1, 2, 3]);
    Object.defineProperty(xs, 'length', {value: 1});
    eq(Array.prototype.map.call(xs, function (x) { return x; }).length, 1, 'generic map');
    eq(xs.map(function (x) { return x; }).length, 3, 'typed map');
    eq(xs[2], 3, 'indexed access unchanged');
    eq(Array.from(xs).length, 3, 'typed iterator');

    // The builtin getter still owns buffer checks, including after warmup.
    var buffer = new ArrayBuffer(4);
    var detached = new Uint8Array(buffer);
    read(detached, 4);
    buffer.transfer();
    read(detached, 0);
    Object.defineProperty(detached, 'length', {value: 8});
    read(detached, 8);
    eq(builtin.call(detached), 0, 'detached builtin');
    var rab = new ArrayBuffer(4, {maxByteLength: 8});
    var fixed = new Uint8Array(rab, 0, 4), tracking = new Uint8Array(rab);
    read(fixed, 4); read(tracking, 4);
    rab.resize(2);
    read(fixed, 0); read(tracking, 2);
    rab.resize(6);
    read(fixed, 4); read(tracking, 6);
    var sab = new SharedArrayBuffer(4, {maxByteLength: 8});
    var shared = new Uint8Array(sab);
    read(shared, 4);
    sab.grow(7);
    read(shared, 7);

    // Changing the builtin itself is visible; restore it even on failure.
    var desc = Object.getOwnPropertyDescriptor(proto, 'length');
    try {
        var fresh = new Uint8Array(6);
        read(fresh, 6);
        Object.defineProperty(proto, 'length', {value: 41, configurable: true});
        read(fresh, 41);
        delete proto.length;
        read(fresh, undefined);
        eq('length' in fresh, false, 'deleted builtin');
    } finally { Object.defineProperty(proto, 'length', desc); }
    read(new Uint8Array(6), 6);
    // Primitive lookup's get_through must preserve virtual-property order too.
    var numberProto = Object.getPrototypeOf(Number.prototype);
    var primitiveParent = [1, 2];
    Object.setPrototypeOf(primitiveParent, {length: 99});
    try {
        Object.setPrototypeOf(Number.prototype, primitiveParent);
        eq((42).length, 2, 'primitive inherited virtual length');
    } finally { Object.setPrototypeOf(Number.prototype, numberProto); }
    // Other element kinds share the same semantics.
    var kinds = [Int8Array, Uint8ClampedArray, Int16Array, Uint16Array,
                 Int32Array, Uint32Array, Float32Array, Float64Array,
                 BigInt64Array, BigUint64Array];
    for (var k = 0; k < kinds.length; k++) {
        var other = new kinds[k](2);
        read(other, 2);
        Object.defineProperty(other, 'length', {value: 'shadow'});
        read(other, 'shadow');
    }
    return 'ok';
})()
