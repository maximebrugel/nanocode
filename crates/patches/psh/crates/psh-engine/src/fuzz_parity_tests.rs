//! Regressions found by differential fuzzing against bash 5.2.

use super::run_on_test_host as run;

fn out(s: &str) -> String {
    String::from_utf8_lossy(&run(s).1).into_owned()
}

#[test]
fn a_brace_expanded_declaration_argument_splits_like_a_command_word() {
    // bash: the words brace expansion makes lose their assignment flag.
    let s = "arr=(x 'y z' w.txt); export E=\"\"\"${arr[@]}\"-{a,b}c\"-x\"; echo \"$E\"";
    assert_eq!(out(s), "x\n");
    assert_eq!(out("x='m -1'; declare v={a,b}$x 2>/dev/null; echo \"$v\""), "bm\n");
    assert_eq!(out("x='m -1'; declare v={a,b}\"$x\"; echo \"$v\""), "bm -1\n");
    // Without a brace expansion it stays an assignment.
    assert_eq!(out("a=(x 'y z'); export E=\"${a[@]}\"; echo \"$E\""), "x y z\n");
    assert_eq!(out("f() { local a={1..3}; echo $a; }; f"), "3\n");
}

fn err(s: &str) -> String {
    String::from_utf8_lossy(&run(s).2).into_owned()
}

#[test]
fn a_simple_command_takes_the_line_its_second_token_ends_on() {
    assert_eq!(out("echo 'a\nb' $LINENO"), "a\nb 2\n");
    assert_eq!(out("echo $LINENO 'a\nb'"), "1 a\nb\n");
    assert!(err("nosuch 'a\nb'").contains("line 2: nosuch"));
    assert!(err("nosuch \\\nb").contains("line 2: nosuch"));
    assert!(err("{ nosuch 'a\nb'; }").contains("line 2: nosuch"));
    assert!(err("x=1 nosuch 'a\nb'").contains("line 1: nosuch"));
    assert!(err("nosuch a 'b\nc'").contains("line 1: nosuch"));
    assert!(err("nosuch\necho").contains("line 1: nosuch"));
}

#[test]
fn an_external_commands_redirections_expand_in_its_child() {
    assert_eq!(out("n=0; cat >/dev/null <<E\n$((n++))\nE\necho \"$n\""), "0\n");
    assert_eq!(out("n=0; cat <<< $((n++)); echo \"$n\""), "0\n0\n");
    assert_eq!(out("cat >${x:=g} </dev/null; echo \"[$x]\""), "[]\n");
    assert_eq!(out("cat {fd}>/dev/null </dev/null; echo \"[$fd]\""), "[]\n");
    assert_eq!(out("n=0; command cat >/dev/null <<< $((n++)); echo \"$n\""), "0\n");
    assert_eq!(out("n=0; nosuch >/dev/null <<< $((n++)) 2>/dev/null; echo \"$n\""), "0\n");
    // Builtins and functions run in the shell, so theirs do.
    assert_eq!(out("n=0; true <<< $((n++)); echo \"$n\""), "1\n");
    assert_eq!(out("n=0; f() { :; }; f <<< $((n++)); echo \"$n\""), "1\n");
    assert_eq!(out("exec {fd}>/dev/null; echo \"[$fd]\""), "[10]\n");
}

#[test]
fn an_expansion_error_in_an_external_commands_redirection_fails_only_it() {
    assert_eq!(out("cat <<< ${zz?boom} 2>/dev/null; echo \"rc=$?\""), "rc=127\n");
    assert_eq!(out("cat <<< $((1/0)) 2>/dev/null; echo \"rc=$?\""), "rc=1\n");
    assert_eq!(out("cat <<E 2>/dev/null\n${zz?boom}\nE\necho \"rc=$?\""), "rc=127\n");
    assert_eq!(out("cat <<E 2>/dev/null || echo \"or $?\"\n$((1/0))\nE\necho \"after $?\""), "or 1\nafter 0\n");
    // A builtin's, a function's and a compound's are the shell's own, so the line is
    // abandoned.
    assert_eq!(out("true <<E 2>/dev/null || echo or\n$((1/0))\nE\necho \"after $?\""), "after 1\n");
    assert_eq!(out("f() { cat; }; f <<E 2>/dev/null; echo same\n$((1/0))\nE\necho \"after $?\""), "after 1\n");
    assert_eq!(out("{ cat; } <<E 2>/dev/null; echo same\n$((1/0))\nE\necho \"after $?\""), "after 1\n");
    assert_eq!(out("true <<< ${zz?boom} 2>/dev/null; echo \"rc=$?\""), "");
}

