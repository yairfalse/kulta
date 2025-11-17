pub mod controller;
pub mod crd;

// Re-export for main.rs tests
pub use crate::controller::{reconcile, Context, ReconcileError};
