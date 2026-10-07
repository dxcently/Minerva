//! The licence boundary, as a test: no `aoide*` package is anywhere in `Cargo.lock`.
//!
//! The perch is MIT, and the Aoide crates are GPL-3.0. The perch reaches `aoide` only as a child process it runs by explicit path
//! (`src/aoide/`), never as a crate, and a dependency of a dependency is as much a linkage as a direct one, so the check is on the whole
//! lock, not on `Cargo.toml`. A build that pulled an `aoide-*` crate in would otherwise ship GPL code inside an MIT binary with nothing
//! failing.

/// The packages in a lock file whose name starts with `aoide`, in any case, with their versions.
fn aoide_packages(lock: &str) -> Vec<String> {
    let table: toml::Table = lock.parse().expect("a Cargo.lock is TOML");
    let packages = table.get("package").and_then(|p| p.as_array()).expect("a Cargo.lock has [[package]] entries");
    packages
        .iter()
        .filter_map(|package| {
            let name = package.get("name")?.as_str()?;
            let version = package.get("version").and_then(|v| v.as_str()).unwrap_or("?");
            name.to_ascii_lowercase().starts_with("aoide").then(|| format!("{name} {version}"))
        })
        .collect()
}

#[test]
fn no_aoide_package_is_in_the_lock() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.lock");
    let lock = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    // The lock must be one this check can see into: the perch itself is in it, and so are the two crates the terminal host adds.
    assert!(lock.contains("name = \"perch\""), "the lock names the perch");
    assert!(lock.contains("name = \"portable-pty\"") && lock.contains("name = \"alacritty_terminal\""), "the terminal host's crates are in the lock");
    assert_eq!(aoide_packages(&lock), Vec::<String>::new(), "a GPL aoide crate is in the dependency graph of an MIT binary");
}

#[test]
fn a_lock_with_an_aoide_package_is_caught() {
    let fixture = "version = 4\n\n[[package]]\nname = \"perch\"\nversion = \"0.1.0\"\ndependencies = [\"aoide-core\"]\n\n\
                   [[package]]\nname = \"aoide-core\"\nversion = \"0.0.26\"\n\n[[package]]\nname = \"Aoided\"\nversion = \"1.0.0\"\n\n\
                   [[package]]\nname = \"not-aoide\"\nversion = \"1.0.0\"\n";
    assert_eq!(aoide_packages(fixture), ["aoide-core 0.0.26", "Aoided 1.0.0"], "a prefix match, in any case, and not a substring match");
}
