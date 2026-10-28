//! Federation (in-house approximation): query a remote daemon instance over HTTP.
pub async fn remote_list(base_url: &str, namespace: &str, token: &str) -> Result<Vec<serde_json::Value>, String> {
    let url = format!("{}/list?namespace={}", base_url.trim_end_matches('/'), namespace);
    let client = reqwest::Client::new();
    let resp = client.get(&url).bearer_auth(token).send().await.map_err(|e| e.to_string())?;
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(v.as_array().cloned().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    #[test]
    fn url_format() {
        let u = format!("{}/list?namespace={}", "http://x".trim_end_matches('/'), "ns");
        assert_eq!(u, "http://x/list?namespace=ns");
    }
}
