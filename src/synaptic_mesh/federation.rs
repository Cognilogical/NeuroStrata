//! Federation (in-house approximation): query a remote daemon instance over HTTP.
//! `remote_list` is also exercised in production by
//! `MeshService::status_report`, which probes the configured HTTP peer.
pub async fn remote_list(base_url: &str, namespace: &str, token: &str) -> Result<Vec<serde_json::Value>, String> {
    let url = format!("{}/list?namespace={}", base_url.trim_end_matches('/'), namespace);
    let client = reqwest::Client::new();
    // An empty (unset) token must not send a bare `Authorization: Bearer` header.
    let token = token.trim();
    let mut req = client.get(&url);
    if !token.is_empty() {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("federation endpoint returned HTTP {}", resp.status()));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    v.as_array()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("remote_list expected JSON array from {base_url}, got non-array"))
        .map_err(|e| format!("{:#}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn url_format() {
        let u = format!("{}/list?namespace={}", "http://x".trim_end_matches('/'), "ns");
        assert_eq!(u, "http://x/list?namespace=ns");
    }

    #[tokio::test]
    async fn remote_list_fetches_json_array_from_http_peer() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let port = addr.port();
        // Spawn a minimal server in a separate tokio task that serves a JSON array
        let body = "[{\"id\":\"m1\"}]";
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                use tokio::io::AsyncWriteExt;
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.ok();
                break;
            }
        });
        let result = remote_list(&format!("http://127.0.0.1:{}", port), "ns", "t").await;
        assert!(result.is_ok());
        let arr = result.unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "m1");
    }

    /// An unset (empty) token must not produce a bare `Authorization` header.
    #[tokio::test]
    async fn remote_list_sends_no_bearer_header_for_an_empty_token() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_lowercase();
            let body = "[{\"id\":\"m1\"}]";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.ok();
            assert!(
                !request.contains("authorization:"),
                "an empty token must omit the Authorization header, got: {request}"
            );
        });
        let result =
            remote_list(&format!("http://127.0.0.1:{}", addr.port()), "ns", "   ").await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(result.unwrap()[0]["id"], "m1");
        task.await.unwrap();
    }

    /// A non-success status must be reported as a federation error, never
    /// decoded as JSON (a probe line, not a parse failure).
    #[tokio::test]
    async fn remote_list_reports_a_non_success_status() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            use tokio::io::AsyncWriteExt;
            let body = "gone";
            let response = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.ok();
        });
        let result =
            remote_list(&format!("http://127.0.0.1:{}", addr.port()), "ns", "t").await;
        let err = result.expect_err("a 404 cannot be a JSON decode path");
        assert!(err.contains("HTTP 404"), "got: {err}");
        task.await.unwrap();
    }
}
