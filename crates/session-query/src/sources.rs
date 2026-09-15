//! Shared immutable-header checks for logical session source observers.

use seekdeep_core::session::SessionHeader;

use crate::config::{SessionQueryError, SessionQueryErrorCode};

/// Rejects incompatible observations of one logical session source.
///
/// # Errors
///
/// Returns a source-conflict failure when the headers disagree.
pub fn assert_session_headers_compatible(
    a: &SessionHeader,
    b: &SessionHeader,
) -> anyhow::Result<()> {
    if a.version != b.version
        || a.id != b.id
        || a.created_at != b.created_at
        || a.cwd != b.cwd
        || a.parent_session != b.parent_session
        || a.seed_length != b.seed_length
        || a.delegation_depth.unwrap_or(0) != b.delegation_depth.unwrap_or(0)
    {
        return Err(SessionQueryError::new(
            format!("session source headers conflict for session \"{}\"", a.id),
            SessionQueryErrorCode::SessionQuerySourceConflict,
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use seekdeep_core::session::SessionId;

    use super::*;

    fn header(cwd: Option<&str>) -> SessionHeader {
        SessionHeader {
            version: 0,
            id: SessionId::new("s1"),
            created_at: 100,
            cwd: cwd.map(str::to_owned),
            parent_session: None,
            seed_length: None,
            origin: None,
            delegation_depth: None,
            agent_preset: None,
        }
    }

    #[test]
    fn compatible_headers_pass_and_conflicts_fail() {
        assert!(
            assert_session_headers_compatible(&header(Some("/w")), &header(Some("/w"))).is_ok()
        );
        let error = assert_session_headers_compatible(&header(Some("/a")), &header(Some("/b")))
            .expect_err("conflicting cwd");
        assert!(format!("{error:#}").contains("headers conflict"));
    }
}
