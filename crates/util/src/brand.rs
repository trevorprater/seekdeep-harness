//! Nominal string-brand primitive for cross-package identifiers.

/// Defines one transparent nominal string identifier owned by the invoking
/// package.
///
/// Different invocations produce non-interchangeable Rust types even though
/// their display and serialized wire representation are ordinary strings. The
/// `repr(transparent)` newtype has the same runtime layout as `String`.
#[macro_export]
macro_rules! string_brand {
    ($(#[$metadata:meta])* $visibility:vis struct $name:ident;) => {
        $(#[$metadata])*
        #[repr(transparent)]
        #[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        $visibility struct $name(String);

        impl $name {
            #[doc = concat!("Brands a raw `", stringify!($name), "` string.")]
            #[must_use]
            $visibility fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Returns the ordinary protocol string.
            #[must_use]
            $visibility fn as_str(&self) -> &str {
                &self.0
            }

            /// Removes the nominal brand and returns the owned string.
            #[must_use]
            $visibility fn into_string(self) -> String {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl std::ops::Deref for $name {
            type Target = str;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl std::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

#[cfg(test)]
mod tests {
    crate::string_brand!(
        pub(super) struct FirstId;
    );
    crate::string_brand!(
        pub(super) struct SecondId;
    );

    #[test]
    fn brands_are_transparent_string_values() {
        let first = FirstId::new("same");
        let second = SecondId::new("same");
        assert_eq!(first.as_str(), "same");
        assert_eq!(second.as_str(), "same");
        assert_eq!(first.to_string(), "same");
        assert_eq!(serde_json::to_string(&first).unwrap(), "\"same\"");
        assert_eq!(
            std::mem::size_of::<FirstId>(),
            std::mem::size_of::<String>()
        );
        assert_eq!(second.into_string(), "same");
        assert_eq!(first.into_string(), "same");
    }
}
