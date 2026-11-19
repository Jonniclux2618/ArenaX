use crate::service::payment_provider::PaymentError;
use futures_util::StreamExt;
use serde::de::DeserializeOwned;
use std::net::{IpAddr, SocketAddr};

const PAYMENT_PROVIDER_HOSTS: &[&str] = &["api.paystack.co", "api.flutterwave.com"];
const MAX_PROVIDER_RESPONSE_BYTES: usize = 1024 * 1024;

pub(crate) async fn payment_provider_client(
    host: &'static str,
) -> Result<reqwest::Client, PaymentError> {
    if !PAYMENT_PROVIDER_HOSTS.contains(&host) {
        return Err(PaymentError::Provider(
            "payment provider destination is not allowed".into(),
        ));
    }

    let addresses = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::lookup_host((host, 443)),
    )
        .await
        .map_err(|_| PaymentError::Provider("payment provider DNS lookup timed out".into()))?
        .map_err(|_| PaymentError::Provider("payment provider DNS lookup failed".into()))?
        .collect::<Vec<SocketAddr>>();
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|address| is_prohibited_destination(address.ip()))
    {
        return Err(PaymentError::Provider(
            "payment provider destination resolved to a prohibited address".into(),
        ));
    }

    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(30))
        .resolve_to_addrs(host, &addresses)
        .build()
        .map_err(|_| PaymentError::Provider("payment provider HTTP client could not be built".into()))
}

pub(crate) async fn parse_provider_json<T: DeserializeOwned>(
    response: reqwest::Response,
    provider: &str,
) -> Result<T, PaymentError> {
    if response.status().is_redirection() {
        return Err(PaymentError::Provider(format!(
            "{provider} returned a redirect; redirects are disabled"
        )));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PROVIDER_RESPONSE_BYTES as u64)
    {
        return Err(PaymentError::Provider(format!(
            "{provider} response exceeded the size limit"
        )));
    }

    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            PaymentError::Provider(format!("{provider} response could not be read"))
        })?;
        if body.len().saturating_add(chunk.len()) > MAX_PROVIDER_RESPONSE_BYTES {
            return Err(PaymentError::Provider(format!(
                "{provider} response exceeded the size limit"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map_err(|_| PaymentError::Provider(format!("{provider} returned invalid JSON")))
}

pub(crate) fn is_prohibited_destination(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [first, second, third, _] = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_unspecified()
                || ip.is_multicast()
                || first == 0
                || first >= 240
                || first == 100 && (64..=127).contains(&second)
                || first == 192 && second == 0 && (third == 0 || third == 2)
                || first == 192 && second == 88 && third == 99
                || first == 198 && (second == 18 || second == 19 || second == 51 && third == 100)
                || first == 203 && second == 0 && third == 113
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.to_ipv4_mapped()
                    .is_some_and(|mapped| is_prohibited_destination(mapped.into()))
                || segments[0] & 0xfe00 == 0xfc00
                || segments[0] & 0xffc0 == 0xfe80
                || segments[0] & 0xe000 != 0x2000
                || segments[0] == 0x2001 && segments[1] == 0x0db8
                || segments[0] == 0x2002
        }
    }
}
