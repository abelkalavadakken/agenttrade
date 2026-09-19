//! The gRPC server and the process that owns the core. See docs/api.md.

pub mod convert;
pub mod core;
pub mod hash;
pub mod server;

// Generated tonic code returns Status by value; 176 bytes is tonic's convention.
#[allow(clippy::result_large_err, clippy::large_enum_variant)]
pub mod v1 {
    tonic::include_proto!("agenttrade.v1");
}

pub use core::{spawn_core, Core, CoreConfig, CoreHandle, CoreInput, StateSnapshot, SOURCES};
pub use hash::StateHasher;
pub use server::{Clock, Service};
