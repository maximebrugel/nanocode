//! Behaviour pinned by performance changes: each fast path gives what the slow one did.

use super::run_on_test_host as run;

fn out(s: &str) -> String {
    String::from_utf8_lossy(&run(s).1).into_owned()
}

#[test]
fn an_array_length_counts_every_kind_of_variable() {
    assert_eq!(
        out("a=(x '' z); a[9]=q; echo ${#a[@]} ${#a[*]} \"${#a[@]}\""),
        "4 4 4\n"
    );
    assert_eq!(out("declare -A m=([k]=1 [j]=2); echo ${#m[@]}"), "2\n");
    assert_eq!(
        out("s=abc; e=; echo ${#s[@]} ${#e[@]} ${#nosuch[@]}"),
        "1 1 0\n"
    );
    assert_eq!(
        out("declare -a d; declare -A h; echo ${#d[@]} ${#h[*]}"),
        "0 0\n"
    );
    assert_eq!(out("a=(1 2 3); declare -n r=a; echo ${#r[@]}"), "3\n");
    assert_eq!(out("set -u; a=(); echo ${#a[@]}"), "0\n");
    assert_eq!(out("f() { echo ${#FUNCNAME[@]}; }; g() { f; }; g"), "2\n");
    assert_eq!(
        out(
            "a=(); for i in {1..3000}; do a+=($i); done; n=0; for ((i=0; i<${#a[@]}; i++)); do ((n+=a[i])); done; echo $n"
        ),
        "4501500\n"
    );
}

#[test]
fn a_replacement_of_a_fixed_length_pattern_finds_every_match() {
    assert_eq!(
        out("s=item1,item22,itemitem; echo ${s//item/x} ${s/item/x}"),
        "x1,x22,xx x1,item22,itemitem\n"
    );
    assert_eq!(
        out("s=abcabc; echo ${s//?b/X} ${s//[ac]/-} ${s//\\*/!}"),
        "XcXc -b--b- abcabc\n"
    );
    assert_eq!(out("s='a*b'; echo ${s//\\*/!}"), "a!b\n");
    assert_eq!(
        out("s=aaaa; echo ${s//aa/b} ${s//a*/b} ${s//*/b}"),
        "bb b b\n"
    );
    assert_eq!(
        out("s=; for i in {1..20000}; do s+=item$i,; done; t=${s//item/x}; echo ${#t}"),
        "128894\n"
    );
}

#[test]
fn a_case_word_split_once_matches_every_pattern_as_before() {
    assert_eq!(
        out("a=0 b=0 c=0 d=0; for ((i=0;i<2000;i++)); do w=item$((i%11)); case $w in item0|item1) ((a++));; item[23]) ((b++));; *5|*7) ((c++));; *) ((d++));; esac; done; echo $a $b $c $d"),
        "364 364 364 908\n"
    );
    assert_eq!(
        out("for w in hello hallo HALLO ''; do case $w in h?llo) echo q;; '') echo e;; *) echo o;; esac; done"),
        "q\nq\no\ne\n"
    );
    assert_eq!(
        out("shopt -s nocasematch; case HALLO in h[a-c]llo) echo y;; *) echo n;; esac"),
        "y\n"
    );
}
