//! KULTA Integration Tests
//!
//! Run with: cargo test --test integration_test

#![allow(clippy::expect_used)] // Integration tests can use expect for clarity

mod integration;

use integration::scenarios::CanaryRolloutScenario;
use integration::{TestConfig, TestContext, TestScenario};

#[tokio::test]
async fn run_integration_tests() {
    // Skip integration tests unless explicitly enabled
    // These tests require 'kind' to be installed and will create a real K8s cluster
    if std::env::var("KULTA_RUN_INTEGRATION_TESTS").is_err() {
        println!(
            "⏭️  Skipping integration tests (set KULTA_RUN_INTEGRATION_TESTS=1 to enable)"
        );
        println!("   These tests require 'kind' to be installed.");
        return;
    }

    // Load configuration
    let config = TestConfig::load().expect("Failed to load test config");

    // Create test context
    let mut ctx = TestContext::new(&config)
        .await
        .expect("Failed to create test context");

    // Register scenarios
    let scenarios: Vec<Box<dyn TestScenario>> = vec![
        Box::new(CanaryRolloutScenario),
        // Add more scenarios here as they're implemented
    ];

    // Run enabled scenarios
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;

    println!("\n🧪 KULTA Integration Tests");
    println!("==========================\n");

    for scenario in scenarios {
        if scenario.should_skip(&config) {
            println!("⏭️  Skipping: {}", scenario.name());
            skipped += 1;
            continue;
        }

        println!("🏃 Running: {}", scenario.name());

        match scenario.run(&mut ctx).await {
            Ok(()) => {
                println!("✅ Passed: {}\n", scenario.name());
                passed += 1;
            }
            Err(e) => {
                eprintln!("❌ Failed: {}", scenario.name());
                eprintln!("   Error: {}\n", e);
                failed += 1;
            }
        }
    }

    // Cleanup
    ctx.cleanup(&config).await.expect("Cleanup failed");

    // Print summary
    println!("\n📊 Summary");
    println!("==========");
    println!("  ✅ Passed:  {}", passed);
    println!("  ❌ Failed:  {}", failed);
    println!("  ⏭️  Skipped: {}", skipped);
    println!();

    if failed > 0 {
        panic!("{} test(s) failed", failed);
    }
}
