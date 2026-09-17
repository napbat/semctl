//! The shared engine.
//!
//! One engine per process owns everything that must not be duplicated per
//! session: the permits that bound concurrent work and, in later stages, the
//! checkout registry and the filesystem watch hub. A session holds a handle to
//! the engine and nothing that another session with the same checkout would own
//! a second copy of.

pub(crate) mod scheduler;
pub(crate) mod watch_hub;

pub(crate) use scheduler::Scheduler;
