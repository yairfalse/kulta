//! Assertion helpers for progressive deployment validation

use k8s_openapi::api::apps::v1::Deployment;
use kube::api::Api;
use std::error::Error;

/// Assert deployment has expected replica count
pub async fn assert_replicas(
    client: &kube::Client,
    namespace: &str,
    deployment_name: &str,
    expected_replicas: i32,
) -> Result<(), Box<dyn Error>> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let deployment = deployments.get(deployment_name).await?;

    let actual = deployment.spec.and_then(|s| s.replicas).unwrap_or(0);

    if actual != expected_replicas {
        return Err(format!(
            "deployment {}: expected {} replicas, got {}",
            deployment_name, expected_replicas, actual
        )
        .into());
    }

    println!("✅ Deployment {} has {} replicas", deployment_name, actual);
    Ok(())
}

/// Assert HTTPRoute has correct traffic weights
pub async fn assert_traffic_split(
    client: &kube::Client,
    namespace: &str,
    route_name: &str,
    stable_weight: i32,
    canary_weight: i32,
) -> Result<(), Box<dyn Error>> {
    use gateway_api::apis::standard::httproutes::HTTPRoute;

    let routes: Api<HTTPRoute> = Api::namespaced(client.clone(), namespace);
    let route = routes.get(route_name).await?;

    // Get backend refs from first rule
    let backend_refs = route
        .spec
        .rules
        .as_ref()
        .and_then(|rules| rules.first())
        .and_then(|r| r.backend_refs.as_ref())
        .ok_or("no backend refs found")?;

    if backend_refs.len() != 2 {
        return Err(format!(
            "expected 2 backend refs (stable + canary), got {}",
            backend_refs.len()
        )
        .into());
    }

    let actual_stable = backend_refs[0].weight.unwrap_or(0);
    let actual_canary = backend_refs[1].weight.unwrap_or(0);

    if actual_stable != stable_weight || actual_canary != canary_weight {
        return Err(format!(
            "traffic split mismatch: expected {}% stable / {}% canary, got {}% / {}%",
            stable_weight, canary_weight, actual_stable, actual_canary
        )
        .into());
    }

    println!(
        "✅ Traffic split: {}% stable / {}% canary",
        actual_stable, actual_canary
    );
    Ok(())
}

/// Assert deployment is ready (all replicas available)
pub async fn assert_deployment_ready(
    client: &kube::Client,
    namespace: &str,
    deployment_name: &str,
) -> Result<(), Box<dyn Error>> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let deployment = deployments.get(deployment_name).await?;

    let status = deployment.status.ok_or("deployment has no status")?;

    let ready = status.ready_replicas.unwrap_or(0);
    let desired = deployment.spec.and_then(|s| s.replicas).unwrap_or(0);

    if ready != desired {
        return Err(format!(
            "deployment {} not ready: {}/{} replicas",
            deployment_name, ready, desired
        )
        .into());
    }

    println!(
        "✅ Deployment {} ready: {}/{} replicas",
        deployment_name, ready, desired
    );
    Ok(())
}

/// Assert error rate is below threshold
pub fn assert_error_rate_below(error_rate: f64, threshold: f64) -> Result<(), Box<dyn Error>> {
    if error_rate > threshold {
        return Err(format!(
            "error rate too high: {:.2}% > {:.2}%",
            error_rate * 100.0,
            threshold * 100.0
        )
        .into());
    }

    println!(
        "✅ Error rate OK: {:.2}% < {:.2}%",
        error_rate * 100.0,
        threshold * 100.0
    );
    Ok(())
}
