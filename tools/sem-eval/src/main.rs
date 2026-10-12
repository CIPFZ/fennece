//! `sem-eval` — semantic search evaluation (Recall@10 / MRR over query→file samples,
//! SPEC ch.5 §5). Implemented with the semantic engine in milestone M5.

use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!("sem-eval: scheduled for milestone M5 (requires fnc-semantic)");
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    #[test]
    fn builds() {}
}
