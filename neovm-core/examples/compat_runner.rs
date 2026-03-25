use neovm_core::emacs_core::{
    Context, format_eval_result_bytes_with_eval, parse_forms, print_expr,
};
use std::fs;
use std::io::{self, Write};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: compat_runner <forms-file>");
        std::process::exit(2);
    };

    let source = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) => {
            eprintln!("failed to read {path}: {err}");
            std::process::exit(2);
        }
    };

    let forms = match parse_forms(&source) {
        Ok(forms) => forms,
        Err(err) => {
            eprintln!("failed to parse forms: {err}");
            std::process::exit(2);
        }
    };

    let mut evaluator = Context::new();
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    for (index, form) in forms.iter().enumerate() {
        let result = evaluator.eval_expr(form);
        out.write_all((index + 1).to_string().as_bytes())
            .expect("write index");
        out.write_all(b"\t").expect("write tab");
        out.write_all(print_expr(form).as_bytes())
            .expect("write form");
        out.write_all(b"\t").expect("write tab");
        out.write_all(&format_eval_result_bytes_with_eval(&evaluator, &result))
            .expect("write result");
        out.write_all(b"\n").expect("write newline");
    }
    out.flush().expect("flush output");
}
