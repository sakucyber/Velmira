use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use reqwest::blocking::{Client, Response};
use reqwest::header::{ETAG, IF_NONE_MATCH, LOCATION, USER_AGENT};
use reqwest::{StatusCode, Url};
pub use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

mod target;
pub use target::{Arch, PackageFormat, Platform, Target};

const API_RELEASES: &str = "https://api.github.com/repos/sakucyber/Velmira/releases";
pub const RELEASES_URL: &str = "https://github.com/sakucyber/Velmira/releases";
const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum Error {
    #[error("GitHub request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("GitHub returned HTTP {0}")]
    Status(StatusCode),
    #[error("GitHub returned an unexpected release redirect")]
    InvalidRedirect,
    #[error("release metadata is larger than {MAX_METADATA_BYTES} bytes")]
    OversizedMetadata,
    #[error("could not decode release metadata: {0}")]
    InvalidMetadata(#[from] serde_json::Error),
    #[error("could not read data: {0}")]
    Io(#[from] io::Error),
    #[error("release provides no SHA-256 checksum for this asset")]
    MissingChecksum,
    #[error("invalid SHA-256 checksum")]
    InvalidChecksum,
    #[error("download does not match its SHA-256 checksum")]
    ChecksumMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Beta,
}

#[derive(Debug)]
pub enum CheckOutcome {
    NotModified,
    Checked {
        release: Option<Release>,
        etag: Option<String>,
    },
}

#[derive(Debug)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    pub notes: String,
    pub assets: Vec<Asset>,
    pub published_at: Option<String>,
    pub url: String,
}

#[derive(Debug)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    sha256: Option<String>,
}

impl Asset {
    pub fn verify(&self, path: &Path) -> Result<(), Error> {
        verify_file(path, self.sha256.as_deref().ok_or(Error::MissingChecksum)?)
    }

    pub fn sha256(&self) -> Option<&str> {
        self.sha256.as_deref()
    }
}

pub struct GitHubClient(Client);

impl GitHubClient {
    pub fn new() -> Result<Self, Error> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self(client))
    }

    /// Check on a background thread. Reuse an ETag only for the same channel.
    pub fn check(
        &self,
        current: &Version,
        channel: Channel,
        etag: Option<&str>,
    ) -> Result<CheckOutcome, Error> {
        let endpoint = match channel {
            Channel::Stable => format!("{API_RELEASES}/latest"),
            Channel::Beta => format!("{API_RELEASES}?per_page=30"),
        };
        let user_agent = format!(
            "Velmira/{current} ({}/{})",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        let mut request = self
            .0
            .get(endpoint)
            .header(USER_AGENT, &user_agent)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(etag) = etag {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = request.send()?;
        let etag = header_value(&response, ETAG).map(str::to_owned);
        let release = match response.status() {
            StatusCode::NOT_MODIFIED => return Ok(CheckOutcome::NotModified),
            StatusCode::NOT_FOUND if channel == Channel::Stable => None,
            StatusCode::OK => {
                let body = read_metadata(response)?;
                let releases = match channel {
                    Channel::Stable => vec![serde_json::from_slice(&body)?],
                    Channel::Beta => serde_json::from_slice(&body)?,
                };
                select_release(releases, current, channel)
            }
            StatusCode::TOO_MANY_REQUESTS | StatusCode::FORBIDDEN
                if channel == Channel::Stable
                    && (response.status() == StatusCode::TOO_MANY_REQUESTS
                        || header_value(&response, "x-ratelimit-remaining") == Some("0")) =>
            {
                return Ok(CheckOutcome::Checked {
                    release: self.check_latest_redirect(current, &user_agent)?,
                    etag: None,
                });
            }
            status => return Err(Error::Status(status)),
        };
        Ok(CheckOutcome::Checked { release, etag })
    }

    fn check_latest_redirect(
        &self,
        current: &Version,
        user_agent: &str,
    ) -> Result<Option<Release>, Error> {
        let response = self
            .0
            .get(format!("{RELEASES_URL}/latest"))
            .header(USER_AGENT, user_agent)
            .send()?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_redirection() {
            return Err(Error::Status(response.status()));
        }
        let location = header_value(&response, LOCATION).ok_or(Error::InvalidRedirect)?;
        redirect_release(location, current)
    }
}

fn header_value(response: &Response, name: impl reqwest::header::AsHeaderName) -> Option<&str> {
    response.headers().get(name)?.to_str().ok()
}

fn read_metadata(response: Response) -> Result<Vec<u8>, Error> {
    let mut body = Vec::new();
    response
        .take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut body)?;
    if body.len() as u64 > MAX_METADATA_BYTES {
        return Err(Error::OversizedMetadata);
    }
    Ok(body)
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    body: Option<String>,
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    size: u64,
    state: String,
    digest: Option<String>,
}