#[test]
fn a_command_word_starting_with_a_percent_resumes_a_job() {
    let (_, o, e) = run("%y; echo \"rc=$?\"; %y() { echo fn; }; %y");
    assert_eq!(String::from_utf8_lossy(&o), "rc=1\n");
    assert!(String::from_utf8_lossy(&e).contains("fg: no job control"));
    assert!(String::from_utf8_lossy(&run("%1 &\nwait").2).contains("bg: no job control"));
}

#[test]
fn a_percent_command_word_that_is_its_own_pipeline_stage_is_a_plain_name() {
    // bash looks a `%job` up only in a shell that has not forked for the command.
    let (_, o, e) = run("echo x | %1; echo \"r=$?\"");
    assert_eq!(String::from_utf8_lossy(&o), "r=127\n");
    assert!(String::from_utf8_lossy(&e).contains("%1: command not found"));
    // A stage that runs it from a group or a function still resumes it.
    assert!(err("{ %1; } | cat").contains("fg: no job control"));
    assert!(err("f() { %1; }; f | cat").contains("fg: no job control"));
}

#[test]
fn printf_time_of_an_empty_or_unrepresentable_argument_is_the_epoch() {
    // Only a missing argument is now; an empty one is 0 and a time past what
    // localtime can represent falls back to 0 (out of range ones only warn).
    let (s, o, e) = run(
        "export TZ=UTC; printf '%(%Y)T|' '' 99999999999999999999 67768036191676800 0x10; \
         printf '%(%S)T\\n' 0x10",
    );
    assert_eq!(String::from_utf8_lossy(&o), "1970|1970|1970|1970|16\n");
    assert_eq!(s, 0);
    let e = String::from_utf8_lossy(&e);
    assert!(e.contains("warning: 99999999999999999999: Numerical result out of range"));
}

#[test]
fn printf_floats_are_long_doubles() {
    // bash converts with strtold, so values past f64's range keep their digits, and
    // nan and inf print in lowercase (uppercase for F E G A).
    let (_, o, e) = run(
        "printf '%e|%g|%.20f|%a\\n' 1e400 1e-4950 0.1 0.1; \
         printf '%f|%F|%E|%g|%+G|%5.1f|%-5e|%A\\n' nan -nan inf -inf nan inf -inf nan; \
         printf -v v '%.0f' 1e400; echo ${#v}; printf '%.2f\\n' -n",
    );
    assert_eq!(
        String::from_utf8_lossy(&o),
        "1.000000e+400|1.09356e-4950|0.10000000000000000000|0xc.ccccccccccccccdp-7\n\
         nan|-NAN|INF|-inf|+NAN|  inf|-inf |NAN\n401\n0.00\n"
    );
    let e = String::from_utf8_lossy(&e);
    assert!(e.contains("warning: 1e-4950: Numerical result out of range"));
    assert!(e.contains("-n: invalid number"));
}

#[test]
fn printf_floats_take_what_strtod_converts() {
    let (_, o, e) = run("printf '%.2f\\n' 2228e9x 12e3abc 1.5x 0x1Ap1 1e e3 -n");
    assert_eq!(
        String::from_utf8_lossy(&o),
        "2228000000000.00\n12000.00\n1.50\n52.00\n1.00\n0.00\n0.00\n"
    );
    let e = String::from_utf8_lossy(&e);
    assert!(e.contains("2228e9x: invalid number") && e.contains("1e: invalid number"));
    assert!(!e.contains("0x1Ap1"));
}

