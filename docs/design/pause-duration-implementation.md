# Pause Duration & Manual Promotion Implementation

**Status**: In Progress (TDD Cycle 18)
**Started**: 2025-11-20
**Goal**: Add time-based pauses and manual promotion to KULTA canary rollouts

---

## Problem Statement

Currently, KULTA automatically progresses through all canary steps immediately. This is not realistic for production use. We need:

1. **Time-based pauses**: Wait N minutes/hours at each step before progressing
2. **Manual promotion**: Allow humans to approve progression at critical steps

### Current Behavior (Before)

```yaml
steps:
- setWeight: 20
- setWeight: 50
- setWeight: 100
```

**Result**: Rollout completes in <1 second (all steps progress immediately)

### Desired Behavior (After)

```yaml
steps:
- setWeight: 20
  pause:
    duration: "5m"  # Wait 5 minutes
- setWeight: 50
  pause: {}  # Indefinite pause (requires manual promotion)
- setWeight: 100
```

**Result**:
- Step 0 (20%): Wait 5 minutes → Auto-progress
- Step 1 (50%): Wait indefinitely → Requires `kubectl annotate rollout <name> kulta.io/promote=true`
- Step 2 (100%): Complete

---

## Design

### CRD Changes

**Added to `RolloutStatus`** (`src/crd/rollout.rs`):

```rust
/// Timestamp when current pause started (RFC3339 format)
#[serde(rename = "pauseStartTime", skip_serializing_if = "Option::is_none")]
pub pause_start_time: Option<String>,
```

**Already exists in `PauseDuration`**:

```rust
pub struct PauseDuration {
    /// Duration in seconds (e.g., "30s", "5m")
    /// If not specified, pauses indefinitely until manually resumed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
}
```

### Duration Parser

**Function**: `parse_duration(duration_str: &str) -> Option<Duration>`

**Supported formats**:
- `"30s"` → 30 seconds
- `"5m"` → 5 minutes (300 seconds)
- `"2h"` → 2 hours (7200 seconds)

**Implementation** (`src/controller/rollout.rs`):

```rust
pub fn parse_duration(duration_str: &str) -> Option<Duration> {
    let duration_str = duration_str.trim();
    if duration_str.is_empty() {
        return None;
    }

    let unit = duration_str.chars().last()?;
    let number_str = &duration_str[..duration_str.len() - 1];
    let number: u64 = number_str.parse().ok()?;

    match unit {
        's' => Some(Duration::from_secs(number)),
        'm' => Some(Duration::from_secs(number * 60)),
        'h' => Some(Duration::from_secs(number * 3600)),
        _ => None,
    }
}
```

**Tests** (6 tests, all passing):
- ✅ `test_parse_duration_seconds`
- ✅ `test_parse_duration_minutes`
- ✅ `test_parse_duration_hours`
- ✅ `test_parse_duration_invalid_unit`
- ✅ `test_parse_duration_empty_string`
- ✅ `test_parse_duration_no_number`

---

## Implementation Plan (TDD)

### ✅ Completed: TDD Cycle 18 - Part 1 (Duration Parser)

**Commit**: `2dbc1ac` - "feat: add duration parser for pause support"

**Changes**:
1. Added `pauseStartTime` field to `RolloutStatus`
2. Implemented `parse_duration()` function
3. Added 6 unit tests for duration parsing
4. All tests passing (30 total)

### 🚧 In Progress: TDD Cycle 18 - Part 2 (Time-based Progression)

**Goal**: Update `should_progress_to_next_step()` to check if pause duration has elapsed

**Current Logic** (`src/controller/rollout.rs:344-381`):

```rust
pub fn should_progress_to_next_step(rollout: &Rollout) -> bool {
    // Get current status
    let status = match &rollout.status {
        Some(status) => status,
        None => return false,
    };

    // If phase is Paused, don't progress
    if status.phase.as_deref() == Some("Paused") {
        return false;
    }

    // Get current step
    let current_step = match canary_strategy.steps.get(current_step_index as usize) {
        Some(step) => step,
        None => return false,
    };

    // If current step has pause, don't progress
    if current_step.pause.is_some() {
        return false;  // ❌ ALWAYS blocks progression if pause exists
    }

    true
}
```

**New Logic** (To Implement):

