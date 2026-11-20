//! Function recursion: psh stops at its nesting limit, near the depth where bash's
//! own stack runs out (about 5,290 levels at the default 8 MiB `ulimit -s`), with an
//! error instead of a crash, and its own stack holds every level up to that limit.

use std::process::Command;

fn sh(src: &str) -> (String, String, Option<i32>) {
    let out = Command::new(env!("CARGO_BIN_EXE_psh"))
        .arg("-c")
        .arg(src)
        .env_remove("FUNCNEST")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

#[test]
fn a_function_recurses_as_deep_as_bash_does() {
    let src = "f() { n=$((n+1)); ((n < 4900)) && f; }; f; echo $n";
    assert_eq!(sh(src), ("4900\n".into(), String::new(), Some(0)));
}

#[test]
fn levels_that_each_run_eval_case_and_a_substitution_fit_the_stack() {
    let src = "f() { case $1 in 2000) echo done ;; *) if true; then eval 'f $(echo $(( $1 + 1 )))'; fi ;; esac; }; f 0";
    assert_eq!(sh(src), ("done\n".into(), String::new(), Some(0)));
}

#[test]
fn endless_recursion_stops_at_the_limit_without_crashing() {
    let (out, err, code) = sh("f() { f; }; f; echo after");
    assert!(
        err.contains("f: maximum function nesting level exceeded (5000)"),
        "{err}"
    );
    // An exit by signal (a stack overflow) has no code.
    assert!(code.is_some(), "{out} {err}");
}
