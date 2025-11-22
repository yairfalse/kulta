// Strict code quality lints
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]

pub mod controller;
pub mod crd;

// Re-export for main.rs tests
pub use crate::controller::{reconcile, Context, ReconcileError};
