//! Sykli CI definition for Kulta
//!
//! Run with: cargo run --manifest-path /path/to/sykli/sdk/rust/Cargo.toml -- --emit

use sykli::Pipeline;

fn main() {
    let mut p = Pipeline::new();

    // Test
    p.task("test")
        .run("cargo test")
        .inputs(&["**/*.rs", "Cargo.toml", "Cargo.lock"]);

    // Lint
    p.task("lint").run("cargo clippy -- -D warnings").inputs(&[
        "**/*.rs",
        "Cargo.toml",
        "Cargo.lock",
    ]);

    // Build (depends on test and lint)
    p.task("build")
        .run("cargo build --release")
        .inputs(&["**/*.rs", "Cargo.toml", "Cargo.lock"])
        .output("binary", "target/release/kulta")
        .after(&["test", "lint"]);

    p.emit();
}
