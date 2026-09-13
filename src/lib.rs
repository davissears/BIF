//! Library support for the BIF task ledger.
//!
//! Dependencies point inward: delivery and infrastructure modules may use the
//! application and domain layers, while [`domain`] remains independent of all
//! other crate modules.

pub mod application;
pub mod cli;
mod cli_mutation;
mod cli_read;
pub mod config;
pub mod domain;
pub mod expose;
pub mod rpc;
pub mod rpc_mutation;
pub mod rpc_read;
pub mod storage;
