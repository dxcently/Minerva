//! `bench` — run a defender agent against a scored practice image and print a
//! report row per run (triage-toolkit.md §7–§8).
//!
//!   bench run   [--config PATH]   reset → boot → work → score → one JSON row per repeat
//!   bench check [--config PATH]   parse the config, print the recorded settings, run nothing
//!
//! With no agent runner wired yet, `run` drives the Noop agent: it measures the
//! frozen baseline over real qemu without changing the guest. Fold into
//! `minerva bench` once the hub exists.

use minerva_bench::agent::NoopRunner;
use minerva_bench::clock::SystemClock;
use minerva_bench::config::BenchConfig;
use minerva_bench::run::run_benchmark;
use minerva_bench::vm::{ShellHarness, Vm};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = match args.split_first() {
        Some((c, r)) => (c.as_str(), r),
        None => {
            usage();
            return ExitCode::FAILURE;
        }
    };
    let config_path = flag(rest, "--config");
    let cfg = match load_config(config_path.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("bench: config: {e}");
            return ExitCode::FAILURE;
        }
    };

    match cmd {
        "check" => {
            for (k, v) in cfg.recorded() {
                println!("{k} = {v}");
            }
            ExitCode::SUCCESS
        }
        "run" => run_repeats(&cfg),
        "-h" | "--help" | "help" => {
            usage();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("bench: unknown command `{other}`");
            usage();
            ExitCode::FAILURE
        }
    }
}

fn run_repeats(cfg: &BenchConfig) -> ExitCode {
    for i in 1..=cfg.repeats {
        let clock = SystemClock::new();
        let vm = Vm::new(ShellHarness, cfg);
        match run_benchmark(cfg, &vm, &vm, &NoopRunner, &clock) {
            Ok(row) => println!("{}", row.to_json()),
            Err(e) => {
                eprintln!("bench: run {i}/{}: {e}", cfg.repeats);
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

fn load_config(path: Option<&str>) -> Result<BenchConfig, String> {
    match path {
        Some(p) => {
            let text = std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?;
            BenchConfig::from_str(&text)
        }
        None => Err("--config PATH is required (a score_cmd must be set)".into()),
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn usage() {
    eprintln!(
        "usage:\n  \
         bench run   --config PATH   run the benchmark, one JSON row per repeat\n  \
         bench check --config PATH   print the recorded settings, run nothing"
    );
}
