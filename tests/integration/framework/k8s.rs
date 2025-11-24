//! Kubernetes resource helpers

use k8s_openapi::api::core::v1::Namespace;
use kube::api::{Api, DeleteParams, ObjectMeta, PostParams};
use std::error::Error;

/// Create a namespace
pub async fn create_namespace(client: &kube::Client, name: &str) -> Result<(), Box<dyn Error>> {
    let ns: Api<Namespace> = Api::all(client.clone());

    let namespace = Namespace {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            ..Default::default()
        },
        ..Default::default()
    };

    ns.create(&PostParams::default(), &namespace).await?;
    println!("📦 Created namespace: {}", name);

    Ok(())
}

/// Delete a namespace
pub async fn delete_namespace(client: &kube::Client, name: &str) -> Result<(), Box<dyn Error>> {
    let ns: Api<Namespace> = Api::all(client.clone());

    match ns.delete(name, &DeleteParams::default()).await {
        Ok(_) => {
            println!("🗑️  Deleted namespace: {}", name);
            Ok(())
        }
        Err(kube::Error::Api(err)) if err.code == 404 => {
            // Already deleted, that's fine
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// Wait for deployment to be ready
pub async fn wait_for_deployment(
    client: &kube::Client,
    namespace: &str,
    name: &str,
    timeout_secs: u64,
) -> Result<(), Box<dyn Error>> {
    use k8s_openapi::api::apps::v1::Deployment;
    use std::time::Duration;
    use tokio::time::sleep;

    let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let start = std::time::Instant::now();

    loop {
        if start.elapsed().as_secs() > timeout_secs {
            return Err(format!("timeout waiting for deployment: {}", name).into());
        }

        match deployments.get(name).await {
            Ok(deployment) => {
                if let Some(status) = &deployment.status {
                    let ready = status.ready_replicas.unwrap_or(0);
                    let desired = deployment
                        .spec
                        .as_ref()
                        .and_then(|s| s.replicas)
                        .unwrap_or(0);

                    if ready == desired && desired > 0 {
                        println!("✅ Deployment ready: {} ({}/{})", name, ready, desired);
                        return Ok(());
                    }
                }
            }
            Err(kube::Error::Api(err)) if err.code == 404 => {
                // Not found yet, keep waiting
            }
            Err(e) => return Err(e.into()),
        }

        sleep(Duration::from_secs(2)).await;
    }
}

/// Wait for HTTPRoute to exist
pub async fn wait_for_httproute(
    client: &kube::Client,
    namespace: &str,
    name: &str,
    timeout_secs: u64,
) -> Result<(), Box<dyn Error>> {
    use gateway_api::apis::standard::httproutes::HTTPRoute;
    use std::time::Duration;
    use tokio::time::sleep;

    let routes: Api<HTTPRoute> = Api::namespaced(client.clone(), namespace);
    let start = std::time::Instant::now();

    loop {
        if start.elapsed().as_secs() > timeout_secs {
            return Err(format!("timeout waiting for HTTPRoute: {}", name).into());
        }

        match routes.get(name).await {
            Ok(_) => {
                println!("✅ HTTPRoute ready: {}", name);
                return Ok(());
            }
            Err(kube::Error::Api(err)) if err.code == 404 => {
                // Not found yet, keep waiting
            }
            Err(e) => return Err(e.into()),
        }

        sleep(Duration::from_secs(1)).await;
    }
}
