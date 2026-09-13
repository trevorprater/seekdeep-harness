//! Pure types of the title domain.

/// The `title` projection key's wire value: the current normalized title, or
/// none before the first title lands.
pub type TitleProjection = Option<String>;
