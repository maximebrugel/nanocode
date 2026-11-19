const MAIN_THREAD_STACK_BYTES: u64 = 8 * 1024 * 1024;

fn main() {
    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc");
    let linux = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "linux");
    let gnu = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "gnu");
    if windows && msvc {
        println!("cargo:rustc-link-arg-bins=/STACK:{MAIN_THREAD_STACK_BYTES}");
    } else if windows {
        println!("cargo:rustc-link-arg-bins=-Wl,--stack,{MAIN_THREAD_STACK_BYTES}");
    } else if linux && gnu {
        println!("cargo:rustc-link-arg-bins=-no-pie");
        let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_owned());
        let found = std::process::Command::new(&cc)
            .arg("-print-file-name=libgcc_eh.a")
            .output();
        match found
            .ok()
            .map(|output| std::path::PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
        {
            Some(archive) if archive.is_absolute() && archive.is_file() => {
                println!(
                    "cargo:rustc-link-search=native={}",
                    archive.parent().unwrap_or(&archive).display()
                );
                println!("cargo:rustc-link-lib=static:-bundle=gcc_eh");
            }
            _ => println!("cargo:warning={cc} has no libgcc_eh.a: the binary needs libgcc_s.so.1"),
        }
    }
}
