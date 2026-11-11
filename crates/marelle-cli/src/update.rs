// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! The startup update check.
//!
//! Decisions that matter:
//!
//! * The check belongs to an interactive session only. It runs in a
//!   background thread with its own short-lived runtime, so the session
//!   starts at once and the notice, when it arrives, is simply appended.
//!   A one-shot question or a pipe run is never delayed by it.
//! * A failure is silent. The check exists to inform, not to nag: no
//!   network, a rate limit, an empty repository or a malformed tag all end
//!   quietly and never surface as an error.
//! * Only a strictly newer version is reported, compared by the rules of
//!   semantic versioning, so a tag of the running version never triggers a
//!   notice and a prerelease never hides a release behind it.
//! * Sources are `github.com` and `gitlab.com`. A release is asked for
//!   first; a repository that only tags its commits falls back to its
//!   newest tag. Both sources are consulted and the newest wins.

use std::cmp::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use marelle_config::Config;
use marelle_providers::{NetworkSettings, plain_client};

use crate::VERSION;

const GITHUB_API: &str = "https://api.github.com";
const GITLAB_API: &str = "https://gitlab.com/api/v4";
const DEFAULT_TIMEOUT_SECS: u64 = 3;

/// What the check found, ready to be shown.
pub(crate) struct UpdateNotice {
    /// The newest version seen on a source, tag prefix stripped.
    pub(crate) latest: String,
    /// The version of this binary.
    pub(crate) current: String,
    /// Where the release or the tags live.
    pub(crate) url: String,
}

/// Where and how long to ask.
#[derive(Clone)]
pub(crate) struct UpdateSettings {
    github_repo: Option<String>,
    gitlab_repo: Option<String>,
    timeout: Duration,
}

impl UpdateSettings {
    /// Reads `update.*` from the built configuration.
    pub(crate) fn from_config(config: &Config) -> Self {
        let repo = |key: &str| {
            config
                .get(key)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        Self {
            github_repo: repo("update.github_repo"),
            gitlab_repo: repo("update.gitlab_repo"),
            timeout: Duration::from_secs(
                config
                    .get_u64("update.timeout_secs")
                    .ok()
                    .flatten()
                    .filter(|seconds| *seconds > 0)
                    .unwrap_or(DEFAULT_TIMEOUT_SECS),
            ),
        }
    }
}

/// Starts the check in the background and returns where the notice lands.
///
/// `None` means the check is off, has no sources, or its client could not be
/// built; in every case the session simply continues without it.
pub(crate) fn spawn(
    network: &NetworkSettings,
    settings: &UpdateSettings,
) -> Option<Receiver<UpdateNotice>> {
    if settings.github_repo.is_none() && settings.gitlab_repo.is_none() {
        return None;
    }
    // Built on the caller's thread: the TLS setup reads files, and the
    // background thread should only talk to the network.
    let client = plain_client(network, settings.timeout).ok()?;
    let (sender, receiver) = mpsc::channel();
    let current = VERSION.to_owned();
    let settings = settings.clone();
    let started = std::thread::Builder::new()
        .name("update-check".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(_) => return,
            };
            if let Some(notice) = runtime.block_on(check(&client, &settings, &current)) {
                let _ = sender.send(notice);
            }
        });
    started.ok()?;
    Some(receiver)
}

/// Asks every source and keeps the newest version that beats `current`.
async fn check(
    client: &reqwest::Client,
    settings: &UpdateSettings,
    current: &str,
) -> Option<UpdateNotice> {
    let github = match &settings.github_repo {
        Some(repo) => fetch_github(client, repo).await,
        None => None,
    };
    let gitlab = match &settings.gitlab_repo {
        Some(repo) => fetch_gitlab(client, repo).await,
        None => None,
    };
    let best = [github, gitlab]
        .into_iter()
        .flatten()
        .filter(|found| newer(&found.version, current))
        .reduce(
            |best, found| match compare_versions(&found.version, &best.version) {
                Some(Ordering::Greater) => found,
                _ => best,
            },
        )?;
    Some(UpdateNotice {
        latest: best.version,
        current: current.to_owned(),
        url: best.url,
    })
}

/// One finding, before it competes with the others.
struct Found {
    version: String,
    url: String,
}

