//! Canary rollout scenario - progressive traffic shifting

use crate::integration::framework::{assertions, k8s, TestContext, TestResult, TestScenario};
use crate::integration::TestConfig;
use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    Container, PodSpec, PodTemplateSpec, Service, ServicePort, ServiceSpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::api::{Api, ObjectMeta, PostParams};
use std::collections::BTreeMap;

pub struct CanaryRolloutScenario;

#[async_trait::async_trait]
impl TestScenario for CanaryRolloutScenario {
    fn name(&self) -> &str {
        "canary_rollout"
    }

    async fn run(&self, ctx: &mut TestContext) -> TestResult {
        println!("\n🚀 Testing Progressive Canary Rollout");
        println!("=====================================\n");

        // Step 1: Deploy stable version
        println!("📦 Step 1: Deploying stable version...");
        deploy_stable(ctx).await?;
        k8s::wait_for_deployment(
            &ctx.client,
            &ctx.namespace,
            "app-stable",
            ctx.config.timeouts.deployment_ready,
        )
        .await?;

        assertions::assert_replicas(
            &ctx.client,
            &ctx.namespace,
            "app-stable",
            ctx.config.deployment.replicas,
        )
        .await?;

        // Step 2: Create services
        println!("\n📡 Step 2: Creating services...");
        create_services(ctx).await?;

        // Step 3: Create HTTPRoute for traffic splitting
        println!("\n🛣️  Step 3: Creating HTTPRoute...");
        create_httproute(ctx).await?;
        k8s::wait_for_httproute(
            &ctx.client,
            &ctx.namespace,
            "app-route",
            ctx.config.timeouts.route_ready,
        )
        .await?;

        // Step 4: Deploy canary version
        println!("\n🐤 Step 4: Deploying canary version...");
        deploy_canary(ctx).await?;
        k8s::wait_for_deployment(
            &ctx.client,
            &ctx.namespace,
            "app-canary",
            ctx.config.timeouts.deployment_ready,
        )
        .await?;

        // Step 5: Progressive traffic shifting
        println!("\n📊 Step 5: Progressive traffic shifting...");
        for weight in &ctx.config.deployment.canary_steps {
            println!("\n  → Shifting to {}% canary...", weight);

            // Update HTTPRoute weights
            update_traffic_split(ctx, *weight).await?;

            // Wait for traffic to stabilize
            tokio::time::sleep(tokio::time::Duration::from_secs(
                ctx.config.deployment.step_duration_secs,
            ))
            .await;

            // Verify weights applied
            let stable_weight = 100 - weight;
            assertions::assert_traffic_split(
                &ctx.client,
                &ctx.namespace,
                "app-route",
                stable_weight,
                *weight,
            )
            .await?;

            // TODO: Measure error rate (requires actual traffic)
            println!("    ✅ Traffic at {}% canary", weight);
        }

        // Step 6: Verify full promotion
        println!("\n🎯 Step 6: Verifying full canary promotion...");
        assertions::assert_traffic_split(&ctx.client, &ctx.namespace, "app-route", 0, 100).await?;
        assertions::assert_deployment_ready(&ctx.client, &ctx.namespace, "app-canary").await?;

        println!("\n✅ Canary rollout completed successfully!\n");
        Ok(())
    }

    fn should_skip(&self, config: &TestConfig) -> bool {
        !config.scenarios.canary_rollout
    }
}