```rust
pub fn should_progress_to_next_step(rollout: &Rollout) -> bool {
    // ... same preamble ...

    // Check if current step has pause
    if let Some(pause) = &current_step.pause {
        // Check for manual promotion annotation
        if has_promote_annotation(rollout) {
            return true;  // Manual promotion overrides pause
        }

        // If pause has duration, check if elapsed
        if let Some(duration_str) = &pause.duration {
            if let Some(duration) = parse_duration(duration_str) {
                // Check if pause started
                if let Some(pause_start_str) = &status.pause_start_time {
                    // Parse pause start time (RFC3339)
                    if let Ok(pause_start) = DateTime::parse_from_rfc3339(pause_start_str) {
                        let now = Utc::now();
                        let elapsed = now.signed_duration_since(pause_start);

                        // If duration elapsed, can progress
                        if elapsed.num_seconds() >= duration.as_secs() as i64 {
                            return true;
                        }
                    }
                }
            }
        }

        // Pause is active and duration not elapsed
        return false;
    }

    true
}
```

**Dependency**: Need to add `chrono` crate for RFC3339 parsing

```toml
[dependencies]
chrono = { version = "0.4", features = ["serde"] }
```

### 📋 TODO: TDD Cycle 18 - Part 3 (Set Pause Start Time)

**Goal**: Update `advance_to_next_step()` to set `pauseStartTime` when entering a step with pause

**Current Logic** (`src/controller/rollout.rs:425-526`):

```rust
pub fn advance_to_next_step(rollout: &Rollout) -> crate::crd::rollout::RolloutStatus {
    // ... calculate next_step_index, next_weight ...

    RolloutStatus {
        current_step_index: Some(next_step_index),
        current_weight: Some(next_weight),
        phase: Some(phase),
        message: Some(message),
        ..current_status.clone()
    }
}
```

**New Logic** (To Implement):

```rust
pub fn advance_to_next_step(rollout: &Rollout) -> crate::crd::rollout::RolloutStatus {
    // ... same preamble ...

    // Check if next step has pause
    let pause_start_time = if next_step.pause.is_some() {
        // Set pause start time to now (RFC3339)
        Some(Utc::now().to_rfc3339())
    } else {
        // Clear pause start time if no pause
        None
    };

    RolloutStatus {
        current_step_index: Some(next_step_index),
        current_weight: Some(next_weight),
        phase: Some(phase),
        message: Some(message),
        pause_start_time,  // NEW FIELD
        ..current_status.clone()
    }
}
```

### 📋 TODO: TDD Cycle 18 - Part 4 (Manual Promotion)

**Goal**: Check for `kulta.io/promote=true` annotation

**Helper Function** (To Implement):

```rust
/// Check if Rollout has the promote annotation
fn has_promote_annotation(rollout: &Rollout) -> bool {
    rollout
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get("kulta.io/promote"))
        .map(|value| value == "true")
        .unwrap_or(false)
}
```

**Usage**:

```bash
# User manually promotes rollout
kubectl annotate rollout test-rollout kulta.io/promote=true -n demo

# Controller sees annotation in next reconcile loop (5 minutes max)
# → should_progress_to_next_step() returns true
# → advance_to_next_step() is called
# → Rollout progresses to next step

# Controller removes annotation after promotion
kubectl annotate rollout test-rollout kulta.io/promote- -n demo
```

**Implementation in reconcile()** (To Add):

```rust
pub async fn reconcile(rollout: Arc<Rollout>, ctx: Arc<Context>) -> Result<Action, ReconcileError> {
    // ... existing ReplicaSet and HTTPRoute logic ...

    // Compute desired status (with pause/promotion logic)
    let desired_status = compute_desired_status(&rollout);

    // If status changed, update it
    if rollout.status.as_ref() != Some(&desired_status) {
        // ... patch status ...

        // If we just promoted, remove the promote annotation
        if has_promote_annotation(&rollout) {
            remove_promote_annotation(&rollout, &ctx).await?;
        }
    }

    Ok(Action::requeue(Duration::from_secs(300)))
}
```

---

## Test Plan

### Unit Tests (To Add)

**Time-based Progression**:

```rust
#[tokio::test]
async fn test_should_progress_when_pause_duration_elapsed() {
    // Create rollout with step that has 5m pause
    // Set pauseStartTime to 6 minutes ago
    // should_progress_to_next_step() should return true
}

#[tokio::test]
async fn test_should_not_progress_when_pause_duration_not_elapsed() {
    // Create rollout with step that has 5m pause
    // Set pauseStartTime to 2 minutes ago
    // should_progress_to_next_step() should return false
}

#[tokio::test]
async fn test_advance_sets_pause_start_time() {
    // Advance to step with pause
    // new_status.pause_start_time should be Some(RFC3339 timestamp)
}
```

**Manual Promotion**:

