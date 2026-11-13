// `FinalizationRegistry`'s API: registering (argument checks, unregister
// tokens counted, held weakly) and unregistering.
// expect: tag=[object FinalizationRegistry] register=undefined register2=undefined noToken=undefined unregister=true again=false unknown=false same=!TypeError badTarget=!TypeError badToken=!TypeError badUnreg=!TypeError noCb=!TypeError noNew=!TypeError brand=!TypeError len=1,2,1

var out = [];
function t(n, f) { try { out.push(n + "=" + f()); } catch (e) { out.push(n + "=!" + e.name); } }
var fr = new FinalizationRegistry(function () {});
var tok = {}, tgt = {};
t("tag", () => Object.prototype.toString.call(fr));
t("register", () => fr.register(tgt, "held", tok));
t("register2", () => fr.register({}, 1, tok));
t("noToken", () => fr.register({}, 2));
t("unregister", () => fr.unregister(tok));
t("again", () => fr.unregister(tok));
t("unknown", () => fr.unregister({}));
t("same", () => fr.register(tgt, tgt));
t("badTarget", () => fr.register(1, 2));
t("badToken", () => fr.register({}, 1, 5));
t("badUnreg", () => fr.unregister(5));
t("noCb", () => new FinalizationRegistry());
t("noNew", () => FinalizationRegistry(function () {}));
t("brand", () => FinalizationRegistry.prototype.register.call({}, {}, 1));
t("len", () => [FinalizationRegistry.length, fr.register.length, fr.unregister.length].join());
out.join(" ");
