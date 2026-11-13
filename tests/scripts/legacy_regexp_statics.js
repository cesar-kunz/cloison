// `RegExp.$1` and the other legacy static properties: the last match's,
// invalid after a subclass's.
// expect: b,true,b,a,d,abd,y,x,TypeError,q,TypeError

        var o = [];
        /(b)(c)?/.exec("abd");
        o.push(RegExp.$1, RegExp.$2 === "", RegExp.lastMatch, RegExp["$`"], RegExp["$'"], RegExp.input);
        "xyz".replace(/(y)/g, "-");
        o.push(RegExp.$1, RegExp.leftContext);
        class R extends RegExp {}
        new R("a").test("a");
        try { RegExp.$1; } catch (e) { o.push(e.name); }
        /q/.test("q");
        o.push(RegExp.lastMatch);
        try { Object.getOwnPropertyDescriptor(RegExp, "$1").get.call(R); } catch (e) { o.push(e.name); }
        o.join()
    
