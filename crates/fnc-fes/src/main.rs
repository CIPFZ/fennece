//! `fes` binary: see the `fnc_fes` library for behaviour and exit codes.

#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    // Crash reports: the default instance's logs folder until `-instance` is parsed;
    // %TEMP% if that is not writable (REL-02).
    let logs = fnc_core::paths::DataPaths::resolve("").logs_dir();
    fnc_core::crash::install("fes", logs, fnc_platform::os::version());
    fnc_platform::os::install_exception_filter();
    let env = fnc_fes::Env::from_process();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut err = std::io::stderr();
    let code = fnc_fes::run(std::env::args().skip(1).collect(), &env, &mut out, &mut err);
    let _ = std::io::Write::flush(&mut out);
    fnc_core::crash::mark_survived();
    ExitCode::from(code)
}
