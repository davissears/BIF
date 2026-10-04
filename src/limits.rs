//! Neutral encoded transport budgets, shared by application and delivery code.

/// Maximum complete encoded v2 request, before configuration or storage access.
pub const MAXIMUM_REQUEST_BYTES: usize = 1_048_576;
/// Maximum complete encoded v2 response (excluding the CLI newline).
pub const MAXIMUM_RESPONSE_BYTES: usize = 1_048_576;
/// Maximum complete encoded continuation token, including its version prefix.
pub const MAX_CURSOR_BYTES: usize = 1_048_576;
