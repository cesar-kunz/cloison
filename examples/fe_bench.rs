// Front-end timing: lex, parse (skimming) and code generation of a file.
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let src = std::fs::read_to_string(path).unwrap();
    let n = 40;
    let t = std::time::Instant::now();
    for _ in 0..n {
        std::hint::black_box(toi::js::lex::lex(&src).unwrap());
    }
    let lex = t.elapsed() / n;
    let t = std::time::Instant::now();
    for _ in 0..n {
        let arena = toi::js::ast::Arenas::default();
        let mut toks = toi::js::lex::lex(&src).unwrap();
        std::hint::black_box(toi::js::parse::parse_with(&mut toks, &arena, true).unwrap());
    }
    let parse = t.elapsed() / n;
    let t = std::time::Instant::now();
    for _ in 0..n {
        std::hint::black_box(toi::js::compile_script_lazy(&src).unwrap());
    }
    let all = t.elapsed() / n;
    println!("lex {:?}  lex+parse {:?}  compile {:?}", lex, parse, all);
}
