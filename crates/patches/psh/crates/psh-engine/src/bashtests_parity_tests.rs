//! Regressions from snippets of GNU bash 5.2's own test suite (the bash-tests parity suite).

use super::run_on_test_host as run;

fn out(s: &str) -> String {
    String::from_utf8_lossy(&run(s).1).into_owned()
}

#[test]
fn bracket_collating_symbols_and_equivalence_classes() {
    let script = r#"
for p in a:'[[.a.]]' -:'[[.hyphen.]-9]' p:'[[.a.]-[.z.]]' -:'[[.-.]]' ' ':'[[.space.]]' \
    4:'[[.-.]-9]' c:'[[.yyz.][.a.]-z]' c:'[[.yyz.][.a.]-[.z.]]' p:'[[.a.]-[.zz.]p]' \
    p:'[[.aa.]-[.z.]p]' c:'[[.yyz.]cde]' a:'[[.cb.]a-Za]' b:'[[=b=]]' q:'[[.yyz.]]' \
    q:'[[.a.]-[.zz.]]'; do
  s=${p%%:*} pat=${p#*:}
  case $s in $pat) echo "y $pat" ;; *) echo "n $pat" ;; esac
done
case $'\t' in [[.space.][.tab.][.newline.]]) echo tab ;; esac
case abc in [[:alpha:]][[=b=]][[:ascii:]]) echo abc ;; esac
"#;
    assert_eq!(
        out(script),
        "y [[.a.]]\ny [[.hyphen.]-9]\ny [[.a.]-[.z.]]\ny [[.-.]]\ny [[.space.]]\ny [[.-.]-9]\n\
         y [[.yyz.][.a.]-z]\ny [[.yyz.][.a.]-[.z.]]\ny [[.a.]-[.zz.]p]\ny [[.aa.]-[.z.]p]\n\
         y [[.yyz.]cde]\ny [[.cb.]a-Za]\ny [[=b=]]\nn [[.yyz.]]\nn [[.a.]-[.zz.]]\ntab\nabc\n"
    );
}

#[test]
fn bracket_invalid_class_name_matches_nothing() {
    assert_eq!(
        out("case a in [[:al:]]) echo bad ;; *) echo ok ;; esac\ncase ':' in [[:al:]]) echo bad ;; *) echo ok ;; esac\n[[ x == [[:nope:]x] ]] && echo x\n"),
        "ok\nok\nx\n"
    );
}

#[test]
fn pattern_substitution_on_an_empty_value() {
    assert_eq!(
        out("var=\necho \"${var/#/x}\" \"${var/*/x}\" \"${var//*/x}\" \"${var/%/y}\" \"[${var/a*/x}]\"\n"),
        "x x x y []\n"
    );
}

#[test]
fn file_comparisons_with_an_empty_name_are_false() {
    assert_eq!(
        out("[[ '' -ef . ]]; echo $?\n[[ . -ef '' ]]; echo $?\n[ '' -nt . ]; echo $?\n[ . -nt '' ]; echo $?\n[[ . -ef . ]]; echo $?\n"),
        "1\n1\n1\n0\n0\n"
    );
}

#[test]
fn bash_cmds_is_an_empty_associative_array() {
    assert_eq!(out("declare -A\n"), "declare -A BASH_ALIASES=()\ndeclare -A BASH_CMDS=()\n");
}

#[test]
fn declare_f_names_show_readonly_functions() {
    assert_eq!(
        out("f() { :; }\ng() { :; }\nreadonly -f f\ndeclare -F\ndeclare -Fr\n"),
        "declare -fr f\ndeclare -f g\ndeclare -fr f\n"
    );
}

#[test]
fn assignment_before_export_of_that_name_persists() {
    assert_eq!(
        out("foo=\"\" export foo\ndeclare -p foo\nbar=1 export baz\necho \"[$bar]\"\n"),
        "declare -x foo=\"\"\n[]\n"
    );
}

#[test]
fn nested_time_runs_the_command() {
    assert_eq!(out("TIMEFORMAT=\ntime time echo hi\n"), "hi\n");
}

#[test]
fn backtick_escaped_backslash_before_newline_is_not_a_continuation() {
    assert_eq!(
        out("a=`printf '%s' 'x\\\\\ny'`\necho \"[$a]\"\nb=`echo p\\\nq`\necho \"[$b]\"\n"),
        "[x\\\ny]\n[pq]\n"
    );
}

#[test]
fn char_range_through_backquote_and_backslash() {
    assert_eq!(
        out("echo {Z..a}\necho {a..Z}\n"),
        "Z [  ] ^ _ ` a\na ` _ ^ ]  [ Z\n"
    );
}

#[test]
fn space_after_dollar_brace_that_is_no_command_list_parses() {
    assert_eq!(
        out("eval 'foo() { _; } >_[${ $() }] ;{ echo eval ok; }'\n"),
        "eval ok\n"
    );
}

#[test]
fn arith_store_to_an_empty_subscript_stores_nothing() {
    assert_eq!(
        out("echo $(( A[$key]++ ))\ndeclare -p A\necho $?\n"),
        "0\n1\n"
    );
}

#[test]
fn backquote_body_lines_count_from_its_command() {
    assert_eq!(
        out("true\necho `echo $LINENO\necho $LINENO`\n"),
        "3 4\n"
    );
}

#[test]
fn time_alone_prints_with_its_space() {
    assert_eq!(
        out("f() { time; echo; }\ndeclare -f f\n"),
        "f () \n{ \n    time ;\n    echo\n}\n"
    );
}

#[test]
fn length_and_bang_heads_with_operators() {
    assert_eq!(
        out("set -- a b\necho ${#-x} ${#/a/b} ${!-p} ${!:-q}\n(echo ${#+})\n(echo ${#%})\necho $?\n"),
        "2 2 p q\n1\n"
    );
}