```rust
#[tokio::test]
async fn test_has_promote_annotation() {
    // Create rollout with kulta.io/promote=true annotation
    // has_promote_annotation() should return true
}

#[tokio::test]
async fn test_should_progress_when_promoted() {
    // Create rollout with indefinite pause
    // Add kulta.io/promote=true annotation
    // should_progress_to_next_step() should return true
}
```

### Integration Tests (To Add)

**Test 1: Time-based Pause**

```yaml
apiVersion: kulta.io/v1alpha1
kind: Rollout
metadata:
  name: time-pause-test
spec:
  replicas: 3
  strategy:
    canary:
      steps:
      - setWeight: 20
        pause:
          duration: "10s"  # 10 second pause for testing
      - setWeight: 100
```

**Expected behavior**:
1. Rollout created → status.currentStepIndex=0, status.pauseStartTime set
2. Wait 5 seconds → still at step 0
3. Wait 10+ seconds → progresses to step 1 (100%)
4. Verify HTTPRoute weights updated correctly

**Test 2: Manual Promotion**

```yaml
apiVersion: kulta.io/v1alpha1
kind: Rollout
metadata:
  name: manual-pause-test
spec:
  replicas: 3
  strategy:
    canary:
      steps:
      - setWeight: 20
        pause: {}  # Indefinite pause
      - setWeight: 100
```

**Expected behavior**:
1. Rollout created → status.currentStepIndex=0, paused indefinitely
2. Wait 10 minutes → still at step 0
3. Annotate: `kubectl annotate rollout manual-pause-test kulta.io/promote=true`
4. Within 5 minutes → progresses to step 1 (100%)
5. Annotation removed automatically
6. Verify HTTPRoute weights updated correctly

---

## Dependencies

### Cargo.toml Changes Needed

```toml
[dependencies]
# Add chrono for RFC3339 timestamp parsing
chrono = { version = "0.4", features = ["serde"] }
```

### Imports to Add

```rust
use chrono::{DateTime, Utc};
```

---

## Files Modified

### Completed

1. **`src/crd/rollout.rs`**
   - Added `pause_start_time` field to `RolloutStatus`

2. **`src/controller/rollout.rs`**
   - Added `parse_duration()` function (lines 738-770)

3. **`src/controller/rollout_test.rs`**
   - Added 6 tests for `parse_duration()` (lines 1373-1416)

### To Modify

4. **`Cargo.toml`**
   - Add `chrono` dependency

5. **`src/controller/rollout.rs`**
   - Update `should_progress_to_next_step()` to check pause duration elapsed
   - Update `advance_to_next_step()` to set `pauseStartTime`
   - Add `has_promote_annotation()` helper
   - Add `remove_promote_annotation()` helper
   - Update `reconcile()` to remove annotation after promotion

6. **`src/controller/rollout_test.rs`**
   - Add 5+ tests for time-based progression
   - Add 2+ tests for manual promotion

7. **`tests/integration_pause.rs`** (new file)
   - Integration test for time-based pause
   - Integration test for manual promotion

---

## Current Status

**Completed**:
- ✅ Duration parser implementation
- ✅ 6 unit tests for duration parsing
- ✅ CRD field for pause start time
- ✅ 1 commit pushed

**In Progress**:
- 🚧 Time-based progression logic

**Remaining**:
- ⏳ Set pause start time on step advance
- ⏳ Manual promotion annotation support
- ⏳ Unit tests for progression logic
- ⏳ Integration tests
- ⏳ Push all changes
- ⏳ Update CI to test new features

**Total Progress**: ~20% complete

---

## Next Steps

1. Add `chrono` dependency to `Cargo.toml`
2. Implement time-based progression in `should_progress_to_next_step()`
3. Write tests (TDD RED phase)
4. Make tests pass (TDD GREEN phase)
5. Implement pause start time tracking in `advance_to_next_step()`
6. Implement manual promotion annotation support
7. Write integration tests
8. Push all changes
9. Verify CI passes

---

## Notes

- **Reconcile interval**: 5 minutes (300 seconds)
  - Pause durations shorter than 5 minutes may not be respected exactly
  - Consider reducing reconcile interval to 30-60 seconds for faster progression

- **Annotation cleanup**: Controller removes `kulta.io/promote` annotation after promotion
  - Prevents accidental double-promotion
  - Users can re-annotate to promote again if needed

- **Clock skew**: Using server-side timestamps (Utc::now()) to avoid client clock issues

- **Backward compatibility**:
  - Empty pause (`pause: {}`) works as indefinite pause (existing behavior)
  - No pause works as immediate progression (existing behavior)
  - New duration field is optional

---

**Last Updated**: 2025-11-20
**Author**: Claude + Yair
**Commits**: 1 (2dbc1ac)
