//! Subcommand implementations. Each module owns one `semctl` subcommand — its
//! clap args plus a `run` entry point — dispatched from [`crate::cli`].
//! (`uninstall` lives in [`install`], next to the install logic it reverses.)

pub mod auth;
pub mod edit;
pub mod files;
pub mod graph;
pub mod hook;
pub mod index;
pub mod inspect;
pub mod install;
pub mod search;
pub mod upgrade;
