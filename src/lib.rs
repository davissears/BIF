//! Library support for the BIF task ledger.
//!
//! Dependencies point inward: delivery and infrastructure modules may use the
//! application and domain layers, while [`domain`] remains independent of all
//! other crate modules.

pub mod application;
pub mod cli;
pub mod config;
pub mod domain;
pub mod rpc;
pub mod storage;

/// Runs the BIF command-line application.
///
/// Command handling will be added by later implementation tasks.
pub fn run() {}
