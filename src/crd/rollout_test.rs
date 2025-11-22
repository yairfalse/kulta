#![allow(clippy::unwrap_used)] // Tests can use unwrap for brevity
#![allow(clippy::expect_used)] // Tests can use expect for better error messages

use super::*;
use kube::CustomResourceExt;

#[test]
fn test_rollout_deserialize_from_yaml() {
    let yaml = r#"
apiVersion: kulta.io/v1alpha1
kind: Rollout
metadata:
  name: test-rollout
spec:
  replicas: 3
  selector:
    matchLabels:
      app: test-app
  template:
    metadata:
      labels:
        app: test-app
    spec:
      containers:
      - name: app
        image: nginx:latest
  strategy:
    canary:
      canaryService: test-app-canary
      stableService: test-app-stable
      steps:
      - setWeight: 20
      - pause:
          duration: 30s
      - setWeight: 50
      trafficRouting:
        gatewayAPI:
          httpRoute: test-route
"#;

    let rollout: Rollout = serde_yaml::from_str(yaml).expect("Failed to deserialize Rollout");

    assert_eq!(rollout.metadata.name.as_deref(), Some("test-rollout"));
    assert_eq!(rollout.spec.replicas, 3);
    assert!(rollout.spec.strategy.canary.is_some());

    let canary = rollout.spec.strategy.canary.unwrap();
    assert_eq!(canary.canary_service, "test-app-canary");
    assert_eq!(canary.stable_service, "test-app-stable");
    assert_eq!(canary.steps.len(), 3);
    assert_eq!(canary.steps[0].set_weight, Some(20));
    assert!(canary.steps[1].pause.is_some());
    assert_eq!(canary.steps[2].set_weight, Some(50));

    assert!(canary.traffic_routing.is_some());
    let traffic = canary.traffic_routing.unwrap();
    assert!(traffic.gateway_api.is_some());
    assert_eq!(traffic.gateway_api.unwrap().http_route, "test-route");
}

#[test]
fn test_rollout_crd_schema_generation() {
    // Generate CRD YAML that gets installed in Kubernetes
    let crd = Rollout::crd();

    assert_eq!(crd.spec.group, "kulta.io");
    assert_eq!(crd.spec.names.kind, "Rollout");
    assert_eq!(crd.spec.names.plural, "rollouts");

    // Verify schema exists (validates CRD structure)
    assert!(!crd.spec.versions.is_empty());
    let version = &crd.spec.versions[0];
    assert_eq!(version.name, "v1alpha1");
    assert!(version.served);
    assert!(version.storage);
    assert!(version.schema.is_some());
}
