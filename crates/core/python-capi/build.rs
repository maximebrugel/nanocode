use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    hash::{DefaultHasher, Hasher},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

/// Runs work on each item with up to jobs threads, each taking the next item as it finishes,
/// and returns the results in item order.
fn pool<T: Sync, R: Send>(jobs: usize, items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let mut done: Vec<(usize, R)> = thread::scope(|scope| {
        let workers: Vec<_> = (0..jobs.min(items.len()))
            .map(|_| {
                let (next, work) = (&next, &work);
                scope.spawn(move || {
                    let mut done = Vec::new();
                    loop {
                        let at = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(at) else {
                            break done;
                        };
                        done.push((at, work(item)));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("a compile worker"))
            .collect()
    });
    done.sort_by_key(|(at, _)| *at);
    done.into_iter().map(|(_, result)| result).collect()
}

fn sorted(dir: &Path, keep: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| keep(path))
        .collect();
    paths.sort();
    paths
}

fn listed(dir: &Path, file: &str) -> impl Iterator<Item = String> {
    fs::read_to_string(dir.join(file))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>()
        .into_iter()
}

fn archive(objects: &[PathBuf]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (index, object) in objects.iter().enumerate() {
        let bytes = fs::read(object).expect("a compiled object");
        let header = format!(
            "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            format!("{index}.o/"),
            0,
            0,
            0,
            644,
            bytes.len()
        );
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(&bytes);
        if bytes.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

fn leb(bytes: &[u8], at: &mut usize) -> u64 {
    let mut result = 0;
    let mut shift = 0;
    loop {
        let byte = bytes[*at];
        *at += 1;
        result |= u64::from(byte & 0x7f) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            return result;
        }
    }
}

fn index(bytes: &[u8], at: &mut usize) -> usize {
    usize::try_from(leb(bytes, at)).expect("an index that fits")
}

fn string(bytes: &[u8], at: &mut usize) -> String {
    let length = index(bytes, at);
    let text = String::from_utf8_lossy(&bytes[*at..*at + length]).into_owned();
    *at += length;
    text
}

fn limits(bytes: &[u8], at: &mut usize) {
    let flags = bytes[*at];
    *at += 1;
    leb(bytes, at);
    if flags & 1 != 0 {
        leb(bytes, at);
    }
}

const SYMBOL_TABLE: u8 = 8;
const UNDEFINED: u64 = 0x10;
const EXPLICIT_NAME: u64 = 0x40;
const TABLE_INDEX: [u8; 6] = [1, 2, 12, 18, 19, 24];
const ADDEND: [u8; 14] = [3, 4, 5, 8, 9, 11, 14, 15, 16, 17, 21, 22, 23, 25];

fn identifier(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|first| !first.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn address_taken(object: &Path) -> Vec<(String, usize)> {
    let bytes = fs::read(object).expect("a compiled object");
    let mut at = 8;
    let mut types = Vec::new();
    let mut imported = 0;
    let mut functions = Vec::new();
    let mut symbols: Vec<(u8, u64, usize, String)> = Vec::new();
    let mut taken = std::collections::BTreeSet::new();
    while at < bytes.len() {
        let id = bytes[at];
        at += 1;
        let size = index(&bytes, &mut at);
        let end = at + size;
        let mut p = at;
        match id {
            1 => {
                for _ in 0..index(&bytes, &mut p) {
                    p += 1;
                    let params = index(&bytes, &mut p);
                    p += params;
                    let results = index(&bytes, &mut p);
                    p += results;
                    types.push(params);
                }
            }
            2 => {
                for _ in 0..index(&bytes, &mut p) {
                    string(&bytes, &mut p);
                    string(&bytes, &mut p);
                    let kind = bytes[p];
                    p += 1;
                    match kind {
                        0 => {
                            leb(&bytes, &mut p);
                            imported += 1;
                        }
                        1 => {
                            p += 1;
                            limits(&bytes, &mut p);
                        }
                        2 => limits(&bytes, &mut p),
                        3 => p += 2,
                        _ => {
                            p += 1;
                            leb(&bytes, &mut p);
                        }
                    }
                }
            }
            3 => {
                for _ in 0..index(&bytes, &mut p) {
                    functions.push(index(&bytes, &mut p));
                }
            }
            0 => {
                let section = string(&bytes, &mut p);
                if section == "linking" {
                    leb(&bytes, &mut p);
                    while p < end {
                        let kind = bytes[p];
                        p += 1;
                        let length = index(&bytes, &mut p);
                        let next = p + length;
                        if kind == SYMBOL_TABLE {
                            for _ in 0..index(&bytes, &mut p) {
                                let kind = bytes[p];
                                p += 1;
                                let flags = leb(&bytes, &mut p);
                                match kind {
                                    1 => {
                                        let name = string(&bytes, &mut p);
                                        if flags & UNDEFINED == 0 {
                                            leb(&bytes, &mut p);
                                            leb(&bytes, &mut p);
                                            leb(&bytes, &mut p);
                                        }
                                        symbols.push((kind, flags, 0, name));
                                    }
                                    3 => {
                                        let target = index(&bytes, &mut p);
                                        symbols.push((kind, flags, target, String::new()));
                                    }
                                    _ => {
                                        let target = index(&bytes, &mut p);
                                        let name = if flags & UNDEFINED == 0
                                            || flags & EXPLICIT_NAME != 0
                                        {
                                            string(&bytes, &mut p)
                                        } else {
                                            String::new()
                                        };
                                        symbols.push((kind, flags, target, name));
                                    }
                                }
                            }
                        }
                        p = next;
                    }
                } else if section.starts_with("reloc.") {
                    leb(&bytes, &mut p);
                    for _ in 0..index(&bytes, &mut p) {
                        let kind = bytes[p];
                        p += 1;
                        leb(&bytes, &mut p);
                        let symbol = index(&bytes, &mut p);
                        if ADDEND.contains(&kind) {
                            leb(&bytes, &mut p);
                        }
                        if TABLE_INDEX.contains(&kind) {
                            taken.insert(symbol);
                        }
                    }
                }
            }
            _ => {}
        }
        at = end;
    }
    taken
        .into_iter()
        .filter_map(|symbol| {
            let (kind, flags, function, name) = symbols.get(symbol)?;
            (*kind == 0 && flags & UNDEFINED == 0 && *function >= imported && identifier(name))
                .then(|| (name.clone(), types[functions[function - imported]]))
        })
        .collect()
}

fn apple(target: &str) -> Option<(String, PathBuf)> {
    let (arch, system) = target.split_once("-apple-")?;
    let (os, sdk, suffix, variable) = match system {
        "darwin" => ("macosx", "macosx", "", "MACOSX_DEPLOYMENT_TARGET"),
        "ios-sim" => (
            "ios",
            "iphonesimulator",
            "-simulator",
            "IPHONEOS_DEPLOYMENT_TARGET",
        ),
        "ios" if arch == "x86_64" => (
            "ios",
            "iphonesimulator",
            "-simulator",
            "IPHONEOS_DEPLOYMENT_TARGET",
        ),
        "ios" => ("ios", "iphoneos", "", "IPHONEOS_DEPLOYMENT_TARGET"),
        _ => return None,
    };
    let output = |command: &mut Command| {
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("{command:?}: {error}"));
        assert!(output.status.success(), "{command:?} failed");
        String::from_utf8(output.stdout)
            .expect("UTF-8 output")
            .trim()
            .to_owned()
    };
    println!("cargo:rerun-if-env-changed={variable}");
    let printed = output(
        Command::new(env::var_os("RUSTC").expect("RUSTC"))
            .args(["--print", "deployment-target", "--target"])
            .arg(target),
    );
    let version = printed
        .split_once('=')
        .map_or(printed.as_str(), |(_, version)| version);
    let sysroot = output(Command::new("xcrun").args(["--sdk", sdk, "--show-sdk-path"]));
    Some((
        format!("{arch}-apple-{os}{version}{suffix}"),
        PathBuf::from(sysroot),
    ))
}

fn is_cxx(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "cpp" || extension == "cc" || extension == "cxx")
}

/// A 128-bit key over everything the archive is built from.
struct Key([DefaultHasher; 2]);

impl Key {
    fn new() -> Self {
        let mut hashers = [DefaultHasher::new(), DefaultHasher::new()];
        hashers[1].write(b"nanocode-python-capi");
        Self(hashers)
    }

    fn add(&mut self, bytes: &[u8]) {
        for hasher in &mut self.0 {
            hasher.write_usize(bytes.len());
            hasher.write(bytes);
        }
    }

    /// Adds every file below dir by its path under root and its content, in sorted order.
    fn tree(&mut self, root: &Path, dir: &Path) {
        let mut paths: Vec<PathBuf> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        paths.sort();
        for path in paths {
            let name = path.strip_prefix(root).unwrap_or(&path).to_owned();
            self.add(name.as_os_str().as_encoded_bytes());
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_dir() => {
                    self.add(b"/");
                    self.tree(root, &path);
                }
                Ok(meta) if meta.is_symlink() => {
                    let to = fs::read_link(&path).unwrap_or_default();
                    self.add(to.as_os_str().as_encoded_bytes());
                }
                _ => self.add(&fs::read(&path).unwrap_or_default()),
            }
        }
    }

    fn finish(&self) -> String {
        format!("{:016x}{:016x}", self.0[0].finish(), self.0[1].finish())
    }
}

const CACHED: [&str; 2] = ["libnanocode_python_capi_ext.a", "extensions.rs"];

/// Where a build with these inputs keeps its archive and extension table: a directory under
/// $NANOCODE_CAPI_CACHE (default ~/.cache/nanocode/capi) named by a hash of the sources, this
/// script, the compiler's path and version, the target and the variables the compiler reads.
/// Also returns the object cache directory beside it and a key over the toolchain alone, which
/// the per-source object cache starts from.
/// None when caching is off (NANOCODE_CAPI_CACHE=0) or the compiler can't say its version.
fn cache_entry(
    root: &Path,
    compiler: &OsStr,
    target: &str,
    host: &str,
    families: &[&str],
    apple: Option<&(String, PathBuf)>,
) -> Option<(PathBuf, PathBuf, String)> {
    let base = match env::var_os("NANOCODE_CAPI_CACHE") {
        Some(dir) if dir.is_empty() || dir == "0" => return None,
        Some(dir) => PathBuf::from(dir),
        None => env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?
            .join("nanocode")
            .join("capi"),
    };
    let version = Command::new(compiler)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())?
        .stdout;
    let mut key = Key::new();
    key.add(target.as_bytes());
    key.add(host.as_bytes());
    key.add(families.join(",").as_bytes());
    key.add(compiler.as_encoded_bytes());
    key.add(&version);
    if let Some((triple, sysroot)) = apple {
        key.add(triple.as_bytes());
        key.add(sysroot.as_os_str().as_encoded_bytes());
    }
    for name in [
        "CPATH",
        "C_INCLUDE_PATH",
        "CPLUS_INCLUDE_PATH",
        "SDKROOT",
        "CCC_OVERRIDE_OPTIONS",
    ] {
        key.add(env::var_os(name).unwrap_or_default().as_encoded_bytes());
    }
    let toolchain = key.finish();
    key.add(&fs::read(root.join("build.rs")).ok()?);
    for dir in ["c", "lib", "ext", "cxx", "include"] {
        key.add(dir.as_bytes());
        key.tree(root, &root.join(dir));
    }
    Some((base.join(key.finish()), base.join("objects"), toolchain))
}

/// Writes bytes to path through a temporary file beside it renamed into place, so readers see
/// a whole file or none.
fn write_atomic(path: &Path, bytes: &[u8]) {
    let tmp = path.with_extension(format!(
        "tmp{}-{:?}",
        std::process::id(),
        thread::current().id()
    ));
    if fs::write(&tmp, bytes).is_err() || fs::rename(&tmp, path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

/// The files a -MD depfile lists after its target, other than the source itself.
fn depfile_inputs(text: &str, source: &Path) -> Vec<String> {
    let body = text.replace("\\\n", " ");
    let body = body.split_once(": ").map_or("", |(_, rest)| rest);
    let source = source.display().to_string();
    let mut inputs: Vec<String> = Vec::new();
    for word in body.split_whitespace() {
        if word != source && !inputs.iter().any(|seen| seen == word) {
            inputs.push(word.to_owned());
        }
    }
    inputs
}

/// Copies a cached archive and table into out; Some(whether it needs the C++ runtime) on a hit.
fn restore(entry: &Path, out: &Path) -> Option<bool> {
    let cxx = fs::read(entry.join("cxx")).ok()?;
    for name in CACHED {
        fs::copy(entry.join(name), out.join(name)).ok()?;
    }
    Some(cxx == b"1")
}

/// Saves the archive and table built in out under entry: written beside it, then renamed into
/// place, so a reader sees a whole entry or none.
fn save(entry: &Path, out: &Path, cxx_runtime: bool) {
    let (Some(parent), Some(name)) = (entry.parent(), entry.file_name()) else {
        return;
    };
    let staging = parent.join(format!(
        ".{}-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    let written = fs::create_dir_all(&staging).is_ok()
        && CACHED
            .iter()
            .all(|name| fs::copy(out.join(name), staging.join(name)).is_ok())
        && fs::write(staging.join("cxx"), if cxx_runtime { "1" } else { "0" }).is_ok();
    if !written || fs::rename(&staging, entry).is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
}

/// Tells cargo to link the archive in out, and the C++ and Windows libraries it needs.
fn link(out: &Path, cxx_runtime: bool, wasm: bool, windows: bool) {
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=nanocode_python_capi_ext");
    if cxx_runtime && !wasm {
        if env::var("CARGO_CFG_TARGET_VENDOR").is_ok_and(|vendor| vendor == "apple") {
            println!("cargo:rustc-link-lib=c++");
        } else if !windows {
            println!("cargo:rustc-link-lib=stdc++");
        }
    }
    if env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|target_env| target_env == "msvc") {
        println!("cargo:rustc-link-lib=oldnames");
        println!("cargo:rustc-link-lib=gdi32");
        println!("cargo:rustc-link-lib=user32");
    }
}

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let target = env::var("TARGET").expect("TARGET");
    let host = env::var("HOST").expect("HOST");
    let families = env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    let families: Vec<&str> = families.split(',').collect();
    let wasm = families.contains(&"wasm");
    let windows = env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    let variable = format!("CC_{}", target.replace('-', "_"));
    println!("cargo:rerun-if-env-changed=NANOCODE_CC");
    println!("cargo:rerun-if-env-changed={variable}");
    println!("cargo:rerun-if-changed=include");
    println!("cargo:rerun-if-changed=ext");
    println!("cargo:rerun-if-changed=lib");
    let compiler = env::var_os("NANOCODE_CC")
        .or_else(|| env::var_os(&variable))
        .unwrap_or_else(|| {
            if wasm || windows || target != host {
                "clang".into()
            } else {
                "cc".into()
            }
        });
    let include = root.join("include");
    let mut objects = Vec::new();
    let mut table = String::new();
    let mut entries = String::new();
    let mut arities = String::new();
    let mut tables = String::new();
    println!("cargo:rerun-if-changed=c");
    let jobs: usize = env::var("NUM_JOBS")
        .ok()
        .and_then(|jobs| jobs.parse().ok())
        .unwrap_or(1)
        .max(1);
    let runtime = root.join("cxx");
    println!("cargo:rerun-if-changed=cxx");
    let apple = apple(&target);
    // About 500 C and C++ files compile below. What they make depends only on the inputs
    // cache_entry hashes, so a build whose inputs another build already compiled copies its
    // archive and table instead.
    println!("cargo:rerun-if-env-changed=NANOCODE_CAPI_CACHE");
    let cache = cache_entry(&root, &compiler, &target, &host, &families, apple.as_ref());
    let entry = cache.as_ref().map(|(entry, _, _)| entry.clone());
    if let Some(cxx_runtime) = entry.as_deref().and_then(|entry| restore(entry, &out)) {
        link(&out, cxx_runtime, wasm, windows);
        return;
    }
    // On an archive miss each source is still looked up on its own, as ccache's direct mode
    // does: objects/<key1>/deps lists the headers that source's last compile read (from its -MD
    // depfile), and objects/<key1>/<key2>.o is the object, where key1 covers the toolchain, the
    // arguments (with the crate and OUT_DIR written as @root and @out) and the source, and key2
    // the listed headers' contents. Misses compile through $NANOCODE_CC_LAUNCHER (such as
    // sccache) when it is set.
    println!("cargo:rerun-if-env-changed=NANOCODE_CC_LAUNCHER");
    let launcher = env::var_os("NANOCODE_CC_LAUNCHER").filter(|launcher| !launcher.is_empty());
    let object_cache = cache.as_ref().map(|(_, objects, toolchain)| (objects, toolchain));
    let (hits, misses) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let (root_text, out_text) = (root.display().to_string(), out.display().to_string());
    let portable = |text: &str| text.replace(&out_text, "@out").replace(&root_text, "@root");
    let local = |text: &str| text.replace("@out", &out_text).replace("@root", &root_text);
    let headers_key = |deps: &str| {
        let mut key = Key::new();
        for line in deps.lines() {
            key.add(line.as_bytes());
            key.add(&fs::read(local(line)).unwrap_or_else(|_| b"\0missing".to_vec()));
        }
        key.finish()
    };
    let compile = |source: &Path, object: &Path, defines: &[String]| {
        let mut command = Command::new(&compiler);
        if let Some((triple, sysroot)) = &apple {
            command
                .arg(format!("--target={triple}"))
                .arg("-isysroot")
                .arg(sysroot);
        } else if target != host {
            command.arg(format!(
                "--target={}",
                target.replace("riscv64gc-", "riscv64-")
            ));
        }
        let cxx = is_cxx(source);
        if wasm {
            command.args(["-ffreestanding", "-nostdlibinc"]);
            if cxx {
                command
                    .args(["-std=c++17", "-fwasm-exceptions", "-nostdinc++", "-isystem"])
                    .arg(include.join("c++").join("v1"))
                    .arg("-isystem")
                    .arg(include.join("libc"));
            } else {
                command
                    .args(["-fwasm-exceptions", "-mllvm", "-wasm-enable-sjlj"])
                    .arg("-I")
                    .arg(include.join("libc"));
            }
        } else {
            if cxx {
                command.arg("-std=c++17");
            }
            if !windows {
                command.arg("-fPIC");
            }
            if target != host && target.contains("-linux-gnu") {
                command.arg("-D_GNU_SOURCE");
            }
        }
        // Objects name their sources through __FILE__ and debug info; mapping the crate and
        // OUT_DIR to fixed names makes them the same from any checkout, so a cached archive
        // never carries another worktree's paths.
        command
            .arg(format!("-ffile-prefix-map={}=/nanocode/python-capi", root.display()))
            .arg(format!("-ffile-prefix-map={}=/nanocode/out", out.display()))
            .args([
                "-O2",
                "-DNDEBUG",
                "-DCYTHON_LIMITED_API=1",
                "-fvisibility=hidden",
            ])
            .args(defines)
            .arg("-I")
            .arg(&include)
            .arg("-I")
            .arg(include.join("cpython"))
            .arg("-c")
            .arg(source)
            .arg("-o")
            .arg(object);
        let depfile = object.with_extension("d");
        let slot = object_cache.map(|(objects, toolchain)| {
            let mut key = Key::new();
            key.add(toolchain.as_bytes());
            for arg in command.get_args() {
                let arg = arg.to_string_lossy();
                if arg == object.to_string_lossy() {
                    key.add(b"@object");
                } else {
                    key.add(portable(&arg).as_bytes());
                }
            }
            key.add(&fs::read(source).unwrap_or_default());
            objects.join(key.finish())
        });
        if let Some(slot) = &slot
            && let Ok(deps) = fs::read_to_string(slot.join("deps"))
            && fs::copy(slot.join(format!("{}.o", headers_key(&deps))), object).is_ok()
        {
            hits.fetch_add(1, Ordering::Relaxed);
            return;
        }
        command.arg("-MD").arg("-MF").arg(&depfile);
        let mut command = match &launcher {
            Some(launcher) => {
                let args: Vec<OsString> = command.get_args().map(OsStr::to_owned).collect();
                let mut wrapped = Command::new(launcher);
                wrapped.arg(&compiler).args(args);
                wrapped
            }
            None => command,
        };
        let status = command
            .status()
            .unwrap_or_else(|error| panic!("{}: {error}", compiler.to_string_lossy()));
        assert!(status.success(), "compiling {} failed", source.display());
        misses.fetch_add(1, Ordering::Relaxed);
        if let Some(slot) = &slot
            && let Ok(text) = fs::read_to_string(&depfile)
            && fs::create_dir_all(slot).is_ok()
        {
            let deps: String = depfile_inputs(&text, source)
                .iter()
                .map(|path| portable(path) + "\n")
                .collect();
            if let Ok(bytes) = fs::read(object) {
                write_atomic(&slot.join(format!("{}.o", headers_key(&deps))), &bytes);
                write_atomic(&slot.join("deps"), deps.as_bytes());
            }
        }
    };
    let mut cxx_runtime = false;
    // Every C and C++ source below goes into one work list, compiled by NUM_JOBS workers that
    // each take the next source as they finish, so one directory's sources never wait for
    // another's. Objects keep the numbering and archive order of a directory-by-directory build.
    let sources: Vec<PathBuf> = sorted(&root.join("c"), |path| {
        path.extension().is_some_and(|extension| extension == "c")
    });
    let first = objects.len();
    let units: Vec<(usize, &Path, &[String])> = sources
        .iter()
        .enumerate()
        .map(|(offset, source)| (first + offset, source.as_path(), &[][..]))
        .collect();
    objects.extend(pool(jobs, &units, |&(index, source, flags)| {
        let object = out.join(format!("{index}.o"));
        compile(source, &object, flags);
        object
    }));
    let mut libs: Vec<(Vec<String>, Vec<PathBuf>)> = Vec::new();
    for dir in sorted(&root.join("lib"), |path| path.is_dir()) {
        let only: Vec<String> = listed(&dir, "families").collect();
        if !only.is_empty()
            && !only
                .iter()
                .any(|family| families.contains(&family.as_str()))
        {
            continue;
        }
        let mut flags = vec![format!("-I{}", dir.display())];
        if wasm {
            flags.extend(["-mllvm".to_owned(), "-wasm-enable-sjlj".to_owned()]);
        }
        flags
            .extend(listed(&dir, "includes").map(|line| format!("-I{}", dir.join(line).display())));
        flags.extend(listed(&dir, "defines").map(|line| format!("-D{line}")));
        flags.extend(listed(&dir, "flags"));
        let sources: Vec<PathBuf> = if dir.join("sources").exists() {
            listed(&dir, "sources").map(|line| dir.join(line)).collect()
        } else {
            sorted(&dir, |path| {
                path.extension().is_some_and(|extension| extension == "c")
            })
        };
        libs.push((flags, sources));
    }
    let mut units: Vec<(usize, &Path, &[String])> = Vec::new();
    for (flags, sources) in &libs {
        for source in sources {
            units.push((objects.len() + units.len(), source, flags));
        }
    }
    objects.extend(pool(jobs, &units, |&(index, source, flags)| {
        let object = out.join(format!("{index}.o"));
        compile(source, &object, flags);
        object
    }));
    let mut modules: Vec<(String, String, Vec<String>, Vec<PathBuf>)> = Vec::new();
    for dir in sorted(&root.join("ext"), |path| path.is_dir()) {
        let name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .expect("a module directory named by its dotted name")
            .to_owned();
        let symbol = format!("PyInit_{}", name.replace('.', "_"));
        let last = name.rsplit('.').next().unwrap_or(&name);
        let mut defines = vec![
            format!("-DPyInit_{last}={symbol}"),
            format!("-I{}", dir.display()),
        ];
        defines.extend(
            sorted(&dir, |path| path.is_dir())
                .iter()
                .map(|include| format!("-I{}", include.display())),
        );
        if let Ok(includes) = fs::read_to_string(dir.join("includes")) {
            defines.extend(
                includes
                    .lines()
                    .filter(|line| !line.is_empty())
                    .map(|line| format!("-I{}", dir.join(line).display())),
            );
        }
        if let Ok(macros) = fs::read_to_string(dir.join("defines")) {
            defines.extend(
                macros
                    .lines()
                    .filter(|line| !line.is_empty())
                    .map(|line| format!("-D{line}")),
            );
        }
        if let Ok(flags) = fs::read_to_string(dir.join("flags")) {
            defines.extend(
                flags
                    .lines()
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned),
            );
        }
        let sources: Vec<PathBuf> = match fs::read_to_string(dir.join("sources")) {
            Ok(list) => list
                .lines()
                .filter(|line| !line.is_empty())
                .map(|line| dir.join(line))
                .collect(),
            Err(_) => sorted(&dir, |path| {
                path.extension().is_some_and(|extension| extension == "c") || is_cxx(path)
            }),
        };
        cxx_runtime |= sources.iter().any(|source| is_cxx(source));
        modules.push((name, symbol, defines, sources));
    }
    let mut units: Vec<(usize, &Path, &[String])> = Vec::new();
    for (_, _, defines, sources) in &modules {
        for source in sources {
            units.push((objects.len() + units.len(), source, defines));
        }
    }
    let built = pool(jobs, &units, |&(index, source, defines)| {
        let object = out.join(format!("{index}.o"));
        compile(source, &object, defines);
        let mut table = false;
        if wasm {
            let cxx = is_cxx(source);
            let functions = address_taken(&object);
            let mut wrapper = format!("#include {:?}\n", source.display().to_string());
            if cxx {
                wrapper.push_str(&format!(
                    "__asm__(\".section .rodata.nanocode_arity_{index},\\\"\\\",@\\n.globl nanocode_arity_{index}\\n.p2align 2\\nnanocode_arity_{index}:\\n\"\n"
                ));
                for (function, params) in &functions {
                    wrapper.push_str(&format!(
                        "\".int32 \\\"{function}\\\"\\n.int32 {params}\\n\"\n"
                    ));
                }
                wrapper.push_str(&format!(
                    "\".int32 0\\n.int32 0\\n.size nanocode_arity_{index}, {}\\n.text\\n\");\n",
                    8 * (functions.len() + 1)
                ));
            } else {
                wrapper.push_str(&format!(
                    "__attribute__((used)) const void *const nanocode_arity_{index}[] = {{\n"
                ));
                for (function, params) in &functions {
                    wrapper.push_str(&format!(
                        "    (const void *){function}, (const void *){params},\n"
                    ));
                }
                wrapper.push_str("    0, 0\n};\n");
            }
            let extension = source
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("c");
            if !functions.is_empty() {
                let path = out.join(format!("{index}.{extension}"));
                fs::write(&path, wrapper).expect("the arity wrapper");
                compile(&path, &object, defines);
                table = true;
            }
        }
        (index, object, table)
    });
    let mut built = built.into_iter();
    for (name, symbol, _, sources) in &modules {
        for (index, object, has_table) in built.by_ref().take(sources.len()) {
            if has_table {
                arities.push_str(&format!("    static nanocode_arity_{index}: [usize; 0];\n"));
                tables.push_str(&format!(
                    "        &raw const nanocode_arity_{index} as *const usize,\n"
                ));
            }
            objects.push(object);
        }
        table.push_str(&format!("    fn {symbol}() -> *mut Object;\n"));
        entries.push_str(&format!(
            "    Extension {{ name: {name:?}, init: {symbol} }},\n"
        ));
    }
    if cxx_runtime && wasm {
        let list =
            fs::read_to_string(runtime.join("sources")).expect("the C++ runtime source list");
        let sources: Vec<PathBuf> = list
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| runtime.join(line))
            .chain([runtime.join("runtime.c")])
            .collect();
        let flags = [
            "-D_LIBUNWIND_HIDE_SYMBOLS".to_owned(),
            "-D_LIBCPP_BUILDING_LIBRARY".to_owned(),
            "-D_LIBCPP_HAS_NO_PRAGMA_SYSTEM_HEADER".to_owned(),
            "-DLIBCXX_BUILDING_LIBCXXABI".to_owned(),
            "-D_LIBCXXABI_BUILDING_LIBRARY".to_owned(),
            "-D_LIBCXXABI_HAS_NO_THREADS".to_owned(),
            "-std=c++23".to_owned(),
            format!("-I{}", runtime.join("libcxx").join("src").display()),
            format!("-I{}", runtime.join("libcxxabi").join("include").display()),
            format!("-I{}", runtime.join("libunwind").join("include").display()),
        ];
        let first = objects.len();
        let mut built: Vec<(usize, PathBuf)> = thread::scope(|scope| {
            let workers: Vec<_> = (0..jobs.min(sources.len()))
                .map(|worker| {
                    let (sources, flags, compile, out) = (&sources, &flags, &compile, &out);
                    scope.spawn(move || {
                        let mut done = Vec::new();
                        for (offset, source) in
                            sources.iter().enumerate().skip(worker).step_by(jobs)
                        {
                            let index = first + offset;
                            let object = out.join(format!("{index}.o"));
                            let unwind = [flags[0].clone(), flags[flags.len() - 1].clone()];
                            let flags = if is_cxx(source) {
                                &flags[..]
                            } else {
                                &unwind[..]
                            };
                            compile(source, &object, flags);
                            done.push((index, object));
                        }
                        done
                    })
                })
                .collect();
            workers
                .into_iter()
                .flat_map(|worker| worker.join().expect("a compile worker"))
                .collect()
        });
        built.sort_by_key(|(index, _)| *index);
        objects.extend(built.into_iter().map(|(_, object)| object));
    }
    if object_cache.is_some() {
        println!(
            "cargo:warning=python-capi object cache: {} hits, {} misses",
            hits.load(Ordering::Relaxed),
            misses.load(Ordering::Relaxed)
        );
    }
    fs::write(out.join("libnanocode_python_capi_ext.a"), archive(&objects)).expect("the archive");
    fs::write(
        out.join("extensions.rs"),
        format!("unsafe extern \"C\" {{\n{table}{arities}}}\n\npub static EXTENSIONS: &[Extension] = &[\n{entries}];\n\npub fn arity_tables() -> Vec<*const usize> {{\n    vec![\n{tables}    ]\n}}\n"),
    )
    .expect("the extension table");
    if let Some(entry) = &entry {
        save(entry, &out, cxx_runtime);
    }
    link(&out, cxx_runtime, wasm, windows);
}
