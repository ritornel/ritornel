//! Compile-time version strings shared by the Ritornel applications.

use std::{env, path::PathBuf, process::Command};

const COMMIT: &str = "RITORNEL_BUILD_COMMIT";
const TAG: &str = "RITORNEL_BUILD_TAG";
const DISTANCE: &str = "RITORNEL_BUILD_DISTANCE";
const DIRTY: &str = "RITORNEL_BUILD_DIRTY";

struct Identity {
    commit: String,
    // Some(0) is the exact package tag. None means there is no usable tag.
    distance: Option<u64>,
    dirty: bool,
}

impl Identity {
    fn new(sha: &str, distance: Option<u64>, dirty: bool) -> Self {
        let commit = sha
            .get(..8)
            .filter(|_| sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .map_or_else(|| "unknown".to_owned(), str::to_ascii_lowercase);
        Self {
            commit,
            distance,
            dirty,
        }
    }

    fn display(&self, package: &str) -> String {
        if self.distance == Some(0) && !self.dirty {
            return package.to_owned();
        }
        let distance = self.distance.map_or_else(String::new, |n| format!(".{n}"));
        let suffix = if self.dirty { ".dirty" } else { "" };
        format!("{package}+dev{distance}.{}{suffix}", self.commit)
    }
}

// An explicit commit selects all inputs, including the absence of a tag.
fn identity(
    release_tag: &str,
    read: impl Fn(&str) -> Option<String>,
    from_git: impl FnOnce() -> Identity,
) -> Identity {
    let Some(commit) = read(COMMIT).filter(|s| !s.is_empty()) else {
        return from_git();
    };
    let distance = if read(TAG).as_deref() == Some(release_tag) {
        match read(DISTANCE) {
            None => Some(0),
            Some(n) => n.parse().ok(),
        }
    } else {
        None
    };
    Identity::new(&commit, distance, read(DIRTY).as_deref() == Some("1"))
}

fn described_distance(description: &str, tag: &str) -> Option<u64> {
    let rest = description.strip_prefix(tag)?.strip_prefix('-')?;
    let (count, _) = rest.rsplit_once("-g")?;
    count.parse().ok()
}

fn git_identity(tag: &str, git: impl Fn(&[&str]) -> Option<String>) -> Identity {
    let probes = (
        git(&["rev-parse", "HEAD"]),
        git(&["tag", "--points-at", "HEAD", "--list", "v*"]),
        git(&["status", "--porcelain", "--untracked-files=no"]),
    );
    let (Some(sha), Some(tags), Some(status)) = probes else {
        return Identity::new("", None, false);
    };
    let distance = if tags.lines().any(|exact| exact != tag) {
        None
    } else if tags.lines().any(|exact| exact == tag) {
        Some(0)
    } else if git(&["rev-parse", "--is-shallow-repository"]).as_deref() == Some("false") {
        git(&["describe", "--tags", "--match", tag, "--long"])
            .and_then(|description| described_distance(&description, tag))
    } else {
        None
    };
    Identity::new(&sha, distance, !status.is_empty())
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .trim_end_matches(['\r', '\n'])
            .to_owned()
    })
}

#[allow(clippy::print_stdout)] // Cargo reads build-script directives from stdout.
fn watch_git() {
    let head_log = git(&["rev-parse", "--git-path", "logs/HEAD"]).map(PathBuf::from);
    let common = git(&["rev-parse", "--git-common-dir"]).map(PathBuf::from);
    let paths = [
        head_log,
        common.as_ref().map(|dir| dir.join("packed-refs")),
        common.as_ref().map(|dir| dir.join("refs/tags")),
    ];
    for path in paths.into_iter().flatten().filter(|path| path.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// Emit Cargo version variables and their rebuild inputs from an application's build script.
#[allow(clippy::print_stdout)] // Cargo reads build-script directives from stdout.
pub fn emit() -> Result<(), env::VarError> {
    let package = env::var("CARGO_PKG_VERSION")?;
    let tag = format!("v{package}");
    for input in [COMMIT, TAG, DISTANCE, DIRTY] {
        println!("cargo:rerun-if-env-changed={input}");
    }
    let build = identity(
        &tag,
        |key| env::var_os(key).map(|s| s.to_string_lossy().into_owned()),
        || {
            watch_git();
            git_identity(&tag, git)
        },
    );
    println!(
        "cargo:rustc-env=RITORNEL_VERSION_DISPLAY={}",
        build.display(&package)
    );
    println!("cargo:rustc-env=RITORNEL_VERSION_COMMIT={}", build.commit);
    println!(
        "cargo:rustc-env=RITORNEL_VERSION_DIRTY={}",
        u8::from(build.dirty)
    );
    Ok(())
}

#[cfg(test)]
mod tests;
