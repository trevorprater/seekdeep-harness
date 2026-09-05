//! Model-facing remediation for guarded-mutation failures.

use seekdeep_fs::{FsError, FsErrorCode};

/// The remedy appended to each remediable failure code's message.
fn remedy_for(code: FsErrorCode) -> Option<&'static str> {
    match code {
        FsErrorCode::FsStaleVersion => Some("re-read the file, then retry"),
        FsErrorCode::FsNotObserved => Some("read the file, then retry"),
        _ => None,
    }
}

/// Appends the correct recovery instruction to a guarded-mutation failure's
/// message, preserving the error code and passing anything else through
/// untouched.
#[must_use]
pub fn remediate_fs_error(error: anyhow::Error) -> anyhow::Error {
    let Some(fs_error) = error.downcast_ref::<FsError>() else {
        return error;
    };
    let Some(remedy) = remedy_for(fs_error.code) else {
        return error;
    };
    anyhow::Error::new(FsError::new(
        format!("{} — {remedy}", fs_error.message),
        fs_error.code,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remediates_guarded_mutation_codes() {
        let stale = anyhow::Error::new(FsError::new(
            "the file changed",
            FsErrorCode::FsStaleVersion,
        ));
        let remediated = remediate_fs_error(stale);
        let fs_error = remediated.downcast_ref::<FsError>().expect("FsError");
        assert_eq!(fs_error.code, FsErrorCode::FsStaleVersion);
        assert_eq!(
            fs_error.message,
            "the file changed — re-read the file, then retry"
        );
    }

    #[test]
    fn passes_through_unremediable_and_non_fs_errors() {
        let io = anyhow::Error::new(FsError::new("io", FsErrorCode::FsIoError));
        let remediated = remediate_fs_error(io);
        let fs_error = remediated.downcast_ref::<FsError>().expect("FsError");
        assert_eq!(fs_error.message, "io");

        let other = anyhow::anyhow!("plain");
        assert_eq!(remediate_fs_error(other).to_string(), "plain");
    }
}
