//! Transport shared by the file-based Codex and Claude usage readers.
use serde_json::Value;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

const MAX_BYTES: u64 = 256 * 1024;

/// 프록시는 요청마다 OS 설정을 따라 고름 ([`crate::net`])
pub(crate) static CLIENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(8)));
    crate::net::tls(config).build().new_agent()
});

pub(crate) fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(bytes)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RequestError {
    AuthRejected,
    RateLimited(Option<Duration>),
    Server,
    Unavailable,
}

fn retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let when = jiff::fmt::rfc2822::parse(value).ok()?.timestamp();
    SystemTime::from(when).duration_since(now).ok()
}

pub(crate) fn get_json(
    client: &ureq::Agent,
    url: &str,
    access_token: &str,
    headers: &[(&str, &str)],
) -> Result<Value, RequestError> {
    let mut authorization = ureq::http::HeaderValue::from_str(&format!("Bearer {access_token}"))
        .map_err(|_| RequestError::Unavailable)?;
    authorization.set_sensitive(true);
    let mut response = crate::net::call(url, |proxy| {
        let mut request = client
            .get(url)
            .config()
            .proxy(proxy)
            .build()
            .header("Authorization", authorization.clone())
            .header("Accept", "application/json")
            .header("User-Agent", concat!("usAgI/", env!("CARGO_PKG_VERSION")));
        for &(name, value) in headers {
            request = request.header(name, value);
        }
        request.call()
    })
    .map_err(|_| RequestError::Unavailable)?;
    match response.status().as_u16() {
        200 => {}
        401 | 403 => return Err(RequestError::AuthRejected),
        429 => {
            let delay = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| retry_after(value, SystemTime::now()));
            return Err(RequestError::RateLimited(delay));
        }
        500..=599 => return Err(RequestError::Server),
        _ => return Err(RequestError::Unavailable),
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_BYTES)
        .read_to_vec()
        .map_err(|_| RequestError::Unavailable)?;
    serde_json::from_slice(&bytes).map_err(|_| RequestError::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn retry_after_accepts_seconds_and_http_dates() {
        let now: SystemTime = "2015-10-21T07:27:00Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .into();
        assert_eq!(retry_after("120", now), Some(Duration::from_secs(120)));
        assert_eq!(
            retry_after("Wed, 21 Oct 2015 07:28:00 GMT", now),
            Some(Duration::from_secs(60))
        );
        assert_eq!(retry_after("unknown", now), None);
    }

    #[test]
    fn production_agent_starts_a_tls_handshake() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("https://{}", listener.local_addr().unwrap());

        // No server answers the handshake. This should return an error, even
        // when the transport is compiled against the operating system TLS API.
        let result = CLIENT
            .get(url)
            .config()
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(1)))
            .build()
            .call();
        assert!(result.is_err());

        let (mut connection, _) = listener.accept().unwrap();
        connection
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut record_type = [0];
        connection.read_exact(&mut record_type).unwrap();
        assert_eq!(record_type[0], 22); // TLS handshake record
    }
}
