# Skaffold Development Workflow - KULTA

Fast iteration workflow for KULTA canary rollout controller development.

---

## Prerequisites

```bash
# Install Skaffold
brew install skaffold  # macOS
# or: https://skaffold.dev/docs/install/

# Install Kind (if not already)
brew install kind

# Verify installation
skaffold version
kind version
```

---

## Quick Start

### 1. Create Kind Cluster

```bash
# Create cluster with Gateway API CRDs
kind create cluster --name kulta-dev

# Install Gateway API CRDs (required by KULTA)
kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.1.0/standard-install.yaml

# Create namespaces
kubectl create namespace kulta-system
kubectl create namespace manual-promo-test
kubectl create namespace time-based-test
```

### 2. Start Development Loop

```bash
# Continuous development mode (auto-rebuild on change)
skaffold dev

# Terminal output:
# - Builds kulta-controller Docker image
# - Deploys to Kind cluster
# - Tails logs from all pods
# - Watches for file changes
# - Auto-rebuilds + redeploys on Rust file changes
```

**That's it!** Edit any Rust file in `src/` and Skaffold will:
1. Rebuild the Docker image
2. Redeploy to Kind
3. Show you the logs

---

## Workflows

### Development (Default)

```bash
skaffold dev

# Edit src/controller.rs
# ... Skaffold detects change ...
# Building kulta-controller...
# Deploying...
# Streaming logs...
```

**Port forwards automatically:**
- `localhost:8080` - KULTA controller health endpoint
- `localhost:9091` - KULTA metrics (Prometheus)

**Test the controller:**
```bash
# Check controller is running
curl http://localhost:8080/healthz

# View metrics
curl http://localhost:9091/metrics

# Create a test Rollout
kubectl apply -f examples/manual-promotion-test.yaml
```

### Test Profiles

KULTA has specialized test profiles for different canary scenarios:

#### Manual Promotion Test

```bash
# Deploy with manual promotion example
skaffold dev -p test-manual

# This loads:
# - KULTA CRD, controller, RBAC
# - examples/manual-promotion-test.yaml (demo rollout)

# Watch canary progress
kubectl get rollouts -n manual-promo-test -w

# Manually promote canary
kubectl patch rollout nginx -n manual-promo-test --type=merge -p '{"spec":{"paused":false}}'
```

#### Time-Based Pause Test

```bash
# Deploy with time-based pause example
skaffold dev -p test-time

# This loads:
# - KULTA CRD, controller, RBAC
# - examples/time-based-pause-test.yaml (auto-promote after delay)

# Watch automatic progression
kubectl get rollouts -n time-based-test -w
```

#### Full Demo

```bash
# Deploy all examples
skaffold dev -p demo

# This loads:
# - KULTA CRD, controller, RBAC
# - Manual promotion test
# - Time-based pause test
# - Port forwards for stable/canary services

# Access stable service
curl -H "Host: echo.local" http://localhost:8081

# Access canary service (after promotion)
curl -H "Host: echo.local" http://localhost:8081 -H "X-Canary: true"
```

### One-off Deploy

```bash
# Build and deploy once (no watch)
skaffold run

# Deploy stays running, but Skaffold exits
# Good for: testing specific commit, CI/CD
```

### Production-like Build

```bash
# Use release build (optimized, slower compile)
skaffold dev -p prod

# Uses Cargo release profile
# No dev tools included
```

### Delete Everything

```bash
# Stop skaffold dev (Ctrl+C)
# Then delete deployment
skaffold delete
```

---

## Tips & Tricks

### Faster Rebuilds

Skaffold uses file sync where possible:
```yaml
# Automatically configured in skaffold.yaml
sync:
  manual:
    - src: "src/**/*.rs"
      dest: /app/src
```

For small Rust changes, this can sync files instead of rebuilding entire image.

### Tail Specific Logs

```bash
# Only show KULTA controller logs
skaffold dev --tail

# Filter by pod
kubectl logs -f -l app=kulta-controller -n kulta-system
```

### Skip Tests

```bash
# Skip cargo test phase
skaffold dev --skip-tests
```

### Use Existing Cluster

```bash
# Deploy to existing cluster (not Kind)
skaffold dev --kube-context my-cluster

# Update skaffold.yaml:
deploy:
  kubeContext: my-cluster  # Change from kind-kulta-dev
```

---

## Troubleshooting

### "Image not found in Kind"

```bash
# Skaffold should handle this, but if issues:
skaffold dev --default-repo ""

# Force local build
skaffold config set --global local-cluster true
```

### "Build timeout"

```bash
# Increase build timeout (for slow machines)
skaffold dev --build-timeout 10m
```