#[test]
fn a_forked_simple_command_that_fails_to_expand_runs_the_inherited_exit_trap() {
    // bash's child for a pipeline stage or job holds the trap until it runs the
    // command, so the trap's output goes down the pipe.
    let s = "trap 'echo bye' EXIT; echo $((1 % 0)) 2>/dev/null | { read -r q; echo \"got $q\"; }";
    assert_eq!(out(s), "got bye\nbye\n");
    assert_eq!(out("trap 'echo bye $?' EXIT; : ${u?x} 2>/dev/null | cat; echo after"), "bye 127\nafter\nbye 0\n");
    assert_eq!(out("trap 'echo bye' EXIT; echo a | x=$((1/0)) 2>/dev/null | cat; echo after"), "bye\nafter\nbye\n");
    assert_eq!(out("trap 'echo bye' EXIT; echo $((1/0)) 2>/dev/null & wait; echo after"), "bye\nafter\nbye\n");
    // Not for a subshell, a group, a command substitution, a function's body, a failing
    // builtin or a prefix assignment to a readonly variable.
    assert_eq!(out("trap 'echo bye' EXIT; ( echo $((1/0)) ) 2>/dev/null | cat; echo after"), "after\nbye\n");
    assert_eq!(out("trap 'echo bye' EXIT; { echo $((1/0)); } 2>/dev/null | cat; echo after"), "after\nbye\n");
    assert_eq!(out("trap 'echo bye' EXIT; x=$(echo $((1/0))) 2>/dev/null; echo after"), "after\nbye\n");
    assert_eq!(out("trap 'echo bye' EXIT; f(){ echo $((1/0)); }; f 2>/dev/null | cat; echo after"), "after\nbye\n");
    assert_eq!(out("trap 'echo bye' EXIT; cd /nope 2>/dev/null | cat; echo after"), "after\nbye\n");
    assert_eq!(out("trap 'echo bye' EXIT; readonly r=1; r=2 echo hi 2>/dev/null | cat; echo after"), "hi\nafter\nbye\n");
    assert_eq!(out("trap 'echo bye' EXIT; ( echo $((1/0)) 2>/dev/null | cat ); echo after"), "after\nbye\n");
    assert_eq!(out("trap 'echo bye' EXIT; (trap 'echo in' EXIT; echo $((1/0)) 2>/dev/null | cat); echo after"), "in\nin\nafter\nbye\n");
}

#[test]
fn an_unset_parameter_error_in_a_forked_simple_command_exits_127() {
    // bash's child for the command is a copy of the shell, so it ends as `-c` does.
    let s = ": ${u?x} 2>/dev/null | cat; echo ${PIPESTATUS[@]}; f() { : ${u?x}; }; f 2>/dev/null | cat; echo ${PIPESTATUS[@]}";
    assert_eq!(out(s), "127 0\n127 0\n");
    assert_eq!(out("set -u; echo $zz 2>/dev/null & wait $!; echo $?"), "127\n");
    // A subshell, a substitution, a compound stage and errexit end with 1.
    assert_eq!(out("set -u; (echo $zz) 2>/dev/null; echo $?; x=$(echo $zz) 2>/dev/null; echo $?"), "1\n1\n");
    assert_eq!(out("set -u; echo hi | while read l; do echo $zz; done 2>/dev/null; echo ${PIPESTATUS[@]}"), "0 1\n");
    assert_eq!(out("set -e; : ${u?x} 2>/dev/null | cat; echo ${PIPESTATUS[@]}"), "1 0\n");
}

#[test]
fn a_negative_subscript_counts_from_the_end_of_an_indexed_array_only() {
    let o = |s: &str| String::from_utf8_lossy(&run(s).1).into_owned();
    // On a scalar a negative reference points nowhere; an assignment sets element 0.
    assert_eq!(o("a=x; echo \"[${a[-1]}]\" $?"), "[] 0\n");
    assert!(err("a=x; echo \"${a[-1]}\"").contains("a: bad array subscript"));
    assert_eq!(o("a=foo; a[-1]=x; declare -p a"), "declare -a a=([0]=\"x\")\n");
    assert_eq!(o("a=foo; unset 'a[-1]'; echo $? $a"), "1 foo\n");
    assert!(err("a=foo; unset 'a[-1]'").contains("unset: a: not an array variable"));
    assert_eq!(o("a=foo; unset 'a[0]'; echo ${a-gone}"), "gone\n");
    assert_eq!(o("unset 'z[-1]'; echo $?"), "0\n");
    // `${#a[-9]}` abandons the line.
    assert_eq!(o("a=(foo); echo ${#a[-2]}; echo no\necho next"), "next\n");
    assert!(err("a=(foo); echo ${#a[-2]}").contains("-2]: bad array subscript"));
    assert_eq!(o("a=(foo bar); echo ${#a[-1]}"), "3\n");
}

