//! Typed identifiers and validated path segments.
//!
//! Every value that ends up as a URL path segment is validated here so the
//! guard lives in one auditable place rather than being re-derived at each
//! call site.

use core::fmt;
use core::str::FromStr;

use crate::error::Error;

macro_rules! id_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_newtype!(
    /// A Redmine issue id.
    IssueId
);
id_newtype!(
    /// A Redmine project id.
    ProjectId
);
id_newtype!(
    /// A Redmine user id.
    UserId
);
id_newtype!(
    /// A Redmine version (roadmap target) id.
    VersionId
);
id_newtype!(
    /// A Redmine project membership id.
    MembershipId
);
id_newtype!(
    /// A Redmine time entry id.
    TimeEntryId
);

/// A validated Redmine project identifier (the slug form, e.g. `my-project`),
/// safe to use as a single URL path segment.
///
/// Redmine identifiers match `^[a-z0-9][a-z0-9_-]{0,99}$`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectIdentifier(String);

impl ProjectIdentifier {
    /// The validated identifier as a `&str`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ProjectIdentifier {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let reject = |reason: String| Error::Config {
            reason: format!("invalid project identifier: {reason}"),
        };

        let mut chars = s.chars();
        let first = chars
            .next()
            .ok_or_else(|| reject("must not be empty".to_string()))?;
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            return Err(reject(format!(
                "must start with a lowercase letter or digit, got {first:?}"
            )));
        }
        let len = s.chars().count();
        if len > 100 {
            return Err(reject(format!("must be at most 100 characters, got {len}")));
        }
        let is_allowed =
            |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-';
        if !s.chars().all(is_allowed) {
            return Err(reject(format!(
                "must match ^[a-z0-9][a-z0-9_-]*$, got {s:?}"
            )));
        }
        Ok(Self(s.to_string()))
    }
}

impl fmt::Display for ProjectIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Either form Redmine accepts to identify a project in a URL path segment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProjectIdent {
    /// Numeric id.
    Id(ProjectId),
    /// Slug identifier.
    Identifier(ProjectIdentifier),
}

impl fmt::Display for ProjectIdent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(id) => write!(f, "{id}"),
            Self::Identifier(ident) => write!(f, "{ident}"),
        }
    }
}

impl serde::Serialize for ProjectIdent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

/// A validated Redmine wiki page title.
///
/// Unlike [`ProjectIdentifier`], wiki titles legitimately contain spaces and
/// non-ASCII text, so this is percent-encoding plus rejection of the
/// dangerous cases, not an allowlist: `/`, `\`, `..`, control characters
/// (including bidirectional-override characters used for spoofing), and NUL
/// are rejected; everything else is accepted and percent-encoded when used as
/// a path segment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WikiTitle(String);

/// Unicode bidirectional-control characters that can visually reorder text
/// (e.g. to disguise a path-traversal payload). `char::is_control` does not
/// cover these — they are format characters (category Cf), not controls
/// (category Cc).
const BIDI_CONTROLS: [char; 9] = [
    '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}',
    '\u{2069}',
];

impl WikiTitle {
    /// Validate and construct a wiki title.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if `s` is empty or contains `/`, `\`, `..`,
    /// a control character, or a bidirectional-override character.
    pub fn new(s: &str) -> Result<Self, Error> {
        let reject = |reason: &str| Error::Config {
            reason: format!("invalid wiki title: {reason}"),
        };
        if s.is_empty() {
            return Err(reject("must not be empty"));
        }
        if s.contains('/') || s.contains('\\') {
            return Err(reject("must not contain a path separator"));
        }
        if s.contains("..") {
            return Err(reject("must not contain '..'"));
        }
        if s.chars()
            .any(|c| c.is_control() || BIDI_CONTROLS.contains(&c))
        {
            return Err(reject(
                "must not contain control or bidi-override characters",
            ));
        }
        Ok(Self(s.to_string()))
    }

    /// The validated title as a `&str`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Percent-encode this title for use as a single URL path segment.
    ///
    /// # Panics
    ///
    /// Never in practice: the base URL is a static `http://` literal, which
    /// is always a valid base for `path_segments_mut`.
    #[must_use]
    pub fn encoded_segment(&self) -> String {
        // Route through `url::Url`'s own path-segment encoder rather than
        // adding a direct `percent-encoding` dependency: `path_segments_mut`
        // percent-encodes exactly the bytes a path segment requires.
        let mut url = url::Url::parse("http://placeholder.invalid")
            .unwrap_or_else(|_| unreachable!("static URL always parses"));
        {
            #[allow(clippy::unwrap_used, reason = "http scheme URLs are always base URLs")]
            let mut segments = url.path_segments_mut().unwrap();
            segments.pop_if_empty();
            segments.push(&self.0);
        }
        url.path().trim_start_matches('/').to_string()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn project_identifier_accepts_valid() {
        assert_eq!(
            ProjectIdentifier::from_str("my-project_2")
                .unwrap()
                .as_str(),
            "my-project_2"
        );
    }

    #[test]
    fn project_identifier_rejects_hostile_inputs() {
        let cases: &[(&str, &str)] = &[
            ("../", "path traversal"),
            ("%2e%2e", "percent-encoded traversal"),
            ("..%2f", "mixed traversal"),
            ("foo/bar", "embedded slash"),
            ("", "empty"),
            ("Foo", "uppercase"),
            ("a\0b", "NUL byte"),
            ("a\u{202E}b", "right-to-left override"),
        ];
        for (input, why) in cases {
            assert!(
                ProjectIdentifier::from_str(input).is_err(),
                "expected rejection ({why}) for {input:?}"
            );
        }
        let too_long = "a".repeat(101);
        assert!(
            ProjectIdentifier::from_str(&too_long).is_err(),
            "expected rejection for 101-char identifier"
        );
    }

    #[test]
    fn wiki_title_rejects_hostile_inputs() {
        let cases: &[(&str, &str)] = &[
            ("", "empty"),
            ("../secret", "path traversal"),
            ("a/b", "embedded slash"),
            ("a\\b", "embedded backslash"),
            ("a\0b", "NUL byte"),
            ("a\u{202E}b", "right-to-left override"),
            ("a\nb", "control character"),
        ];
        for (input, why) in cases {
            assert!(
                WikiTitle::new(input).is_err(),
                "expected rejection ({why}) for {input:?}"
            );
        }
    }

    #[test]
    fn wiki_title_accepts_spaces_and_non_ascii_and_encodes_them() {
        let title = WikiTitle::new("My Wiki Page héllo").expect("should be valid");
        let encoded = title.encoded_segment();
        assert!(
            !encoded.contains(' '),
            "spaces must be percent-encoded: {encoded}"
        );
        assert!(
            encoded.contains("%20") || encoded.contains('+'),
            "got {encoded}"
        );
    }
}
