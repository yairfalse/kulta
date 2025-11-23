//! Kind cluster management

use super::ClusterConfig;
use std::error::Error;
use std::process::Command;

/// Ensure kind cluster exists
pub async fn ensure_cluster(config: &ClusterConfig) -> Result<(), Box<dyn Error>> {
    if config.reuse && cluster_exists(&config.name)? {
        println!("♻️  Reusing existing cluster: {}", config.name);
        return Ok(());
    }

    println!("🏗️  Creating kind cluster: {}", config.name);
    create_cluster(&config.name)?;
    println!("✅ Cluster ready: {}", config.name);

    Ok(())
}

/// Check if kind cluster exists
fn cluster_exists(name: &str) -> Result<bool, Box<dyn Error>> {
    let output = Command::new("kind").args(["get", "clusters"]).output()?;

    let clusters = String::from_utf8(output.stdout)?;
    Ok(clusters.lines().any(|line| line.trim() == name))
}

/// Create new kind cluster
fn create_cluster(name: &str) -> Result<(), Box<dyn Error>> {
    let status = Command::new("kind")
        .args(["create", "cluster", "--name", name])
        .status()?;

    if !status.success() {
        return Err("Failed to create kind cluster".into());
    }

    Ok(())
}

/// Delete kind cluster
pub async fn delete_cluster(config: &ClusterConfig) -> Result<(), Box<dyn Error>> {
    println!("🗑️  Deleting cluster: {}", config.name);

    let status = Command::new("kind")
        .args(["delete", "cluster", "--name", &config.name])
        .status()?;

    if !status.success() {
        return Err("Failed to delete kind cluster".into());
    }

    Ok(())
}
