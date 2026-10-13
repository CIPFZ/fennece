//! `fes` binary: see the `fnc_fes` library for behaviour and exit codes.

#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    let env = fnc_fes::Env::from_process();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut err = std::io::stderr();
    let code = fnc_fes::run(std::env::args().skip(1).collect(), &env, &mut out, &mut err);
    let _ = std::io::Write::flush(&mut out);
    ExitCode::from(code)
}