/// Asks GitHub for the newest release of `owner/repo`.
async fn fetch_github(client: &reqwest::Client, repo: &str) -> Option<Found> {
    if !valid_repo(repo) {
        return None;
    }
    let url = format!("{GITHUB_API}/repos/{repo}/releases/latest");
    let response = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .ok()?;
    if response.status().as_u16() == 200 {
        let text = response.text().await.ok()?;
        return github_release_json(&text, repo);
    }
    // A repository may publish no release and still tag its commits.
    if response.status().as_u16() == 404 {
        let url = format!("{GITHUB_API}/repos/{repo}/tags");
        let response = client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .ok()?;
        if response.status().as_u16() == 200 {
            let text = response.text().await.ok()?;
            return github_tags_json(&text, repo);
        }
    }
    None
}

/// Asks GitLab for the newest release of `owner/repo`.
async fn fetch_gitlab(client: &reqwest::Client, repo: &str) -> Option<Found> {
    if !valid_repo(repo) {
        return None;
    }
    let project = repo.replace('/', "%2F");
    let url = format!("{GITLAB_API}/projects/{project}/releases");
    let response = client.get(&url).send().await.ok()?;
    if response.status().as_u16() == 200 {
        let text = response.text().await.ok()?;
        if let Some(found) = gitlab_releases_json(&text, repo) {
            return Some(found);
        }
    }
    // Releases may be absent while tags exist.
    let url = format!("{GITLAB_API}/projects/{project}/repository/tags");
    let response = client.get(&url).send().await.ok()?;
    if response.status().as_u16() == 200 {
        let text = response.text().await.ok()?;
        return gitlab_tags_json(&text, repo);
    }
    None
}

/// Reads `{"tag_name": ..., "html_url": ...}` from the GitHub releases API.
fn github_release_json(text: &str, repo: &str) -> Option<Found> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let version = json.get("tag_name")?.as_str()?.trim().to_owned();
    if version.is_empty() {
        return None;
    }
    let url = json
        .get("html_url")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map_or_else(
            || format!("https://github.com/{repo}/releases"),
            str::to_owned,
        );
    Some(Found { version, url })
}

/// Reads the first tag of the GitHub tags API.
fn github_tags_json(text: &str, repo: &str) -> Option<Found> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let version = json
        .as_array()?
        .first()?
        .get("name")?
        .as_str()?
        .trim()
        .to_owned();
    if version.is_empty() {
        return None;
    }
    Some(Found {
        version,
        url: format!("https://github.com/{repo}/releases"),
    })
}

/// Reads the first element of the GitLab releases API.
fn gitlab_releases_json(text: &str, repo: &str) -> Option<Found> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let first = json.as_array()?.first()?;
    let version = first.get("tag_name")?.as_str()?.trim().to_owned();
    if version.is_empty() {
        return None;
    }
    let url = format!("https://gitlab.com/{repo}/-/releases/{version}");
    Some(Found { version, url })
}

/// Reads the first tag of the GitLab tags API.
fn gitlab_tags_json(text: &str, repo: &str) -> Option<Found> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let version = json
        .as_array()?
        .first()?
        .get("name")?
        .as_str()?
        .trim()
        .to_owned();
    if version.is_empty() {
        return None;
    }
    Some(Found {
        version,
        url: format!("https://gitlab.com/{repo}/-/tags"),
    })
}

/// Whether `repo` may go into an API URL: exactly `owner/repo` of URL-safe
/// words. Everything else is refused, so a setting cannot turn the check
/// into a request to a host nobody meant.
fn valid_repo(repo: &str) -> bool {
    let (owner, name) = match repo.split_once('/') {
        Some((owner, name)) => (owner, name),
        None => return false,
    };
    if owner.is_empty() || name.is_empty() {
        return false;
    }
    let word = |text: &str| {
        !text.starts_with('.')
            && !text.ends_with('.')
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    };
    word(owner) && word(name)
}

/// Whether `latest` is strictly newer than `current`.
fn newer(latest: &str, current: &str) -> bool {
    compare_versions(latest, current) == Some(Ordering::Greater)
}

/// Compares two versions by the rules of semantic versioning.
///
/// A value that is not a version at all never compares: the check stays
/// silent rather than nagging about a tag it cannot read.
fn compare_versions(latest: &str, current: &str) -> Option<Ordering> {
    let (latest_core, latest_pre) = split_version(latest)?;
    let (current_core, current_pre) = split_version(current)?;
    for (latest, current) in latest_core.iter().zip(current_core.iter()) {
        let latest: u64 = latest.parse().ok()?;
        let current: u64 = current.parse().ok()?;
        match latest.cmp(&current) {
            Ordering::Equal => {}
            other => return Some(other),
        }
    }
    match latest_core.len().cmp(&current_core.len()) {
        Ordering::Equal => {}
        other => return Some(other),
    }
    Some(compare_prerelease(latest_pre, current_pre))
}

