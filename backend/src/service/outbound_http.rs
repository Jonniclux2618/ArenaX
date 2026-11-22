//! Centralized outbound HTTP destination policy for provider integrations.
//!
//! # Security properties
//! - Allowed hosts and schemes are explicit per integration via [`OutboundPolicy::https`].
//! - DNS results are checked against prohibited private/link-local/metadata ranges before
//!   any connection is attempted ([`is_prohibited_address`]).
//! - Redirects are disabled at the HTTP client level; callers that need to follow a redirect
//!   must call [`OutboundPolicy::validate_redirect`] first so the target is re-validated.
//! - Credentials (`Authorization`, `Cookie`) are never forwarded across origins; they must
//!   be supplied per-request by the caller after redirect validation.
//! - Connection and response sizes are bounded: [`OutboundPolicy`] carries a
//!   `response_limit` and [`send_bounded`] enforces it.
//! - Failures are logged without secrets: error strings never include bearer tokens,
//!   API keys, or response body fragments that may contain sensitive values.

use reqwest::{redirect::Policy as RedirectPolicy, Client, Method, Url};
use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::Duration,
};

#[derive(Debug, Clone)]
pub struct OutboundPolicy {
    allowed_hosts: HashSet<String>,
    allowed_schemes: HashSet<String>,
    connect_timeout: Duration,
    response_limit: usize,
}

impl OutboundPolicy {
    pub fn https(hosts: &[&str], response_limit: usize) -> Self {
        Self {
            allowed_hosts: hosts.iter().map(|host| (*host).to_ascii_lowercase()).collect(),
            allowed_schemes: HashSet::from(["https".to_string()]),
            connect_timeout: Duration::from_secs(10),
            response_limit,
        }
    }

    pub fn validate_url(&self, value: &str) -> Result<Url, String> {
        let url = Url::parse(value).map_err(|_| "invalid outbound URL".to_string())?;
        if !self.allowed_schemes.contains(url.scheme()) {
            return Err("outbound URL scheme is not allowed".to_string());
        }
        let host = url.host_str().ok_or_else(|| "outbound URL host is missing".to_string())?;
        let normalized = host.trim_end_matches('.').to_ascii_lowercase();
        if !self.allowed_hosts.contains(&normalized) {
            return Err("outbound URL host is not allowed".to_string());
        }
        Ok(url)
    }

    pub async fn resolve_and_validate(&self, url: &Url) -> Result<(), String> {
        let host = url.host_str().ok_or_else(|| "outbound URL host is missing".to_string())?;
        let addresses = tokio::net::lookup_host((host, url.port_or_known_default().unwrap_or(443)))
            .await
            .map_err(|_| "outbound hostname could not be resolved".to_string())?;
        let mut found = false;
        for address in addresses {
            found = true;
            if is_prohibited_address(address.ip()) {
                return Err("outbound URL resolves to a prohibited network range".to_string());
            }
        }
        if !found {
            return Err("outbound hostname resolved to no addresses".to_string());
        }
        Ok(())
    }

    pub fn client(&self) -> Result<Client, String> {
        Client::builder()
            .redirect(RedirectPolicy::none())
            .connect_timeout(self.connect_timeout)
            .tcp_keepalive(Duration::from_secs(30))
            .build()
            .map_err(|_| "could not create outbound HTTP client".to_string())
    }

    pub fn response_limit(&self) -> usize {
        self.response_limit
    }

    pub fn validate_redirect(&self, current: &Url, location: &str) -> Result<Url, String> {
        let redirect = current.join(location).map_err(|_| "invalid redirect URL".to_string())?;
        self.validate_url(redirect.as_str())
    }
}

pub fn is_prohibited_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_unspecified()
                || ip == Ipv4Addr::new(169, 254, 169, 254)
                || ip.is_documentation()
                || ip.is_broadcast()
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_multicast()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
        }
    }
}

/// Credential headers that must never be forwarded to a redirect target.
///
/// When following a redirect that changes the origin, these headers must be
/// stripped.  [`send_bounded`] disables redirects entirely at the HTTP client
/// level, so callers that need redirect support must call
/// [`OutboundPolicy::validate_redirect`] and then issue a new request — without
/// copying these headers unless they explicitly re-authorise the new target.
pub const CREDENTIAL_HEADERS: &[&str] = &["authorization", "cookie", "proxy-authorization"];

pub async fn send_bounded(
    policy: &OutboundPolicy,
    method: Method,
    url: &str,
) -> Result<(reqwest::StatusCode, Vec<u8>), String> {
    let parsed = policy.validate_url(url)?;
    policy.resolve_and_validate(&parsed).await?;
    let response = policy
        .client()?
        .request(method, parsed)
        .send()
        .await
        // Intentionally omit the reqwest error message: it may contain the URL
        // (which could include API-key query parameters) or provider error body.
        .map_err(|_| "outbound provider request failed".to_string())?;
    let status = response.status();
    if status.is_redirection() {
        // Redirects are never followed automatically.  The caller must call
        // validate_redirect, obtain a new validated URL, and issue a fresh
        // request — without forwarding CREDENTIAL_HEADERS unless the new
        // origin has been explicitly re-authorised.
        return Err("outbound provider redirects must be revalidated".to_string());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "outbound provider response could not be read".to_string())?;
    if bytes.len() > policy.response_limit() {
        return Err("outbound provider response exceeded the size limit".to_string());
    }
    Ok((status, bytes.to_vec()))
}