#[test]
fn a_subscript_that_is_not_arithmetic_ends_the_script() {
    let o = |s: &str| String::from_utf8_lossy(&run(s).1).into_owned();
    assert_eq!(run("m=(1); m[\"x y\"]=3\necho next").0, 1);
    assert_eq!(o("m=(1); m[\"x y\"]=3\necho next"), "");
    assert_eq!(o("a=(1); echo ${a[1+]}\necho next"), "");
    assert_eq!(o("m=(1); unset 'm[x y]'\necho next"), "");
    assert_eq!(o("( m[\"x y\"]=3; echo in ); echo $?"), "1\n");
    // A bad compound element only warns and is skipped.
    assert_eq!(o("m=(1); m+=([-5]=v [\"\"]=w); echo $?; declare -p m"), "0\ndeclare -a m=([0]=\"1\")\n");
    assert!(err("m=(1); m+=([-5]=v)").contains("[-5]=v: bad array subscript"));
    // An empty associative key in an assignment abandons the line.
    assert_eq!(o("declare -A m; m[\"\"]=v; echo no\necho next"), "next\n");
    assert!(err("declare -A m; m[\"\"]=v").contains("m[\"\"]: bad array subscript"));
}

#[test]
fn bash_fuzz6_heredoc_continuations_join_names() {
    assert_eq!(out("a=1 ab=2\ncat <<E\n$a\\\nb.\nE"), "2.\n");
    assert_eq!(out("a=1\ncat <<E\nx $a\\\n plain\nE"), "x 1 plain\n");
    assert_eq!(out("a=v\ncat <<E\n$\\\n'sq $a'\nE"), "$'sq v'\n");
    assert_eq!(out("a=v\ncat <<E\n$\\\n\"dq $a\"\nE"), "$\"dq v\"\n");
}


#[test]
fn bash_fuzz6_arrays() {
    let o = |s: &str| String::from_utf8_lossy(&run(s).1).into_owned();
    // An empty associative key in a compound assignment warns and skips the element.
    assert_eq!(o("declare -A q; q+=([\"\"]=1); echo $? ${#q[@]}"), "0 0\n");
    assert_eq!(o("declare -A w=([\"\"]=1 [x]=2); echo $? ${!w[@]}"), "0 x\n");
    assert!(err("declare -A w; w=([\"\"]=1 [y]=3)").contains("[\"\"]=1: bad array subscript"));
    assert!(err("declare -A w=([\"\"]=1)").contains("['']='1': bad array subscript"));
    // -v on an unset name evaluates the subscript as arithmetic.
    assert_eq!(o("[[ -v m[k=5] ]]; echo $? $k"), "1 5\n");
    assert_eq!(o("[[ -v m[\"q z\"] ]] || echo same; echo no\necho next"), "next\n");
    assert_eq!(run("a='*.t'\n[[ -v m[\"$a\"] ]] || echo unset").0, 1);
    assert!(!err("[[ -v m[1+] ]]").contains("[[: "));
    assert_eq!(o("m[x]+=v\n[[ -v m[$k] ]] && echo set || echo unset"), "unset\n");
    // On an associative array @ and * are keys.
    assert_eq!(o("declare -A m=([a]=1); [[ -v m[*] ]]; echo $?"), "1\n");
    assert_eq!(o("declare -A m=([@]=1); [[ -v m[@] ]]; echo $?"), "0\n");
    // A scalar sliced past its end as [@] gives no word.
    assert_eq!(o("a=''; b=(\"${a[@]:1}\"); echo ${#b[@]}"), "0\n");
    assert_eq!(o("a='xy'; b=(\"${a[@]:2}\"); echo ${#b[@]}"), "1\n");
    assert_eq!(o("a='xy'; b=(\"${a[*]:5}\"); echo ${#b[@]}"), "1\n");
}
