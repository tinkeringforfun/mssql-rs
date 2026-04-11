//! OCI registry client for pulling images from MCR (or any Docker v2 registry).

use crate::error::{Error, Result};
use reqwest::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{debug, info};

/// An OCI / Docker v2 image manifest.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageManifest {
    pub schema_version: u32,
    pub config: Descriptor,
    pub layers: Vec<Descriptor>,
}

/// A content-addressable descriptor (used for config + layers).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    pub media_type: String,
    pub size: u64,
    pub digest: String,
}

/// Token response from the anonymous auth flow.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

/// OCI registry client that can pull manifests and blobs.
pub struct RegistryClient {
    client: Client,
    registry: String,
    repository: String,
}

impl RegistryClient {
    /// Create a new registry client for the given image.
    ///
    /// `registry` is e.g. `"mcr.microsoft.com"` and `repository` is e.g.
    /// `"mssql/server"`.
    pub fn new(registry: &str, repository: &str) -> Self {
        Self {
            client: Client::new(),
            registry: registry.to_string(),
            repository: repository.to_string(),
        }
    }

    /// Obtain an anonymous bearer token from the registry's token endpoint.
    async fn get_token(&self) -> Result<String> {
        // MCR uses a Www-Authenticate challenge. We do the happy-path shortcut:
        // request the manifest without auth, parse the Www-Authenticate header,
        // then fetch the token.
        let url = format!(
            "https://{}/v2/{}/manifests/latest",
            self.registry, self.repository
        );
        let resp = self.client.get(&url).send().await?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            if let Some(www_auth) = resp.headers().get("www-authenticate") {
                let www_auth = www_auth.to_str().unwrap_or_default();
                let (realm, service, scope) = parse_www_authenticate(www_auth);
                let token_url = format!(
                    "{}?service={}&scope={}",
                    realm, service, scope
                );
                debug!(token_url, "Fetching anonymous token");
                let token_resp: TokenResponse =
                    self.client.get(&token_url).send().await?.json().await?;
                return Ok(token_resp.access_token);
            }
        }

        // If the registry doesn't require auth, return empty token.
        Ok(String::new())
    }

    /// Pull the image manifest for the given tag.
    pub async fn pull_manifest(&self, tag: &str) -> Result<ImageManifest> {
        let token = self.get_token().await?;
        let url = format!(
            "https://{}/v2/{}/manifests/{}",
            self.registry, self.repository, tag
        );
        info!(url, "Pulling manifest");

        let mut req = self.client.get(&url).header(
            "Accept",
            "application/vnd.docker.distribution.manifest.v2+json",
        );
        if !token.is_empty() {
            req = req.bearer_auth(&token);
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            return Err(Error::Registry {
                status: resp.status().as_u16(),
                url,
            });
        }

        let manifest: ImageManifest = resp.json().await?;
        debug!(layers = manifest.layers.len(), "Manifest pulled");
        Ok(manifest)
    }

    /// Download a blob (layer or config) by digest, storing it in `cache_dir`.
    ///
    /// If the blob already exists in the cache with a matching digest, the
    /// download is skipped.
    pub async fn pull_blob(&self, digest: &str, cache_dir: &Path) -> Result<PathBuf> {
        let filename = digest.replace(':', "_");
        let dest = cache_dir.join(&filename);

        if dest.exists() {
            // Verify cached blob digest.
            let data = fs::read(&dest).await?;
            let actual = format!("sha256:{}", hex::encode(Sha256::digest(&data)));
            if actual == digest {
                debug!(digest, "Blob cache hit");
                return Ok(dest);
            }
            debug!(digest, "Blob cache corrupted, re-downloading");
        }

        let token = self.get_token().await?;
        let url = format!(
            "https://{}/v2/{}/blobs/{}",
            self.registry, self.repository, digest
        );
        info!(url, "Pulling blob");

        let mut req = self.client.get(&url);
        if !token.is_empty() {
            req = req.bearer_auth(&token);
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            return Err(Error::Registry {
                status: resp.status().as_u16(),
                url,
            });
        }

        let bytes = resp.bytes().await?;

        // Verify digest.
        let actual = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
        if actual != digest {
            return Err(Error::DigestMismatch {
                expected: digest.to_string(),
                actual,
            });
        }

        fs::create_dir_all(cache_dir).await?;
        fs::write(&dest, &bytes).await?;
        info!(path = %dest.display(), "Blob cached");
        Ok(dest)
    }
}

/// Parse a `Www-Authenticate: Bearer realm="...",service="...",scope="..."` header.
fn parse_www_authenticate(header: &str) -> (String, String, String) {
    let mut realm = String::new();
    let mut service = String::new();
    let mut scope = String::new();

    for part in header.split(',') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("Bearer realm=").or_else(|| part.strip_prefix("realm="))
        {
            realm = v.trim_matches('"').to_string();
        } else if let Some(v) = part.strip_prefix("service=") {
            service = v.trim_matches('"').to_string();
        } else if let Some(v) = part.strip_prefix("scope=") {
            scope = v.trim_matches('"').to_string();
        }
    }

    (realm, service, scope)
}
