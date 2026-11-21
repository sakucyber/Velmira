use crate::{check_digest, header_value, safe_asset_name, validate_sha256, Asset, Error, Version};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT_ENCODING, LOCATION, USER_AGENT};
use reqwest::{redirect, StatusCode, Url};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;
use tempfile::NamedTempFile;

const MAX_ASSET_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_REDIRECTS: usize = 4;
const ASSET_PATH: &str = "/sakucyber/Velmira/releases/download/";

impl Asset {
    /// Whether this asset contains usable metadata for a verified download.
    pub fn can_download(&self) -> bool {
        self.download_metadata().is_ok()
    }

    /// Download on a worker thread into an existing directory. Reports transferred and total
    /// bytes. The destination appears only after verification and an existing file is preserved.
    pub fn download(
        &self,
        destination: &Path,
        progress: impl FnMut(u64, u64),
    ) -> Result<(), Error> {
        let (sha256, mut url) = self.download_metadata()?;
        let mut staged = stage(destination)?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .redirect(redirect::Policy::none())
            .build()?;

        let mut redirects = 0;
        let response = loop {
            let response = client
                .get(url.clone())
                .header(USER_AGENT, "Velmira Native")
                .header(ACCEPT_ENCODING, "identity")
                .send()
                .map_err(reqwest::Error::without_url)?;
            match response.status() {
                StatusCode::OK => break response,
                StatusCode::MOVED_PERMANENTLY
                | StatusCode::FOUND
                | StatusCode::SEE_OTHER
                | StatusCode::TEMPORARY_REDIRECT
                | StatusCode::PERMANENT_REDIRECT => {
                    let location =
                        header_value(&response, LOCATION).ok_or(Error::InvalidRedirect)?;
                    redirects += 1;
                    if redirects > MAX_REDIRECTS {
                        return Err(Error::InvalidRedirect);
                    }
                    url = redirect_url(&url, location)?;
                }
                status => return Err(Error::Status(status)),
            }
        };
        if let Some(actual) = response.content_length() {
            if actual != self.size {
                return Err(Error::SizeMismatch {
                    expected: self.size,
                    actual,
                });
            }
        }
        transfer(response, &mut staged, self.size, sha256, progress)?;
        publish(staged, destination)
    }

    fn download_metadata(&self) -> Result<(&str, Url), Error> {
        let sha256 = self.sha256().ok_or(Error::MissingChecksum)?;
        validate_sha256(sha256)?;
        if self.size == 0 || self.size > MAX_ASSET_BYTES {
            return Err(Error::InvalidAssetSize);
        }
        let url = Url::parse(&self.url).map_err(|_| Error::InvalidAssetUrl)?;
        let name = repository_asset_name(&url).ok_or(Error::InvalidAssetUrl)?;
        if name != self.name {
            return Err(Error::InvalidAssetUrl);
        }
        Ok((sha256, url))
    }
}

fn clean_https(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none()
}

fn repository_asset_name(url: &Url) -> Option<&str> {
    if !clean_https(url) || url.host_str() != Some("github.com") || url.query().is_some() {
        return None;
    }
    let (tag, name) = url.path().strip_prefix(ASSET_PATH)?.split_once('/')?;
    Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()?;
    safe_asset_name(name).then_some(name)
}

fn redirect_url(base: &Url, location: &str) -> Result<Url, Error> {
    let url = base.join(location).map_err(|_| Error::InvalidRedirect)?;
    if !clean_https(&url) {
        return Err(Error::InvalidRedirect);
    }
    match url.host_str() {
        Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com")
            if url.path().starts_with("/github-production-release-asset/")
                || url.path().starts_with("/github-production-release-asset-") =>
        {
            Ok(url)
        }
        _ => Err(Error::InvalidRedirect),
    }
}

fn stage(destination: &Path) -> Result<NamedTempFile, Error> {
    if destination.file_name().is_none() {
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "missing destination filename").into(),
        );
    }
    match destination.symlink_metadata() {
        Ok(_) => {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "destination exists").into())
        }
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
        Err(_) => {}
    }
    let directory = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(tempfile::Builder::new()
        .prefix(".velmira-update-")
        .suffix(".part")
        .tempfile_in(directory)?)
}

fn transfer(
    mut source: impl Read,
    destination: &mut NamedTempFile,
    expected: u64,
    sha256: &str,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), Error> {
    let mut digest = Sha256::new();
    let mut buffer = [0; 32 * 1024];
    let mut total = 0;
    progress(total, expected);
    loop {
        let read = match source.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > expected {
            return Err(Error::SizeMismatch {
                expected,
                actual: total,
            });
        }
        let chunk = &buffer[..read];
        digest.update(chunk);
        destination.write_all(chunk)?;
        progress(total, expected);
    }
    if total != expected {
        return Err(Error::SizeMismatch {
            expected,
            actual: total,
        });
    }
    check_digest(digest, sha256)?;
    destination.as_file().sync_all()?;
    Ok(())
}

fn publish(staged: NamedTempFile, destination: &Path) -> Result<(), Error> {
    staged
        .persist_noclobber(destination)
        .map_err(|error| error.error)?;
    Ok(())
}
