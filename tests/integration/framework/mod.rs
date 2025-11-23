//! Integration test framework for KULTA
//!
//! Provides infrastructure for testing progressive deployments:
//! - Kind cluster management
//! - Kubernetes resource helpers
//! - Metrics collection and analysis
//! - Network traffic capture

#![allow(dead_code)] // Test framework - fields/functions used across different scenarios

pub mod assertions;
pub mod cluster;
pub mod k8s;
pub mod metrics;

use serde::Deserialize;
use std::error::Error;

pub type TestResult = Result<(), Box<dyn Error>>;

/// Test configuration loaded from config.toml
#[derive(Debug, Clone, Deserialize)]
pub struct TestConfig {
    pub cluster: ClusterConfig,
    pub scenarios: ScenarioConfig,
    pub timeouts: TimeoutConfig,
    pub performance: PerformanceConfig,
    pub sniffer: SnifferConfig,
    pub deployment: DeploymentConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClusterConfig {
    pub name: String,
    pub reuse: bool,
    pub cleanup: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScenarioConfig {
    pub canary_rollout: bool,
    pub blue_green_swap: bool,
    pub traffic_splitting: bool,
    pub rollback_on_error: bool,
    pub progressive_headers: bool,
    pub load_testing: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimeoutConfig {
    pub gateway_ready: u64,
    pub route_ready: u64,
    pub deployment_ready: u64,
    pub reconciliation: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PerformanceConfig {
    pub tool: String,
    pub duration_seconds: u64,
    pub connections: u32,
    pub threads: u32,
    pub target_rps: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SnifferConfig {
    pub enabled: bool,
    pub interface: String,
    pub filter: String,
    pub output_dir: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeploymentConfig {
    pub stable_image: String,
    pub canary_image: String,
    pub replicas: i32,
    pub canary_steps: Vec<i32>,
    pub step_duration_secs: u64,
}

impl TestConfig {
    /// Load configuration from tests/integration/config.toml
    pub fn load() -> Result<Self, Box<dyn Error>> {
        let config_path = "tests/integration/config.toml";
        let contents = std::fs::read_to_string(config_path)?;
        let config: TestConfig = toml::from_str(&contents)?;
        Ok(config)
    }
}

/// Test context holds shared state across scenarios
pub struct TestContext {
    pub client: kube::Client,
    pub namespace: String,
    pub metrics: metrics::MetricsCollector,
    pub config: TestConfig,

    // KULTA-specific state
    pub canary_weight: f64,
    pub deployment_id: String,
}

impl TestContext {
    /// Create new test context
    pub async fn new(config: &TestConfig) -> Result<Self, Box<dyn Error>> {
        // Ensure cluster exists
        cluster::ensure_cluster(&config.cluster).await?;

        // Create K8s client
        let client = kube::Client::try_default().await?;

        // Create test namespace
        let namespace = format!("kulta-test-{}", chrono::Utc::now().timestamp());
        k8s::create_namespace(&client, &namespace).await?;

        // Initialize metrics collector
        let metrics = metrics::MetricsCollector::new();

        Ok(Self {
            client,
            namespace,
            metrics,
            config: config.clone(),
            canary_weight: 0.0,
            deployment_id: String::new(),
        })
    }

    /// Cleanup test resources
    pub async fn cleanup(&self, config: &TestConfig) -> Result<(), Box<dyn Error>> {
        // Delete test namespace
        k8s::delete_namespace(&self.client, &self.namespace).await?;

        // Optionally cleanup cluster
        if config.cluster.cleanup {
            cluster::delete_cluster(&config.cluster).await?;
        }

        Ok(())
    }
}

/// Trait for test scenarios
#[async_trait::async_trait]
pub trait TestScenario: Send + Sync {
    /// Name of the scenario
    fn name(&self) -> &str;

    /// Run the scenario
    async fn run(&self, ctx: &mut TestContext) -> TestResult;

    /// Check if scenario should be skipped
    fn should_skip(&self, config: &TestConfig) -> bool;
}