/// Splits `v1.2.3-alpha.1` into core numbers and the prerelease part.
fn split_version(version: &str) -> Option<(Vec<&str>, Option<&str>)> {
    let version = version.trim();
    let version = version
        .strip_prefix('v')
        .or_else(|| version.strip_prefix('V'))
        .unwrap_or(version);
    let version = version.split('+').next()?;
    if version.is_empty() {
        return None;
    }
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) if !pre.is_empty() => (core, Some(pre)),
        _ => (version, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() < 3 {
        return None;
    }
    if parts
        .iter()
        .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    Some((parts, pre))
}

/// A release outranks its own prerelease; prereleases compare identifier by
/// identifier, where numbers come before words.
fn compare_prerelease(latest: Option<&str>, current: Option<&str>) -> Ordering {
    match (latest, current) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(latest), Some(current)) => {
            let mut latest = latest.split('.');
            let mut current = current.split('.');
            loop {
                match (latest.next(), current.next()) {
                    (None, None) => return Ordering::Equal,
                    (None, Some(_)) => return Ordering::Less,
                    (Some(_), None) => return Ordering::Greater,
                    (Some(latest), Some(current)) => {
                        let order = match (latest.parse::<u64>(), current.parse::<u64>()) {
                            (Ok(latest), Ok(current)) => latest.cmp(&current),
                            (Ok(_), Err(_)) => Ordering::Less,
                            (Err(_), Ok(_)) => Ordering::Greater,
                            (Err(_), Err(_)) => latest.cmp(current),
                        };
                        if order != Ordering::Equal {
                            return order;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_json_gives_the_tag_and_its_page() {
        let text =
            r#"{"tag_name": "v0.2.0", "html_url": "https://github.com/o/r/releases/tag/v0.2.0"}"#;
        let found = github_release_json(text, "o/r").expect("a release");
        assert_eq!(found.version, "v0.2.0");
        assert_eq!(found.url, "https://github.com/o/r/releases/tag/v0.2.0");
    }

    #[test]
    fn release_json_without_a_tag_is_none() {
        assert!(github_release_json(r#"{"message": "Not Found"}"#, "o/r").is_none());
        assert!(github_release_json("not json", "o/r").is_none());
    }

    #[test]
    fn tags_json_takes_the_first_name() {
        let text = r#"[{"name": "v0.1.0"}, {"name": "v0.0.9"}]"#;
        let found = github_tags_json(text, "o/r").expect("a tag");
        assert_eq!(found.version, "v0.1.0");
        assert_eq!(found.url, "https://github.com/o/r/releases");
        assert!(github_tags_json("[]", "o/r").is_none());
    }

    #[test]
    fn gitlab_json_takes_the_first_release() {
        let text = r#"[{"tag_name": "v0.3.0"}, {"tag_name": "v0.2.0"}]"#;
        let found = gitlab_releases_json(text, "o/r").expect("a release");
        assert_eq!(found.version, "v0.3.0");
        assert_eq!(found.url, "https://gitlab.com/o/r/-/releases/v0.3.0");
        assert!(gitlab_releases_json("[]", "o/r").is_none());
    }

    #[test]
    fn repo_names_are_tight() {
        assert!(valid_repo("cornier-en-branche/marelle"));
        assert!(valid_repo("Liada-Siverskyi/rebis"));
        assert!(!valid_repo("marelle"));
        assert!(!valid_repo("/marelle"));
        assert!(!valid_repo("marelle/"));
        assert!(!valid_repo("o/r extra"));
        assert!(!valid_repo("../etc"));
        assert!(!valid_repo("o/repo\n"));
    }

    #[test]
    fn strictly_newer_versions_are_reported() {
        assert!(newer("0.2.0", "0.1.0-alpha.1"));
        assert!(newer("v0.1.1", "0.1.0"));
        assert!(newer("0.1.0", "0.1.0-alpha.1"));
        assert!(newer("0.1.0-alpha.2", "0.1.0-alpha.1"));
        assert!(newer("0.1.0-beta", "0.1.0-alpha.15"));
        assert!(newer("0.1.0-alpha.1.1", "0.1.0-alpha.1"));
        assert!(!newer("0.1.0-alpha.1", "0.1.0-alpha.1"));
        assert!(!newer("0.1.0-alpha.1", "0.1.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn unreadable_versions_stay_silent() {
        assert!(!newer("marelle", "0.1.0"));
        assert!(!newer("1.2", "0.1.0"));
        assert!(!newer("", "0.1.0"));
        assert!(!newer("0.1.0-", "0.1.0"));
    }
}