### "Deployment not ready"

```bash
# Check pod status
kubectl get pods -n kulta-system

# Check controller logs
kubectl logs -l app=kulta-controller -n kulta-system

# Increase status check timeout (in skaffold.yaml)
deploy:
  statusCheckDeadlineSeconds: 300  # 5 minutes
```

### "Gateway API CRDs not found"

```bash
# Verify Gateway API CRDs are installed
kubectl get crd gateways.gateway.networking.k8s.io

# If missing, install them
kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.1.0/standard-install.yaml
```

### Clean Rebuild

```bash
# Delete Kind cluster and start fresh
kind delete cluster --name kulta-dev
kind create cluster --name kulta-dev

# Rebuild from scratch
skaffold dev --cache-artifacts=false
```

---

## Comparison: Before vs After

### Before Skaffold

```bash
# 1. Build
cargo build --release

# 2. Docker build
docker build -t kulta-controller:dev .

# 3. Load to Kind
kind load docker-image kulta-controller:dev --name kulta-dev

# 4. Deploy
kubectl apply -f deploy/

# 5. Check logs
kubectl logs -f ...

# 6. Make code change
# 7. REPEAT ALL STEPS ❌
```

**Time per iteration:** ~3-5 minutes

### After Skaffold

```bash
# 1. Start
skaffold dev

# 2. Make code change
# 3. Auto-rebuild + redeploy ✅

# Logs streaming automatically
```

**Time per iteration:** ~30-60 seconds (cached builds)

---

## Integration with RAUTA

KULTA works alongside RAUTA for progressive delivery:

**Combined workflow:**
```bash
# Terminal 1: Run RAUTA Gateway (port 8080)
cd ../rauta
skaffold dev

# Terminal 2: Run KULTA Canary Controller (port 9091)
cd ../kulta
skaffold dev -p demo

# Terminal 3: Test progressive rollout
curl -H "Host: echo.local" http://localhost:8080  # Routes to stable/canary based on weights
```

**Architecture:**
```
RAUTA Gateway (Port 8080)
    ↓ HTTPRoute traffic splitting
KULTA Rollout Controller (Port 9091)
    ↓ Manages canary weights
Kubernetes Deployments
    ↓ Stable + Canary pods
```

---

## CI/CD Integration

### GitHub Actions

```yaml
# .github/workflows/dev.yml
- name: Deploy to Kind with Skaffold
  run: |
    kind create cluster --name test
    kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.1.0/standard-install.yaml
    skaffold run -p test-manual

- name: Run integration tests
  run: |
    kubectl wait --for=condition=ready pod -l app=kulta-controller
    ./scripts/integration-tests.sh
```

### Local Pre-commit Testing

```bash
# Test before committing
skaffold run -p test-manual
cargo test --workspace
skaffold delete
```

---

## Canary Testing Workflows

### Manual Promotion Workflow

```bash
# 1. Start KULTA with manual test
skaffold dev -p test-manual

# 2. Watch rollout status
kubectl get rollout nginx -n manual-promo-test -w

# 3. Initial state: 100% stable, 0% canary
# NAME    STABLE   CANARY   PAUSED
# nginx   100%     0%       true

# 4. Update deployment (trigger canary)
kubectl set image deployment/nginx nginx=nginx:1.26 -n manual-promo-test

# 5. KULTA creates canary: 90% stable, 10% canary
# NAME    STABLE   CANARY   PAUSED
# nginx   90%      10%      true

# 6. Manually approve promotion
kubectl patch rollout nginx -n manual-promo-test --type=merge -p '{"spec":{"paused":false}}'

# 7. KULTA progresses: 50% → 100% canary
# NAME    STABLE   CANARY   PAUSED
# nginx   0%       100%     false

# 8. Canary becomes new stable
```

### Time-Based Workflow

```bash
# 1. Start KULTA with time-based test
skaffold dev -p test-time

# 2. Update deployment
kubectl set image deployment/nginx nginx=nginx:1.26 -n time-based-test

# 3. KULTA automatically progresses after pause duration
# - 10% canary (pause 30s)
# - 50% canary (pause 1m)
# - 100% canary (done)

# Watch automatic progression
kubectl get rollout nginx -n time-based-test -w
```

---

## Next Steps

1. **Try it out:** `skaffold dev`
2. **Edit code:** `src/controller.rs`
3. **See auto-rebuild:** Watch terminal
4. **Test canary:** `skaffold dev -p test-manual`
5. **Iterate fast:** Make changes, see results immediately

**Happy hacking! 🚀**

---

False Systems - Building progressive delivery tools that actually work 🇫🇮
