//! eslabon: re-export of the PHI egress policy, which lives in the shared HTTP transport.

pub use codex_http_client::eslabon_policy::*;

pub(crate) fn check_egress(url: &str) -> Result<(), crate::error::ApiError> {
    match egress_violation(url) {
        Some(message) => Err(crate::error::ApiError::InvalidRequest { message }),
        None => Ok(()),
    }
}
