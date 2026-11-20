//! Regressions from the coverage-2 and coverage-3 bash parity suites (bash 5.2).

use super::run_on_test_host as run;

fn out(s: &str) -> String {
    String::from_utf8_lossy(&run(s).1).into_owned()
}

fn nctool_stub<'a>(
    _ctx: &'a mut crate::util::UtilCtx<'_>,
    _argv: &'a [Vec<u8>],
) -> crate::util::LocalBoxFuture<'a, i32> {
    Box::pin(async { 0 })
}

static SANDBOX_UTILS: [crate::util::UtilEntry; 1] = [crate::util::UtilEntry {
    names: &["nctool"],
    mode: crate::util::Mode::PathFallback,
    destructive: false,
    run: nctool_stub,
}];

/// Stdout of `source` in a sandbox shell (no program files) with a `nctool` utility.
fn sandbox_out(source: &str) -> String {
    use crate::{Script, Shell, Step};
    let host = std::rc::Rc::new(crate::testhost::TestHost::new());
    let mut cfg = host.config();
    cfg.env = vec![(b"PATH".to_vec(), b"/usr/local/bin:/usr/bin:/bin".to_vec())];
    cfg.utilities = crate::util::UtilPolicy {
        entries: &SANDBOX_UTILS,
        sandbox: true,
        ..crate::util::UtilPolicy::default()
    };
    let mut shell = Shell::new(cfg);
    shell.start(Script::command(source));
    while let Step::Yield | Step::Blocked = shell.step(1000) {}
    String::from_utf8_lossy(&host.stdout()).into_owned()
}

#[test]
fn sandbox_utilities_and_builtins_stand_in_for_programs_in_usr_bin() {
    assert_eq!(
        sandbox_out("type -a echo; type -P echo nctool; type nctool; command -V nctool"),
        "echo is a shell builtin\necho is /usr/bin/echo\n/usr/bin/echo\n/usr/bin/nctool\n\
         nctool is /usr/bin/nctool\nnctool is /usr/bin/nctool\n"
    );
    assert_eq!(sandbox_out("command -pv nctool"), "/usr/bin/nctool\n");
    assert_eq!(
        sandbox_out("hash nctool; echo $?; hash -t nctool; type nctool; hash -d nctool"),
        "0\n/usr/bin/nctool\nnctool is hashed (/usr/bin/nctool)\n"
    );
    assert_eq!(sandbox_out("PATH=/nowhere; type -P echo; echo $?"), "1\n");
}

#[test]
fn break_and_continue_give_their_loop_status_zero() {
    assert_eq!(out("while true; do false || break; done; echo $?"), "0\n");
    assert_eq!(out("for i in 1; do false; break; done; echo $?"), "0\n");
    assert_eq!(out("for i in 1 2; do false; continue; done; echo $?"), "0\n");
    assert_eq!(out("until false; do (exit 4) || break; done; echo $?"), "0\n");
    assert_eq!(out("for a in 1; do for b in 1; do false; break 2; done; done; echo $?"), "0\n");
    assert_eq!(out("set -- a b c; while (( $# )); do shift 2 || break; done; echo $?"), "0\n");
}

#[test]
fn unset_error_names_the_subscript_as_written() {
    let e = |s: &str| String::from_utf8_lossy(&run(s).2).into_owned();
    assert!(e("a=(); : ${a[0]:?no elem}").ends_with("a[0]: no elem\n"));
    assert!(e("i=1; : ${a[$i]?}").ends_with("a[$i]: parameter not set\n"));
}

#[test]
fn prompt_transform_decodes_octal_and_expands_like_a_prompt() {
    assert_eq!(out(r"x='a\101b'; echo ${x@P}"), "aAb\n");
    assert_eq!(out(r"y='$((1+2)) $HOME'; HOME=/h; echo ${y@P}"), "3 /h\n");
    assert_eq!(out(r"a=('\101' '$((2*2))'); echo ${a[@]@P}"), "A 4\n");
}

#[test]
fn seeded_random_follows_bash_sequence() {
    assert_eq!(out("RANDOM=42; echo $RANDOM $RANDOM $RANDOM"), "17772 26794 1435\n");
    assert_eq!(out("RANDOM=0; a=$RANDOM; RANDOM=0; [ $a = $RANDOM ] && echo same"), "same\n");
}

#[test]
fn unicode_escapes_stay_escaped_outside_utf8_locales() {
    // bash decodes `$'…'` as it parses a line, so the locale is set a line before.
    assert_eq!(out("LC_ALL=C\necho $'\\u0043\\u00e9\\U0001F600'"), "C\\u00E9\\U0001F600\n");
    assert_eq!(out("LC_ALL=C.UTF-8\necho $'\\u00e9'"), "\u{e9}\n");
    assert_eq!(out(r"LC_ALL=C; x='\u00e9'; echo ${x@E}"), "\\u00E9\n");
}

#[test]
fn quoted_prefix_names_with_no_match_give_no_word() {
    assert_eq!(out(r#"set -- "${!zzq_@}"; echo $#; set -- "${!zzq_*}"; echo $#"#), "0\n1\n");
    assert_eq!(out(r#"zq_a=1 zq_b=2; set -- "${!zq_@}"; echo $# $2"#), "2 zq_b\n");
}

#[test]
fn printf_empty_time_format_is_the_locale_time() {
    assert_eq!(out("TZ=UTC printf '%()T\\n' 3661"), "01:01:01\n");
}
