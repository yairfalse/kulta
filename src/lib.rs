pub mod crd;
pub mod controller;

// Re-export for main.rs tests
pub use crate::controller::{reconcile, Context, ReconcileError};
