# KULTA 

**Progressive Delivery for Kubernetes - Simple, Fast, Observable**

[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)
[![Gateway API](https://img.shields.io/badge/Gateway%20API-v1.2.0-purple.svg)](https://gateway-api.sigs.k8s.io/)
[![CDEvents](https://img.shields.io/badge/CDEvents-v0.4.1-orange.svg)](https://cdevents.dev)

**Tech Stack:**
[![Tokio](https://img.shields.io/badge/async-tokio-blue.svg?logo=rust)](https://tokio.rs)
[![Kubernetes](https://img.shields.io/badge/K8s-kube--rs-326CE5.svg?logo=kubernetes&logoColor=white)](https://kube.rs)
[![Prometheus](https://img.shields.io/badge/metrics-prometheus-E6522C.svg?logo=prometheus&logoColor=white)](https://prometheus.io)

---

## What is KULTA?

A progressive delivery controller for Kubernetes - deploy safely with canary rollouts, no service mesh required.

**What's Actually Built:**
- Gateway API-native traffic routing (no service mesh!)
- Automated canary analysis (Prometheus metrics)
- CDEvents emission (full pipeline observability)
- Auto-rollback on errors
- Written in Rust for performance

**Why Build This?**
- Learn Rust + Kubernetes controllers (building on RAUTA)
- Explore progressive delivery patterns
- Make CD pipelines observable via CDEvents
- Have fun building systems software

---

## What Works

**Progressive Delivery** 🚀 (Planned)
- Canary rollouts (10% → 50% → 100%)
- Blue-green deployments
- Automated traffic shifting via Gateway API
- Manual pause/resume controls

**Safety & Analysis** 🛡️ (Planned)
- Prometheus metrics analysis
- Automated rollback on errors
- Configurable thresholds (error rate, latency)
- Health checking integration

**Observability** 📊 (Planned)
- CDEvents emission (every deployment step)
- Git commit → deployment correlation
- Full pipeline tracing (with Tekton/CDviz)
- Prometheus metrics

**Gateway API Integration** 🌐 (Planned)
- HTTPRoute weight manipulation
- Works with RAUTA or any Gateway API implementation
- No service mesh sidecars required
- Simple, transparent traffic routing

---

## Architecture

```
┌─────────────────────────────────────────────┐
│   Developer Workflow                        │
├─────────────────────────────────────────────┤
│   git push                                  │
│   ↓                                         │
│   Tekton/Jenkins (CI)                       │
│   ↓ (emits CDEvents: artifact.published)    │
│   Argo CD / FluxCD (GitOps)                 │
│   ↓ (syncs Rollout YAML from git)           │
│   KULTA Controller                          │
│   ├─ Creates canary ReplicaSet              │
│   ├─ Emits: deployment.started (CDEvent)    │
│   ├─ Updates Gateway API HTTPRoute weights  │
│   ├─ Queries Prometheus for health          │
│   ├─ Auto-rollback if errors OR             │
│   └─ Advance: 10% → 50% → 100%              │
│   ↓                                         │
│   RAUTA / Gateway API                       │
│   ↓ (routes traffic based on weights)       │
│   CDviz / Observability                     │
│   └─ Shows: git commit → deploy → health    │
└─────────────────────────────────────────────┘
```

**The Stack:**
```
RAUTA ⚙️ (Gateway API routing)
  ↓ routes traffic
KULTA 🏆 (Progressive delivery)
  ↓ manages deployments
Both: Rust + Gateway API native = Simple, fast, integrated
```

---

## Quick Start

```bash
# Clone and build
git clone https://github.com/yairfalse/kulta
cd kulta
cargo build --release

# Run controller (requires KUBECONFIG)
./target/release/kulta

# Deploy in Kubernetes
kubectl apply -f manifests/
```

**Requirements:**
- Rust 1.75+
- Kubernetes cluster (kind/minikube/production)
- Gateway API CRDs installed
- KUBECONFIG configured

---

## Design Choices

**Why Progressive Delivery instead of just GitOps?**

GitOps (Argo CD, FluxCD) syncs what's in git to the cluster. Progressive delivery controls **how** changes roll out - gradually, safely, with automatic rollback.

**Why Gateway API instead of Service Mesh?**

Service meshes (Istio, Linkerd) add complexity:
- Sidecar containers (+50MB memory per pod)
- Complex configuration (VirtualService, DestinationRule, etc.)
- Hard to debug (traffic routing in mesh)

Gateway API is simpler:
- Just HTTPRoute weight changes
- No sidecars
- `kubectl get httproute` shows traffic splits
- Works with RAUTA or any Gateway API implementation

**Why CDEvents?**

Current state: CI emits events (Tekton, Jenkins), CD doesn't (Argo Rollouts, Flagger). Your pipeline visibility is broken.

KULTA bridges the gap:
- Emits CDEvents at every deployment step
- Links git commit → build → deploy → health
- Works with CDviz for full pipeline observability

**Why Rust?**

- Memory safety (no segfaults)
- Strong type system (catch bugs at compile time)
- Excellent async ecosystem (tokio)
- Performance (fast reconciliation loops)
- Building on RAUTA knowledge

---

## Comparison

**vs Argo Rollouts:**
- Argo: Go-based, service mesh for advanced features
- KULTA: Rust-based, Gateway API-native, CDEvents built-in

**vs Flagger:**
- Flagger: Requires service mesh (Istio/Linkerd)
- KULTA: Gateway API only (simpler stack)

**vs Both:**
- Argo/Flagger: No CDEvents (manual correlation across tools)
- KULTA: Full pipeline tracing (git → deploy → production)

---

## Technology Stack

- **tokio** - Async runtime
- **kube-rs** - Kubernetes API client
- **gateway-api** - Official Gateway API CRD types
- **prometheus** - Metrics analysis
- **cdevents** - Event emission
- **serde** - Serialization

---

## Development

### Build and Test

```bash
# Build
cargo build

# Run tests
cargo test

# Format
cargo fmt

# Lint
cargo clippy -- -D warnings
```

### TDD Workflow

All features follow Test-Driven Development:

1. **RED**: Write failing test
2. **GREEN**: Minimal implementation to pass
3. **REFACTOR**: Improve code quality
4. **COMMIT**: Small, focused commits

See `CLAUDE.md` for detailed guidelines.

---

## Naming

**Kulta** (Finnish: "gold") - Part of the Finnish tool naming theme:
- **RAUTA**: Gateway API routing ⚙️ (iron)
- **KULTA**: Progressive delivery 🏆 (gold - your precious deployments)
- **TAPIO**: Kubernetes observer 🌲
- **AHTI**: Event correlation 🌊

Iron routes your traffic, Gold protects your deployments.

---

## License

Apache 2.0 - Free and open source.

---

## Links

- **GitHub**: https://github.com/yairfalse/kulta
- **RAUTA**: https://github.com/yairfalse/rauta
- **CDEvents**: https://cdevents.dev

---

**Built for fun. Keeps deployments safe.** 🦀
