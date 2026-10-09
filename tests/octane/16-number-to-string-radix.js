// Number.prototype.toString with a radix.
// for: earley-boyer, pdfjs, typescript, code-load
// expect: ff,11111111,-73,0,z,0.1,3635c9adc5dea00000,12
[(255).toString(16), (255).toString(2), (-255).toString(36), (0).toString(16), (35).toString(36),
 (0.5).toString(2), (1e21).toString(16), (12).toString(10)].join(",");
