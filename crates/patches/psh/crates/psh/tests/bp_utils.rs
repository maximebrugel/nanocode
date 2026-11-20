//! Utilities matching GNU on cases found by comparing psh with bash.

use std::process::Command;

fn sh(src: &str) -> (String, i32) {
    let dir = std::env::temp_dir().join(format!(
        "psh-bp-utils-{}-{:x}",
        std::process::id(),
        src.len() * 31 + src.bytes().map(usize::from).sum::<usize>()
    ));
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
fn xargs_runs_the_items_before_an_unmatched_quote() {
    assert_eq!(
        sh("printf '%s\\n' x x \"q'q\" | xargs echo; echo $?"),
        ("x x\n1\n".into(), 0)
    );
}

#[test]
fn basename_takes_a_dash_operand_as_the_suffix() {
    assert_eq!(sh("basename end -x; basename x -s"), ("end\nx\n".into(), 0));
}

#[test]
fn comm_checks_order_once_a_line_is_unpairable() {
    let src = "printf '%s\\n' 3 1 2 10 b a b > f1; printf '%s\\n' b a b c | comm -12 - f1 2>&1";
    assert_eq!(
        sh(src),
        (
            "comm: file 2 is not in sorted order\nb\ncomm: file 1 is not in sorted order\na\nb\ncomm: input is not in sorted order\n".into(),
            1
        )
    );
    let src = "printf '%s\\n' 3 1 2 10 b a b > f1; sed s/a/X/ f1 | comm -12 - f1 2>&1";
    assert_eq!(sh(src), ("3\n1\n2\n10\nb\nb\n".into(), 0));
}

#[test]
fn join_keeps_a_trailing_empty_field_and_empty_lines() {
    let src = "printf 'k1 v1\\nk2 v2\\n' > f2; printf 'k1 \\n' | join - f2";
    assert_eq!(sh(src), ("k1  v1\n".into(), 0));
    let src = "printf 'k1 v1\\nk2 v2\\nb y\\n' > f2; echo | join - f2 2>/dev/null";
    assert_eq!(sh(src), (String::new(), 1));
}


#[test]
fn numfmt_pads_and_converts_one_field() {
    assert_eq!(sh("numfmt --padding=8 42; numfmt --padding=-4 7"), ("      42\n7   \n".into(), 0));
    let src = "printf 'a 2048\\nb 10\\n' | numfmt --field=2 --to=iec; echo 123456 | numfmt --grouping";
    assert_eq!(sh(src), ("a 2.0K\nb 10\n123456\n".into(), 0));
}

#[test]
fn wc_writes_each_count_before_the_next_inputs_error() {
    assert_eq!(
        sh("printf '1\\n2\\n' > f; wc -l f nope f 2>&1; echo $?"),
        (
            "2 f\nwc: nope: No such file or directory\n2 f\n4 total\n1\n".into(),
            0
        )
    );
}

#[test]
fn xargs_reports_a_missing_command_in_its_own_words() {
    assert_eq!(
        sh("echo a | xargs nosuchcmd 2>&1; echo $?"),
        ("xargs: nosuchcmd: No such file or directory\n127\n".into(), 0)
    );
}
