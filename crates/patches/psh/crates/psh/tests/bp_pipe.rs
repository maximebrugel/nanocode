//! Pipeline stages matching bash: each stage opens its redirections before any
//! stage's program runs, and stages that wait on each other still finish.

use std::process::Command;

fn sh(name: &str, src: &str) -> (String, i32) {
    let dir = std::env::temp_dir().join(format!("psh-bp-pipe-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_psh"))
        .arg("-c")
        .arg(src)
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn a_later_stage_opens_its_output_before_the_first_program_runs() {
    // The redirection's file opens on a host thread; the first stage waits for it.
    let src = "for i in 1 2 3 4 5 6 7 8 9 10; do rm -f out; ls | cat > out; cat out; done";
    assert_eq!(sh("cat-out", src), ("out\n".repeat(10), 0));
}

#[test]
fn a_file_the_later_program_writes_is_the_last_word() {
    let src = "mkdir -p d/e; touch d/a.txt d/e/b.txt d/c.md; \
               ls d | tee out | comm -12 - f1 > out; cat out";
    assert_eq!(sh("tee-out", src), ("a.txt\nc.md\ne\n".into(), 0));
}

#[test]
fn a_stage_that_reads_its_input_before_its_command_still_gets_it() {
    assert_eq!(sh("subst", "echo x | echo \"$(cat)\""), ("x\n".into(), 0));
    assert_eq!(
        sh("subst3", "echo x | echo \"<$(cat)>\" | echo \"[$(cat)]\""),
        ("[<x>]\n".into(), 0)
    );
    assert_eq!(
        sh(
            "subst-mid",
            "seq 3 | { read a; echo \"$a\"; cat; } | echo \"$(tr '\\n' ,)\""
        ),
        ("1,2,3,\n".into(), 0)
    );
}

#[test]
fn a_stage_three_forks_later_opens_its_output_after_the_first_program_ran() {
    // bash forks `grep` three stages after `ls`, which usually lists the directory
    // before `> out` exists; two stages on (`cat > out`) it is already there.
    let src = "ls | cat | sort | grep -o '[a-z]' > out; cat out; echo; \
               ls | cat | cat > out2; cat out2";
    assert_eq!(sh("reach", src), ("\nout\nout2\n".into(), 0));
}

#[test]
fn a_write_after_a_forked_program_lets_the_reader_take_its_lines_first() {
    // bash waits for `cat`'s exit while `head` takes its lines and exits, so `echo x`
    // usually meets the closed pipe (SIGPIPE).
    let src = "printf '1\\n2\\n3\\n4\\n' > f; set -o pipefail; \
               ( cat f; echo x ) 2>&1 | head -3; echo \"$? ${PIPESTATUS[*]}\"";
    assert_eq!(sh("sigpipe", src), ("1\n2\n3\n141 141 0\n".into(), 0));
    let src = "printf '1\\n2\\n' > f; set -o pipefail; ( cat f; echo x ) | cat; echo $?";
    assert_eq!(sh("reader-stays", src), ("1\n2\nx\n0\n".into(), 0));
    let src = "printf '1\\n2\\n' > f; ( cat f; echo x ) | sleep 0; echo $?";
    assert_eq!(sh("reader-never-reads", src), ("0\n".into(), 0));
}

#[test]
fn a_utility_writing_a_pipe_whose_reader_left_ends_by_sigpipe() {
    let src = "echo 2 > f; set -o pipefail; { echo 1; sleep 0.2; cat f; } | head -1; \
               echo \"${PIPESTATUS[*]}\"";
    assert_eq!(sh("epipe", src), ("1\n141 0\n".into(), 0));
}

#[test]
fn a_forked_program_starting_lets_the_reader_take_its_lines_first() {
    // bash forks the last `cat` after `echo 3`; `head` takes line 3 and exits while
    // it starts, so its write meets the closed pipe (SIGPIPE).
    let src = "echo 2 > f; set -o pipefail; { echo 1; cat f; echo 3; cat f; } | head -3; \
               echo \"${PIPESTATUS[*]}\"";
    assert_eq!(sh("start", src), ("1\n2\n3\n141 0\n".into(), 0));
    let src = "echo 2 > f; set -o pipefail; for x in x x; do echo \"<$x>\"; : > out; cat f; \
               done 2>&1 | head -3 > /dev/null; echo $?";
    assert_eq!(sh("loop", src), ("141\n".into(), 0));
    let src = "echo 2 > f; set -o pipefail; { echo 1; cat f; echo 3; cat f; } | cat; echo $?";
    assert_eq!(sh("start-reader-stays", src), ("1\n2\n3\n2\n0\n".into(), 0));
}