/// Deploy stable version
async fn deploy_stable(ctx: &TestContext) -> TestResult {
    let mut labels = BTreeMap::new();
    labels.insert("app".to_string(), "myapp".to_string());
    labels.insert("version".to_string(), "stable".to_string());

    let deployment = Deployment {
        metadata: ObjectMeta {
            name: Some("app-stable".to_string()),
            namespace: Some(ctx.namespace.clone()),
            labels: Some(labels.clone()),
            ..Default::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(ctx.config.deployment.replicas),
            selector: LabelSelector {
                match_labels: Some(labels.clone()),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels.clone()),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    containers: vec![Container {
                        name: "app".to_string(),
                        image: Some(ctx.config.deployment.stable_image.clone()),
                        ports: Some(vec![k8s_openapi::api::core::v1::ContainerPort {
                            container_port: 80,
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        status: None,
    };

    let deployments: Api<Deployment> = Api::namespaced(ctx.client.clone(), &ctx.namespace);
    deployments
        .create(&PostParams::default(), &deployment)
        .await?;

    Ok(())
}

/// Deploy canary version
async fn deploy_canary(ctx: &TestContext) -> TestResult {
    let mut labels = BTreeMap::new();
    labels.insert("app".to_string(), "myapp".to_string());
    labels.insert("version".to_string(), "canary".to_string());

    let deployment = Deployment {
        metadata: ObjectMeta {
            name: Some("app-canary".to_string()),
            namespace: Some(ctx.namespace.clone()),
            labels: Some(labels.clone()),
            ..Default::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(1), // Start with 1 replica for canary
            selector: LabelSelector {
                match_labels: Some(labels.clone()),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels.clone()),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    containers: vec![Container {
                        name: "app".to_string(),
                        image: Some(ctx.config.deployment.canary_image.clone()),
                        ports: Some(vec![k8s_openapi::api::core::v1::ContainerPort {
                            container_port: 80,
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        status: None,
    };

    let deployments: Api<Deployment> = Api::namespaced(ctx.client.clone(), &ctx.namespace);
    deployments
        .create(&PostParams::default(), &deployment)
        .await?;

    Ok(())
}

/// Create services for stable and canary
async fn create_services(ctx: &TestContext) -> TestResult {
    let services: Api<Service> = Api::namespaced(ctx.client.clone(), &ctx.namespace);

    // Stable service
    let mut stable_labels = BTreeMap::new();
    stable_labels.insert("app".to_string(), "myapp".to_string());
    stable_labels.insert("version".to_string(), "stable".to_string());

    let stable_svc = Service {
        metadata: ObjectMeta {
            name: Some("app-stable".to_string()),
            namespace: Some(ctx.namespace.clone()),
            ..Default::default()
        },
        spec: Some(ServiceSpec {
            selector: Some(stable_labels),
            ports: Some(vec![ServicePort {
                port: 80,
                ..Default::default()
            }]),
            ..Default::default()
        }),
        status: None,
    };

    services.create(&PostParams::default(), &stable_svc).await?;

    // Canary service
    let mut canary_labels = BTreeMap::new();
    canary_labels.insert("app".to_string(), "myapp".to_string());
    canary_labels.insert("version".to_string(), "canary".to_string());

    let canary_svc = Service {
        metadata: ObjectMeta {
            name: Some("app-canary".to_string()),
            namespace: Some(ctx.namespace.clone()),
            ..Default::default()
        },
        spec: Some(ServiceSpec {
            selector: Some(canary_labels),
            ports: Some(vec![ServicePort {
                port: 80,
                ..Default::default()
            }]),
            ..Default::default()
        }),
        status: None,
    };

    services.create(&PostParams::default(), &canary_svc).await?;

    Ok(())
}

/// Create HTTPRoute for traffic splitting
async fn create_httproute(ctx: &TestContext) -> TestResult {
    use gateway_api::apis::standard::httproutes::{
        HTTPRoute, HTTPRouteRulesBackendRefs, HTTPRouteSpec,
    };

    let routes: Api<HTTPRoute> = Api::namespaced(ctx.client.clone(), &ctx.namespace);

    let httproute = HTTPRoute {
        metadata: ObjectMeta {
            name: Some("app-route".to_string()),
            namespace: Some(ctx.namespace.clone()),
            ..Default::default()
        },
        spec: HTTPRouteSpec {
            parent_refs: None, // No actual gateway in test
            rules: Some(vec![
                gateway_api::apis::standard::httproutes::HTTPRouteRules {
                    name: None, // Optional rule name
                    backend_refs: Some(vec![
                        HTTPRouteRulesBackendRefs {
                            name: "app-stable".to_string(),
                            port: Some(80),
                            weight: Some(100), // Start with 100% stable
                            kind: Some("Service".to_string()),
                            group: Some("".to_string()),
                            namespace: None,
                            filters: None,
                        },
                        HTTPRouteRulesBackendRefs {
                            name: "app-canary".to_string(),
                            port: Some(80),
                            weight: Some(0), // Start with 0% canary
                            kind: Some("Service".to_string()),
                            group: Some("".to_string()),
                            namespace: None,
                            filters: None,
                        },
                    ]),
                    filters: None,
                    matches: None,
                    timeouts: None,
                },
            ]),
            ..Default::default()
        },
        status: None,
    };

    routes.create(&PostParams::default(), &httproute).await?;

    Ok(())
}

/// Update HTTPRoute traffic split
async fn update_traffic_split(ctx: &TestContext, canary_weight: i32) -> TestResult {
    use gateway_api::apis::standard::httproutes::HTTPRoute;
    use kube::api::{Patch, PatchParams};

    let routes: Api<HTTPRoute> = Api::namespaced(ctx.client.clone(), &ctx.namespace);

    let stable_weight = 100 - canary_weight;

    let patch = serde_json::json!({
        "spec": {
            "rules": [{
                "backendRefs": [
                    {
                        "name": "app-stable",
                        "port": 80,
                        "weight": stable_weight,
                        "kind": "Service",
                        "group": ""
                    },
                    {
                        "name": "app-canary",
                        "port": 80,
                        "weight": canary_weight,
                        "kind": "Service",
                        "group": ""
                    }
                ]
            }]
        }
    });

    routes
        .patch("app-route", &PatchParams::default(), &Patch::Merge(&patch))
        .await?;

    Ok(())
}
