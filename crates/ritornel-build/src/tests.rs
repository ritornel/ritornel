use super::*;

const PACKAGE: &str = "1.2.3-beta.1";
const RELEASE: &str = "v1.2.3-beta.1";
const SHA: &str = "ABCDEF0123456789abcdef0123456789abcdef01";

#[test]
fn normalizes_the_entire_sha_before_shortening() {
    for (input, expected) in [
        (SHA, "abcdef01"),
        ("1234ABCD", "1234abcd"),
        ("", "unknown"),
        ("1234567", "unknown"),
        ("abcdef01z", "unknown"),
        ("1234567é", "unknown"),
    ] {
        assert_eq!(Identity::new(input, None, false).commit, expected);
    }
}

#[test]
fn explicit_commit_never_consults_git() {
    for (tag, distance, dirty, expected) in [
        (Some(RELEASE), None, None, "1.2.3-beta.1"),
        (Some(RELEASE), Some(""), None, "1.2.3-beta.1"),
        (
            Some(RELEASE),
            Some(""),
            Some("1"),
            "1.2.3-beta.1+dev.0.abcdef01.dirty",
        ),
        (Some(RELEASE), Some("0"), Some("0"), "1.2.3-beta.1"),
        (
            Some(RELEASE),
            None,
            Some("1"),
            "1.2.3-beta.1+dev.0.abcdef01.dirty",
        ),
        (
            Some(RELEASE),
            Some("12"),
            None,
            "1.2.3-beta.1+dev.12.abcdef01",
        ),
        (
            Some(RELEASE),
            Some("12"),
            Some("1"),
            "1.2.3-beta.1+dev.12.abcdef01.dirty",
        ),
        (None, None, None, "1.2.3-beta.1+dev.abcdef01"),
        (None, Some("42"), None, "1.2.3-beta.1+dev.abcdef01"),
        (
            Some("v1.2.2"),
            Some("0"),
            Some("1"),
            "1.2.3-beta.1+dev.abcdef01.dirty",
        ),
        (Some(RELEASE), Some("-1"), None, "1.2.3-beta.1+dev.abcdef01"),
    ] {
        let build = identity(
            RELEASE,
            |key| {
                match key {
                    COMMIT => Some(SHA),
                    TAG => tag,
                    DISTANCE => distance,
                    DIRTY => dirty,
                    _ => unreachable!(),
                }
                .map(str::to_owned)
            },
            || unreachable!("explicit commit selected Git"),
        );
        assert_eq!(build.display(PACKAGE), expected);
    }
}

#[test]
fn empty_or_absent_commit_ignores_other_overrides() {
    for commit in [None, Some("")] {
        let build = identity(
            RELEASE,
            |key| {
                assert_eq!(key, COMMIT);
                commit.map(str::to_owned)
            },
            || Identity::new("01234567", None, false),
        );
        assert_eq!(build.display(PACKAGE), "1.2.3-beta.1+dev.01234567");
    }
}

#[test]
fn git_uses_only_the_package_tag_and_complete_history_for_distance() {
    for (tags, described, shallow, status, expected) in [
        (RELEASE, None, "true", "", "1.2.3-beta.1"),
        (
            RELEASE,
            None,
            "false",
            " M tracked",
            "1.2.3-beta.1+dev.0.abcdef01.dirty",
        ),
        (
            "",
            Some("v1.2.3-beta.1-3-gabc"),
            "false",
            "",
            "1.2.3-beta.1+dev.3.abcdef01",
        ),
        (
            "",
            Some("v1.2.3-beta.1-3-gabc"),
            "true",
            "",
            "1.2.3-beta.1+dev.abcdef01",
        ),
        ("", None, "false", "", "1.2.3-beta.1+dev.abcdef01"),
        (
            "v1.0",
            Some("v1.2.3-beta.1-3-gabc"),
            "false",
            "M  tracked",
            "1.2.3-beta.1+dev.abcdef01.dirty",
        ),
        (
            "v1.2.3-beta.1\nv2",
            None,
            "false",
            "",
            "1.2.3-beta.1+dev.abcdef01",
        ),
    ] {
        let build = git_identity(RELEASE, |args| {
            match args {
                ["rev-parse", "HEAD"] => Some(SHA),
                ["tag", "--points-at", "HEAD", "--list", "v*"] => Some(tags),
                [
                    "--no-optional-locks",
                    "status",
                    "--porcelain",
                    "--untracked-files=no",
                ] => Some(status),
                ["rev-parse", "--is-shallow-repository"] => Some(shallow),
                ["describe", "--tags", "--match", RELEASE, "--long"] => described,
                _ => unreachable!(),
            }
            .map(str::to_owned)
        });
        assert_eq!(build.display(PACKAGE), expected);
    }
}

#[test]
fn no_git_build_has_an_unknown_commit() {
    let build = git_identity(RELEASE, |_| None);
    assert_eq!(build.display(PACKAGE), "1.2.3-beta.1+dev.unknown");
}
