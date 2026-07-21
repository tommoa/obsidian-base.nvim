#![forbid(unsafe_code)]
//! Native JSON-lines worker for evaluating Obsidian Bases against a vault index.

pub mod actor;
pub mod error;
pub mod expression;
pub mod index;
pub mod limits;
pub mod markdown;
pub mod path;
pub mod protocol;
pub mod query;
pub mod service;
pub mod value;
pub mod watch;
pub mod yaml;

pub use error::{Result, WorkerError};
pub use service::WorkerService;
