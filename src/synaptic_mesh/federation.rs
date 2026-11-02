//! Federation (in-house approximation): query a remote daemon instance over HTTP.
// `remote_list` is also exercised in production by
// `MeshService::status_report`, which probes the configured HTTP peer.
pub async fn remote_list(base_url: &str, namespace: &str, token: &str) -> Result<Vec<serde_json::Value>, String> {
    let url = format!("{}/list?namespace={}", base_url.trim_end_matches('/'), namespace);
    let client = reqwest::Client::new();
    let resp = client.get(&url).bearer_auth(token).send().await.map_err(|e| e.to_string())?;
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(v.as_array().cloned().unwrap_or_default())
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
}
