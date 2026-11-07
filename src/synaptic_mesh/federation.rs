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

    /// Read one HTTP request (headers + declared body) before responding.
    /// A server that answers before reading the request closes the socket
    /// with the request still unread, which makes the kernel RST the
    /// connection and fails the client's in-flight write. That is what
    /// these tests hit under tokio 1.54, where accepted sockets start
    /// readable and writable (tokio#8435). Mirrors `read_http_request`
    /// in `embed.rs`.
    async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut acc: Vec<u8> = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            acc.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&acc).to_string();
            if let Some(idx) = text.find("\r\n\r\n") {
                let mut content_length = 0usize;
                for line in text[..idx].lines() {
                    if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = rest.trim().parse().unwrap_or(0);
                    }
                }
                if acc.len() >= idx + 4 + content_length {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&acc).to_string()
    }

    /// Serve exactly one request: accept, read the request first, then
    /// write the given status/body and close. Returns the request text so
    /// the test can assert on what the client sent. The read-first
    /// ordering lives here and nowhere else, so no test can get it wrong.
    async fn serve_once(
        listener: TcpListener,
        status_line: &str,
        content_type: &str,
        body: &str,
    ) -> String {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = read_request(&mut socket).await;
        let response = format!(
            "{status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        use tokio::io::AsyncWriteExt;
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.shutdown().await.ok();
        request
    }

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
        let task = tokio::spawn(serve_once(
            listener,
            "HTTP/1.1 200 OK",
            "application/json",
            body,
        ));
        let result = remote_list(&format!("http://127.0.0.1:{}", port), "ns", "t").await;
        assert!(result.is_ok());
        let arr = result.unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "m1");
        let request = task.await.unwrap();
        assert!(
            request.contains("GET /list?namespace=ns"),
            "expected a GET for /list, got: {request}"
        );
    }

    /// An unset (empty) token must not produce a bare `Authorization` header.
    #[tokio::test]
    async fn remote_list_sends_no_bearer_header_for_an_empty_token() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = "[{\"id\":\"m1\"}]";
        let task = tokio::spawn(serve_once(
            listener,
            "HTTP/1.1 200 OK",
            "application/json",
            body,
        ));
        let result =
            remote_list(&format!("http://127.0.0.1:{}", addr.port()), "ns", "   ").await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(result.unwrap()[0]["id"], "m1");
        let request = task.await.unwrap().to_lowercase();
        assert!(
            !request.contains("authorization:"),
            "an empty token must omit the Authorization header, got: {request}"
        );
    }

    /// A non-success status must be reported as a federation error, never
    /// decoded as JSON (a probe line, not a parse failure).
    #[tokio::test]
    async fn remote_list_reports_a_non_success_status() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(serve_once(
            listener,
            "HTTP/1.1 404 Not Found",
            "text/plain",
            "gone",
        ));
        let result =
            remote_list(&format!("http://127.0.0.1:{}", addr.port()), "ns", "t").await;
        let err = result.expect_err("a 404 cannot be a JSON decode path");
        assert!(err.contains("HTTP 404"), "got: {err}");
        let request = task.await.unwrap().to_lowercase();
        assert!(
            request.contains("authorization: bearer t"),
            "a set token must be sent, got: {request}"
        );
    }
}
