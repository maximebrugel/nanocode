use std::{
    collections::BTreeMap,
    env,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use nanocode_compression::deflate::{
    Deflate, DeflateConfig, DeflateFlush, DeflateStatus, Strategy,
};
use rustpython_compiler::{CompileOpts, Mode, compile};
use rustpython_compiler_core::frozen::FrozenCodeObject;

const DATA: &[&str] = &["matplotlib/mpl-data", "tzdata"];

// Site packages compiled at optimize 2 (no docstrings, no asserts), as
// CPython's -OO would: their docstrings are a third of their code.
const OPTIMIZED: &[&str] = &["matplotlib", "mpl_toolkits", "numpy", "pandas"];

// Testing helpers keep their asserts: scripts check results with them.
const KEEP_ASSERTS: &[&str] = &["numpy.testing", "pandas._testing"];

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .expect("data directory")
        .map(|entry| entry.expect("entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_none_or(|extension| extension != "py") {
            out.push(path);
        }
    }
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let config = DeflateConfig {
        level: 9,
        window_bits: -15,
        mem_level: 9,
        strategy: Strategy::Default,
    };
    let mut deflate = Deflate::new(config).expect("deflate");
    let mut out = vec![0; data.len() + data.len() / 8 + 1024];
    let status = deflate
        .deflate(data, &mut out, DeflateFlush::Finish)
        .expect("deflate");
    assert_eq!(status, DeflateStatus::StreamEnd);
    out.truncate(usize::try_from(deflate.total_out()).expect("size"));
    out
}

fn freeze(dir: &Path, parent: &str, optimize: u8, modules: &mut BTreeMap<String, (bool, Vec<u8>)>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .expect("module directory")
        .map(|entry| entry.expect("entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("utf-8 file name");
        if path.is_dir() {
            let child = if parent.is_empty() {
                file_name.to_owned()
            } else {
                format!("{parent}.{file_name}")
            };
            let optimize = if parent.is_empty() && OPTIMIZED.contains(&file_name) {
                2
            } else {
                optimize
            };
            freeze(&path, &child, optimize, modules);
            continue;
        }
        let Some(stem) = file_name.strip_suffix(".py") else {
            continue;
        };
        let package = stem == "__init__";
        let name = if package {
            parent.to_owned()
        } else if parent.is_empty() {
            stem.to_owned()
        } else {
            format!("{parent}.{stem}")
        };
        let mut source = fs::read_to_string(&path).expect("python source");
        if source.starts_with('\u{feff}') {
            source.drain(..'\u{feff}'.len_utf8());
        }
        let keep = KEEP_ASSERTS.iter().any(|package| {
            name.strip_prefix(package)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
        });
        let opts = CompileOpts {
            optimize: if keep { 0 } else { optimize },
            ..CompileOpts::default()
        };
        let code = match compile(&source, Mode::Exec, &name, opts) {
            Ok(code) => code,
            Err(_) if stem.starts_with("badsyntax_") || parent.ends_with(".encoded_modules") => {
                continue;
            }
            Err(error) => panic!("Python compile error from {}: {error}", path.display()),
        };
        modules.insert(name, (package, FrozenCodeObject::encode(&code).bytes));
    }
}

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let site = manifest.join("../python/site");
    let lib = Path::new(rustpython_pylib::LIB_PATH);
    println!("cargo:rerun-if-changed={}", site.display());
    println!("cargo:rerun-if-changed={}", lib.display());
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));

    let mut modules = BTreeMap::new();
    freeze(lib, "", 0, &mut modules);
    freeze(&site, "", 0, &mut modules);

    let mut blob = Vec::new();
    let mut table = String::from("pub static MODULES: &[(&str, bool, u32, u32)] = &[\n");
    for (name, (package, code)) in &modules {
        writeln!(
            table,
            "    ({name:?}, {package}, {}, {}),",
            blob.len(),
            code.len()
        )
        .expect("format");
        blob.extend_from_slice(code);
    }
    table.push_str("];\n");

    let mut files = Vec::new();
    for dir in DATA {
        walk(&site.join(dir), &mut files);
    }
    table.push_str("pub static FILES: &[(&str, u32, u32, u32)] = &[\n");
    for path in &files {
        let data = fs::read(path).expect("data file");
        let packed = deflate(&data);
        let name = path
            .strip_prefix(&site)
            .expect("under site")
            .to_str()
            .expect("utf-8 path");
        writeln!(
            table,
            "    ({name:?}, {}, {}, {}),",
            data.len(),
            blob.len(),
            packed.len()
        )
        .expect("format");
        blob.extend_from_slice(&packed);
    }
    table.push_str("];\n");
    writeln!(table, "pub const DATA_LEN: usize = {};", blob.len()).expect("format");
    u32::try_from(blob.len()).expect("the data blob fits 32-bit offsets");
    fs::write(out.join("python-data.bin"), &blob).expect("write");
    fs::write(out.join("site_data.rs"), table).expect("write");
}
