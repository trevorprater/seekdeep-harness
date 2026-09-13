//! Opaque cursor identity for session-search pagination.

seekdeep_util::string_brand!(
    /// Provider-owned opaque continuation token returned by session search.
    pub struct SessionSearchCursor;
);
