use std::{env, fs, path::PathBuf};

#[expect(
    dead_code,
    reason = "the build script makes only the per-cell setup's image"
)]
#[path = "src/setup.rs"]
mod setup;

fn main() {
    for source in [
        "src/setup.rs",
        "src/bootstrap.js",
        "src/code-tools.mjs",
        "src/code-values.mjs",
        "src/web.js",
        "src/node",
    ] {
        println!("cargo:rerun-if-changed={source}");
    }
    let snapshot = setup::per_cell()
        .and_then(|builder| builder.build().map_err(|error| error.to_string()))
        .unwrap_or_else(|error| panic!("the Code Mode bootstrap snapshot did not build: {error}"));
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("code-mode.snap");
    fs::write(&out, snapshot.to_bytes())
        .unwrap_or_else(|error| panic!("{}: {error}", out.display()));
}
