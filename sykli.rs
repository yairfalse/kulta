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

    // Integration tests with seppo (requires kind cluster)
    // Run manually: KULTA_RUN_SEPPO_TESTS=1 cargo test --test seppo_integration_test -- --ignored
    p.task("integration-test")
        .run("cargo test --test seppo_integration_test -- --ignored")
        .env("KULTA_RUN_SEPPO_TESTS", "1")
        .inputs(&["tests/seppo_integration_test.rs", "**/*.rs", "Cargo.toml"]);

    p.emit();
}
