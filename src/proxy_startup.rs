//! Read-only proxy preflight with a short startup timeout and useful error causes.
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug)]
pub(crate) enum ValidationError {
    Network(String),
    Protocol(String),
}

#[derive(Deserialize)]
struct Reply {
    jsonrpc: String,
    id: Option<String>,
    result: Option<Info>,
    error: Option<RpcError>,
}

// Same wire requirements as the pinned RGB-lib ServerInfoResponse. Do not
// accept an older protocol or coerce non-compliant field types to success.
#[derive(Deserialize)]
struct Info {
    protocol_version: String,
    version: String,
    uptime: i64,
    current_time: Option<String>,
    max_expiry_secs: Option<String>,
    max_quota: Option<String>,
    max_pow_difficulty: Option<String>,
    retention_grace_secs: Option<String>,
}

#[derive(Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

fn network_error(error: reqwest::Error) -> ValidationError {
    use std::error::Error;
    if error.is_timeout() {
        return ValidationError::Network("RGB proxy server.info request timed out".into());
    }
    let error = error.without_url();
    let mut message = format!("RGB proxy server.info: {error}");
    let mut source = error.source();
    for _ in 0..6 {
        let Some(cause) = source else { break };
        message.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    ValidationError::Network(message)
}

pub(crate) async fn validate(url: &str) -> Result<(), ValidationError> {
    validate_with_timeout(url, Duration::from_secs(15)).await
}

async fn validate_with_timeout(url: &str, timeout: Duration) -> Result<(), ValidationError> {
    let client = reqwest::Client::builder()
        .http1_only()
        .connect_timeout(Duration::from_secs(5))
        .timeout(timeout)
        .build()
        .map_err(network_error)?;
    let reply = client
        .post(url)
        .json(&serde_json::json!({"method":"server.info","jsonrpc":"2.0","id":null}))
        .send()
        .await
        .map_err(network_error)?
        .error_for_status()
        .map_err(network_error)?
        .json::<Reply>()
        .await
        .map_err(network_error)?;
    if reply.jsonrpc != "2.0" || reply.id.is_some() {
        return Err(ValidationError::Network(
            "RGB proxy returned an invalid JSON-RPC envelope".into(),
        ));
    }
    if let Some(error) = reply.error {
        return Err(ValidationError::Network(format!(
            "RGB proxy RPC error {}: {}",
            error.code, error.message
        )));
    }
    let info = reply.result.ok_or_else(|| {
        ValidationError::Network("RGB proxy response has no server.info result".into())
    })?;
    if info.protocol_version != "1.0" {
        return Err(ValidationError::Protocol(info.protocol_version));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    fn fixture(body: &'static str, delay: bool) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",body.len()).unwrap();
            stream.flush().unwrap();
            if delay {
                std::thread::sleep(Duration::from_millis(350));
            }
            let _ = stream.write_all(body.as_bytes());
        });
        (format!("http://{address}"), thread)
    }
    #[tokio::test]
    async fn accepts_new_protocol_and_rejects_old_protocol_or_missing_result() {
        for (body, expected) in [
            (
                r#"{"jsonrpc":"2.0","id":null,"result":{"protocol_version":"1.0","version":"0.3.0","uptime":1}}"#,
                "ok",
            ),
            (
                r#"{"jsonrpc":"2.0","id":null,"result":{"protocol_version":"0.2","version":"old","uptime":1}}"#,
                "protocol",
            ),
            (r#"{"jsonrpc":"2.0","id":null}"#, "network"),
        ] {
            let (url, thread) = fixture(body, false);
            let result = validate(&url).await;
            match (expected, result) {
                ("ok", Ok(()))
                | ("protocol", Err(ValidationError::Protocol(_)))
                | ("network", Err(ValidationError::Network(_))) => (),
                (_, other) => panic!("Unexpected preflight result: {other:?}"),
            }
            thread.join().unwrap();
        }
    }
    #[tokio::test]
    async fn reports_malformed_body_and_cancels_slow_body_without_waiting_120_seconds() {
        for delay in [false, true] {
            let (url, thread) = fixture("<html>unavailable</html>", delay);
            let start = std::time::Instant::now();
            let error = validate_with_timeout(&url, Duration::from_millis(100))
                .await
                .unwrap_err();
            let ValidationError::Network(message) = error else {
                panic!("Expected network error")
            };
            assert!(
                message.contains(if delay { "timed out" } else { "expected value" }),
                "{message}"
            );
            assert!(start.elapsed() < Duration::from_secs(2));
            thread.join().unwrap();
        }
    }
}
