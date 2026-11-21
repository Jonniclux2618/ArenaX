//! Centralized outbound HTTP destination policy for provider integrations.

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
        .map_err(|_| "outbound provider request failed".to_string())?;
    let status = response.status();
    if status.is_redirection() {
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
