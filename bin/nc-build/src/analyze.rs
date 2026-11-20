use std::{
    path::{Path, PathBuf},
    process::Command,
};

use nanocode_build_opt::{
    analyze::{self, Basis},
    graph::Graph,
};

pub fn analyze(args: &[String]) -> Result<(), String> {
    let since = option(args, "--since").unwrap_or("30.days");
    let top: usize = option(args, "--top")
        .map_or(Ok(10), str::parse)
        .map_err(|error| format!("--top: {error}"))?;
    let mut metadata_args = vec!["metadata", "--format-version", "1", "--locked"];
    if let Some(manifest) = option(args, "--manifest-path") {
        metadata_args.extend(["--manifest-path", manifest]);
    }
    let metadata = run("cargo", &metadata_args)?;
    let crates = analyze::parse_crates(&metadata).map_err(|error| format!("{error:?}"))?;
    let graph = Graph::from_metadata(&metadata).map_err(|error| format!("{error:?}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&metadata).map_err(|error| error.to_string())?;
    let target = value
        .get("target_directory")
        .and_then(serde_json::Value::as_str)
        .ok_or("metadata: no target_directory")?;
    let timings = option(args, "--timings")
        .map_or_else(|| Path::new(target).join("cargo-timings"), PathBuf::from);
    let timings = if timings.is_dir() {
        timings.join("cargo-timing.html")
    } else {
        timings
    };
    let html = std::fs::read_to_string(&timings).map_err(|error| {
        format!(
            "{}: {error} (run cargo build --timings first)",
            timings.display()
        )
    })?;
    let units = analyze::parse_timings(&html).map_err(|error| format!("{error:?}"))?;
    let root = graph.root.trim_end_matches('/');
    let since = format!("--since={since}");
    let log = run(
        "git",
        &[
            "-C",
            root,
            "log",
            &since,
            "--relative",
            "--format=%x01",
            "--name-only",
        ],
    )?;
    let (commits, edits) = analyze::edits(&graph, &log);
    let report = analyze::analyze(&units, &crates, &graph, commits, &edits, top);
    let mut out = format!(
        "{} units: wall {:.1}s, cpu {:.1}s, ideal critical path {:.1}s\ncritical path: {}\n{commits} commits {since}; {} build scripts, {} proc-macros, {} duplicated packages, {} recompiled libraries, {} test binaries\nrank  wall_s   cpu_s  per     change\n",
        report.units,
        report.wall_s,
        report.cpu_s,
        report.critical_s,
        report.critical_path.join(" > "),
        report.build_scripts,
        report.proc_macros,
        report.duplicated,
        report.recompiled,
        report.test_binaries,
    );
    for (rank, change) in report.changes.iter().take(top).enumerate() {
        let per = match change.basis {
            Basis::ColdBuild => "cold",
            Basis::Commit => "commit",
        };
        out.push_str(&format!(
            "{:>4} {:>7.1} {:>7.1}  {per:<7} {}\n",
            rank + 1,
            change.saved_wall_s,
            change.saved_cpu_s,
            change.summary
        ));
    }
    print!("{out}");
    Ok(())
}

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("{program}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| format!("{program}: {error}"))
}