impl GitHubRelease {
    fn into_release(self, version: Version) -> Release {
        let notes = self.body.unwrap_or_default();
        let assets = self
            .assets
            .into_iter()
            .filter(|asset| asset.state == "uploaded" && safe_asset_name(&asset.name))
            .map(|asset| {
                let url = format!("{RELEASES_URL}/download/{}/{}", self.tag_name, asset.name);
                let sha256 = match asset.digest {
                    Some(digest) => Some(digest.strip_prefix("sha256:").unwrap_or("").to_owned()),
                    None => checksum_from_notes(&notes, &asset.name),
                };
                Asset {
                    name: asset.name,
                    url,
                    size: asset.size,
                    sha256,
                }
            })
            .collect();
        Release {
            url: format!("{RELEASES_URL}/tag/{}", self.tag_name),
            tag: self.tag_name,
            version,
            notes,
            assets,
            published_at: self.published_at,
        }
    }
}

fn safe_asset_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name.len() <= 255
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._+-".contains(&c))
}

fn newer_version(tag: &str, current: &Version) -> Option<Version> {
    let version = Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()?;
    version.cmp_precedence(current).is_gt().then_some(version)
}

fn select_release(
    releases: Vec<GitHubRelease>,
    current: &Version,
    channel: Channel,
) -> Option<Release> {
    releases
        .into_iter()
        .filter(|release| !release.draft)
        .filter_map(|release| {
            let version = newer_version(&release.tag_name, current)?;
            let pre = version.pre.as_str();
            let stable = !release.prerelease && pre.is_empty();
            let beta = pre == "beta" || pre.starts_with("beta.");
            (stable || channel == Channel::Beta && beta).then_some((version, release))
        })
        .max_by(|(a, _), (b, _)| a.cmp_precedence(b))
        .map(|(version, release)| release.into_release(version))
}

fn redirect_release(location: &str, current: &Version) -> Result<Option<Release>, Error> {
    let latest_url = format!("{RELEASES_URL}/latest");
    let url = Url::parse(&latest_url)
        .and_then(|base| base.join(location))
        .map_err(|_| Error::InvalidRedirect)?;
    if url.as_str() == RELEASES_URL {
        return Ok(None);
    }
    let prefix = format!("{RELEASES_URL}/tag/");
    let tag = url
        .as_str()
        .strip_prefix(&prefix)
        .filter(|tag| Version::parse(tag.strip_prefix('v').unwrap_or(tag)).is_ok())
        .ok_or(Error::InvalidRedirect)?;
    Ok(newer_version(tag, current)
        .filter(|version| version.pre.is_empty())
        .map(|version| Release {
            version,
            tag: tag.to_owned(),
            notes: String::new(),
            assets: Vec::new(),
            published_at: None,
            url: url.to_string(),
        }))
}

fn checksum_from_notes(notes: &str, name: &str) -> Option<String> {
    let mut matches = notes.lines().filter_map(|line| {
        let (checksum, filename) = line.trim().split_once(char::is_whitespace)?;
        (filename.trim_start().trim_start_matches('*') == name && checksum.len() == 64)
            .then_some(checksum)
    });
    let first = matches.next()?;
    if matches.any(|checksum| !checksum.eq_ignore_ascii_case(first)) {
        return Some(String::new());
    }
    Some(first.to_owned())
}

/// Verify a file against a 64-character, hexadecimal SHA-256 checksum.
pub fn verify_file(path: &Path, sha256: &str) -> Result<(), Error> {
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::InvalidChecksum);
    }
    let mut digest = Sha256::new();
    io::copy(&mut File::open(path)?, &mut digest)?;
    if !format!("{:x}", digest.finalize()).eq_ignore_ascii_case(sha256) {
        return Err(Error::ChecksumMismatch);
    }
    Ok(())
}
