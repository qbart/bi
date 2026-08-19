//! The other file: the test beside the implementation, the header beside the
//! source.
//!
//! See `docs/specs/alternate.md`.

/// What `*` matched in `pattern`, if `path` matches it at all.
///
/// One `*` per pattern, and it matches everything — separators included — so
/// `*_test.go` reads `internal/thing_test.go` as `internal/thing`. A pattern
/// with no `*` matches itself exactly and captures nothing.
pub fn capture(pattern: &str, path: &str) -> Option<String> {
    let Some((prefix, suffix)) = pattern.split_once('*') else {
        return (pattern == path).then(String::new);
    };
    let rest = path.strip_prefix(prefix)?;
    let middle = rest.strip_suffix(suffix)?;
    // Greedy on purpose: `*.go` against `a.b.go` is `a.b`, not `a`. The suffix
    // is matched from the end, which is what makes it so.
    Some(middle.to_string())
}

/// `target` with its `*` replaced by what the pattern captured.
pub fn expand(target: &str, captured: &str) -> String {
    target.replacen('*', captured, 1)
}

/// Every path `rules` offers for `path`, in the order they were written.
///
/// The first rule whose pattern matches decides, which is why the order of the
/// rules is the config's and not a map's: `*_test.go` has to be tried before
/// `*.go`, or a test file is its own alternate forever.
pub fn candidates(rules: &[(String, Vec<String>)], path: &str) -> Vec<String> {
    for (pattern, targets) in rules {
        let Some(captured) = capture(pattern, path) else { continue };
        return targets.iter().map(|target| expand(target, &captured)).collect();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Vec<(String, Vec<String>)> {
        vec![
            ("*_test.go".into(), vec!["*.go".into()]),
            ("*.go".into(), vec!["*_test.go".into()]),
            ("*.cpp".into(), vec!["*.hpp".into(), "*.h".into()]),
        ]
    }

    #[test]
    fn a_star_captures_across_directories() {
        assert_eq!(capture("*.go", "internal/thing.go").as_deref(), Some("internal/thing"));
        assert_eq!(capture("*.go", "thing.rs"), None);
        assert_eq!(capture("*.go", "a.b.go").as_deref(), Some("a.b"), "greedy from the end");
    }

    #[test]
    fn the_first_matching_rule_decides_which_is_why_order_is_kept() {
        // `*_test.go` is written first, so a test file goes to the
        // implementation rather than matching `*.go` and pointing at itself.
        assert_eq!(candidates(&rules(), "internal/thing_test.go"), ["internal/thing.go"]);
        assert_eq!(candidates(&rules(), "internal/thing.go"), ["internal/thing_test.go"]);
    }

    #[test]
    fn a_rule_may_offer_several_and_they_keep_their_order() {
        assert_eq!(candidates(&rules(), "src/main.cpp"), ["src/main.hpp", "src/main.h"]);
    }

    #[test]
    fn a_path_nothing_matches_offers_nothing() {
        assert!(candidates(&rules(), "README.md").is_empty());
    }
}
