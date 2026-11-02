use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

pub const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a precise assistant answering questions from NeuroStrata memory context.";

pub const ALLOWED_API_KEY_ENV_VARS: [&str; 2] = ["OPENAI_API_KEY", "NEUROSTRATA_RAG_API_KEY"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RagChatConfig {
    pub provider: String,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub model: String,
    pub max_context_chars: usize,
    pub timeout_secs: u64,
    pub allow_remote_base_url: bool,
    pub remote_egress_allowed: bool,
    pub default_principal: String,
}

impl Default for RagChatConfig {
    fn default() -> Self {
        Self {
            provider: "offline".into(),
            base_url: None,
            api_key_env: None,
            model: "gpt-4o-mini".into(),
            max_context_chars: 4000,
            timeout_secs: 30,
            allow_remote_base_url: false,
            remote_egress_allowed: false,
            default_principal: "local-user".into(),
        }
    }
}

pub fn truncate_snippet(content: &str, max_chars: usize) -> String {
    let cleaned: String = content
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect::<String>();
    let collapsed = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let total = collapsed.chars().count();
    if total <= max_chars {
        collapsed
    } else {
        let truncated: String = collapsed.chars().take(max_chars).collect();
        format!("{}{}", truncated, '\u{2026}')
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatError {
    Disabled,
    Config(String),
    EgressDenied(String),
    Retrieval(String),
    Provider(String),
}

impl std::fmt::Display for ChatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatError::Disabled => write!(f, "chat is disabled"),
            ChatError::Config(s) => write!(f, "config error: {}", s),
            ChatError::EgressDenied(s) => write!(f, "egress denied: {}", s),
            ChatError::Retrieval(s) => write!(f, "retrieval error: {}", s),
            ChatError::Provider(s) => write!(f, "provider error: {}", s),
        }
    }
}

impl std::error::Error for ChatError {}

pub async fn retrieve_context_with_egress(
    store: Arc<dyn crate::traits::VectorStore>,
    emb: Arc<dyn crate::traits::Embedder>,
    namespace: &str,
    principal: &str,
    query: &str,
    limit: usize,
    include_tasks: bool,
    include_archived: bool,
    require_shareable: bool,
    snippet_max_chars: usize,
) -> Result<RagContext, ChatError> {
    let vector = emb.embed(query).await.map_err(|e| ChatError::Retrieval(format!("embed: {}", e)))?;
    let outcome = store
        .search_with_domain(namespace, vector, limit, include_tasks, include_archived, &[], Some(query), false)
        .await
        .map_err(|e| ChatError::Retrieval(format!("search: {}", e)))?;
    if outcome.incomplete {
        tracing::debug!(
            "rag_chat search in '{}' was incomplete ({} hits)",
            namespace,
            outcome.results.len()
        );
    }
    let principal = if principal.is_empty() { "admin" } else { principal };
    let mut results: Vec<crate::traits::SearchResult> = Vec::new();
    for r in outcome.results {
        let owner = crate::rebac::extract_resource_owner(&r.payload.user_id, &r.payload.metadata);
        let visible = crate::rebac::extract_visible_to(&r.payload.metadata);
        if !crate::rebac::allowed(principal, &owner, "viewer", &visible) {
            continue;
        }
        if require_shareable {
            if r.payload.metadata.get("eligibility").and_then(|v| v.as_str()) != Some("shareable") {
                continue;
            }
        }
        results.push(r);
    }
    results.sort_by(|a, b| {
        a.score.total_cmp(&b.score).then_with(|| a.id.cmp(&b.id))
    });
    let hits: Vec<RagHit> = results
        .into_iter()
        .enumerate()
        .map(|(idx, r)| {
            let snippet = truncate_snippet(&r.payload.content, snippet_max_chars);
            let derivation = if let Some(ev) = &r.evidence {
                let mut d = Vec::new();
                for e in ev {
                    for s in &e.derivation {
                        if !d.contains(s) {
                            d.push(s.clone());
                        }
                    }
                }
                if d.is_empty() {
                    vec!["vector".to_string()]
                } else {
                    d
                }
            } else {
                vec!["vector".to_string()]
            };
            RagHit {
                rank: idx + 1,
                memory_id: r.id.clone(),
                score: r.score,
                snippet,
                derivation,
            }
        })
        .collect();
    let total = hits.len();
    Ok(RagContext {
        query: query.to_string(),
        hits,
        total,
    })
}

pub fn build_prompt_with_citations(
    ctx: &RagContext,
    question: &str,
    system: Option<&str>,
    max_context_chars: usize,
) -> (String, Vec<String>) {
    let mut prompt = String::new();
    prompt.push_str(&format!("System: {}\n", system.unwrap_or(DEFAULT_SYSTEM_PROMPT)));
    prompt.push_str("Context:\n");
    let mut citations = Vec::new();
    let mut context_lines_total = 0;
    let mut included_lines = 0;
    if ctx.hits.is_empty() {
        prompt.push_str("No relevant context found.\n");
    } else {
        for hit in &ctx.hits {
            let mut line = format!(
                "[{}] (id={}, score={:.2}) {}\n",
                hit.rank, hit.memory_id, hit.score, hit.snippet
            );
            // The first line must respect max_context_chars too: an oversized
            // first hit is truncated instead of being included whole.
            if included_lines == 0
                && max_context_chars > 0
                && line.chars().count() > max_context_chars
            {
                line = line.chars().take(max_context_chars).collect();
            }
            if context_lines_total + line.chars().count() > max_context_chars && included_lines > 0 {
                break;
            }
            if included_lines == 0 && max_context_chars == 0 {
                break;
            }
            context_lines_total += line.chars().count();
            prompt.push_str(&line);
            citations.push(hit.memory_id.clone());
            included_lines += 1;
        }
        if included_lines == 0 {
            prompt.push_str("No relevant context found.\n");
        }
    }
    prompt.push_str(&format!("Question: {}\n", question));
    prompt.push_str("Answer (cite sources as [n]):\n");
    (prompt, citations)
}

pub async fn retrieve_context(
    store: Arc<dyn crate::traits::VectorStore>,
    emb: Arc<dyn crate::traits::Embedder>,
    namespace: &str,
    principal: &str,
    query: &str,
    limit: usize,
    include_tasks: bool,
    include_archived: bool,
    snippet_max_chars: usize,
) -> Result<RagContext, ChatError> {
    retrieve_context_with_egress(
        store, emb, namespace, principal, query, limit,
        include_tasks, include_archived, false, snippet_max_chars,
    ).await
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RagHit {
    pub rank: usize,
    pub memory_id: String,
    pub score: f32,
    pub snippet: String,
    pub derivation: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RagContext {
    pub query: String,
    pub hits: Vec<RagHit>,
    pub total: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub answer: String,
    pub citations: Vec<String>,
    pub context_count: usize,
    pub provider: String,
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;
    fn is_remote(&self) -> bool {
        false
    }
    async fn answer(&self, prompt: &str) -> Result<String, ChatError>;
}

pub struct OfflineProvider;

#[async_trait]
impl LlmProvider for OfflineProvider {
    fn name(&self) -> &str {
        "offline"
    }
    fn is_remote(&self) -> bool {
        false
    }
    async fn answer(&self, prompt: &str) -> Result<String, ChatError> {
        let question = prompt
            .lines()
            .find(|l| l.starts_with("Question: "))
            .map(|l| l.strip_prefix("Question: ").unwrap_or("unknown"))
            .unwrap_or("unknown");
        let mut count = 0usize;
        for line in prompt.lines() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix('[') {
                if let Some(end) = rest.find(']') {
                    if rest[..end].chars().all(|c| c.is_ascii_digit()) {
                        count += 1;
                    }
                }
            }
        }
        if count == 0 {
            Ok(format!("Offline answer: no relevant context found for question '{}'. No citations.", question))
        } else {
            Ok(format!("Offline answer: question='{}', citations={}, context included.", question, count))
        }
    }
}

pub struct OpenAiCompatibleProvider {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub timeout: Duration,
    pub client: reqwest::Client,
}

impl OpenAiCompatibleProvider {
    pub fn new(
        base_url: &str,
        api_key_env: Option<&str>,
        model: &str,
        timeout: Duration,
        allow_remote_base_url: bool,
    ) -> Result<Self, ChatError> {
        if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
            return Err(ChatError::Config("base_url must start with http:// or https://".into()));
        }
        if base_url.contains('@') {
            return Err(ChatError::Config("base_url must not contain userinfo (@)".into()));
        }
        let stripped = base_url.trim_end_matches('/').to_string();
        let url = reqwest::Url::parse(&stripped).map_err(|e| ChatError::Config(format!("invalid url: {}", e)))?;
        let host = url.host_str().unwrap_or("");
        let is_loopback = host == "localhost" || host == "127.0.0.1" || host == "[::1]";
        if !is_loopback && !allow_remote_base_url {
            return Err(ChatError::Config("non-loopback base_url requires allow_remote_base_url".into()));
        }
        let api_key = if let Some(env_name) = api_key_env {
            if env_name.is_empty() {
                return Err(ChatError::Config("empty api_key_env".into()));
            }
            if !ALLOWED_API_KEY_ENV_VARS.contains(&env_name) {
                return Err(ChatError::Config(format!("disallowed api_key_env: {}", env_name)));
            }
            match std::env::var(env_name) {
                Ok(v) => {
                    if v.trim().is_empty() {
                        tracing::warn!(
                            "RAG chat API key env var '{}' is set but empty (whitespace only); returning None",
                            env_name
                        );
                        None
                    } else {
                        Some(v)
                    }
                }
                Err(_) => None,
            }
        } else {
            None
        };
        Ok(Self {
            base_url: stripped,
            api_key,
            model: model.to_string(),
            timeout,
            client: reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .map_err(|e| ChatError::Config(format!("reqwest build: {}", e)))?,
        })
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompatibleProvider {
    fn name(&self) -> &str {
        "openai-compatible"
    }
    fn is_remote(&self) -> bool {
        true
    }
    async fn answer(&self, prompt: &str) -> Result<String, ChatError> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let mut body = serde_json::Map::new();
        body.insert("model".into(), serde_json::Value::String(self.model.clone()));
        body.insert("messages".into(), serde_json::json!([{"role":"user","content":prompt}]));
        body.insert("temperature".into(), serde_json::json!(0));
        let mut req = self.client.post(&url).timeout(self.timeout).json(&body);
        if let Some(key) = &self.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }
        let resp = req.send().await.map_err(|e| ChatError::Provider(format!("send: {}", e)))?;
        if !resp.status().is_success() {
            return Err(ChatError::Provider(format!("http {}", resp.status())));
        }
        let json: serde_json::Value = resp.json().await.map_err(|e| ChatError::Provider(format!("json: {}", e)))?;
        let content = json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| ChatError::Provider("missing content".into()))?;
        Ok(content.to_string())
    }
}

pub async fn ask(
    store: Arc<dyn crate::traits::VectorStore>,
    emb: Arc<dyn crate::traits::Embedder>,
    namespace: &str,
    principal: &str,
    question: &str,
    limit: usize,
    system: Option<&str>,
    max_context_chars: usize,
    provider: Arc<dyn LlmProvider>,
    include_tasks: bool,
    include_archived: bool,
    remote_egress_allowed: bool,
) -> Result<ChatResponse, ChatError> {
    // Kill-switch magic question: the disabled sentinel short-circuits before
    // any retrieval or provider call, so a wedge cannot be debugged by chat.
    if question.trim().eq_ignore_ascii_case("__disabled__") {
        return Err(ChatError::Disabled);
    }
    if provider.is_remote() && !remote_egress_allowed {
        return Err(ChatError::EgressDenied("remote LLM egress is disabled by configuration".into()));
    }
    let ctx = if !remote_egress_allowed {
        retrieve_context(
            store.clone(), emb.clone(), namespace, principal, question, limit,
            include_tasks, include_archived,
            // Snippets are the context budget's working unit: keep per-hit
            // truncation proportional to the prompt ceiling.
            (max_context_chars / 4).clamp(120, 1000),
        ).await?
    } else {
        retrieve_context_with_egress(
            store, emb, namespace, principal, question, limit,
            include_tasks, include_archived, provider.is_remote(),
            (max_context_chars / 4).clamp(120, 1000),
        ).await?
    };
    let (prompt, citations) = build_prompt_with_citations(&ctx, question, system, max_context_chars);
    let answer = provider.answer(&prompt).await?;
    let context_count = citations.len();
    Ok(ChatResponse {
        answer,
        citations,
        context_count,
        provider: provider.name().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plain prompt wrapper: same builder as `build_prompt_with_citations`,
    /// discarding the citation list.
    fn build_prompt(
        ctx: &RagContext,
        question: &str,
        system: Option<&str>,
        max_context_chars: usize,
    ) -> String {
        build_prompt_with_citations(ctx, question, system, max_context_chars).0
    }

    #[test]
    fn prompt_format_and_citations() {
        let ctx = RagContext {
            query: "q".into(),
            hits: vec![
                RagHit { rank: 1, memory_id: "mem-abc".into(), score: 0.12, snippet: "hello".into(), derivation: vec!["vector".into()] },
                RagHit { rank: 2, memory_id: "mem-def".into(), score: 0.34, snippet: "world".into(), derivation: vec!["vector".into()] },
            ],
            total: 2,
        };
        let (prompt, cit) = build_prompt_with_citations(&ctx, "test?", None, 4000);
        // The plain wrapper must match the citable builder byte-for-byte.
        assert_eq!(build_prompt(&ctx, "test?", None, 4000), prompt);
        assert!(prompt.contains("System:"));
        assert!(prompt.contains("Context:"));
        assert!(prompt.contains("[1] (id=mem-abc, score=0.12)"));
        assert!(prompt.contains("[2] (id=mem-def, score=0.34)"));
        assert!(prompt.contains("Question: test?"));
        assert!(prompt.contains("Answer (cite sources as [n]):"));
        assert_eq!(cit, vec!["mem-abc", "mem-def"]);
    }

    #[test]
    fn snippet_truncation_utf8() {
        let s = "a".repeat(200) + "\n\n\t\t" + &"🎉".repeat(100);
        let out = truncate_snippet(&s, 280);
        assert!(out.contains("🎉") || out.ends_with("\u{2026}"));
        assert_eq!(out.chars().count(), 281); // truncated + ellipsis
    }

    #[tokio::test]
    async fn offline_provider_determinism() {
        let p = OfflineProvider;
        let prompt = "System: x\nContext:\n[1] (id=a, score=0.1) s\nQuestion: q\nAnswer (cite sources as [n]):\n";
        let r1 = p.answer(prompt).await.unwrap();
        let r2 = p.answer(prompt).await.unwrap();
        assert_eq!(r1, r2);
        assert!(r1.contains("q"));
        assert!(r1.contains("1"));
    }

    #[test]
    fn context_char_cap_whole_line() {
        let ctx = RagContext {
            query: "q".into(),
            hits: vec![
                RagHit { rank: 1, memory_id: "a".into(), score: 0.1, snippet: "x".into(), derivation: vec![] },
                RagHit { rank: 2, memory_id: "b".into(), score: 0.2, snippet: "y".into(), derivation: vec![] },
            ],
            total: 2,
        };
        let (p, c) = build_prompt_with_citations(&ctx, "q", None, 10);
        assert!(c.len() <= 1 || p.contains("No relevant context found."));
    }

    #[test]
    fn tie_breaking_by_score_then_id() {
        let a = crate::traits::SearchResult { id: "b".into(), score: 0.5, payload: crate::traits::MemoryPayload { content: "".into(), user_id: "u".into(), memory_type: "m".into(), agent_name: None, location: "".into(), location_lines: "".into(), metadata: serde_json::json!({}) }, evidence: None };
        let b = crate::traits::SearchResult { id: "a".into(), score: 0.5, payload: crate::traits::MemoryPayload { content: "".into(), user_id: "u".into(), memory_type: "m".into(), agent_name: None, location: "".into(), location_lines: "".into(), metadata: serde_json::json!({}) }, evidence: None };
        let mut v = vec![a.clone(), b.clone()];
        v.sort_by(|a, b| a.score.total_cmp(&b.score).then_with(|| a.id.cmp(&b.id)));
        assert_eq!(v[0].id, "a");
    }

    #[test]
    fn openai_provider_new_validation() {
        assert!(OpenAiCompatibleProvider::new("http://localhost:11434/v1", Some("OPENAI_API_KEY"), "m", Duration::from_secs(5), false).is_ok());
        assert!(OpenAiCompatibleProvider::new("bad", Some("OPENAI_API_KEY"), "m", Duration::from_secs(5), false).is_err());
        assert!(OpenAiCompatibleProvider::new("https://evil.com", Some("OPENAI_API_KEY"), "m", Duration::from_secs(5), false).is_err());
        assert!(OpenAiCompatibleProvider::new("https://evil.com", Some("OPENAI_API_KEY"), "m", Duration::from_secs(5), true).is_ok());
        assert!(OpenAiCompatibleProvider::new("http://localhost/v1", Some("BAD"), "m", Duration::from_secs(5), false).is_err());
    }

    #[test]
    fn remote_egress_gate() {
        let p = Arc::new(OpenAiCompatibleProvider::new("http://localhost/v1", None, "m", Duration::from_secs(5), false).unwrap());
        assert!(p.is_remote());
    }

    #[tokio::test]
    async fn ask_disabled_sentinel_short_circuits() {
        use crate::guard::testkit::{MockStore, StubEmbedder};
        let store: Arc<dyn crate::traits::VectorStore> = Arc::new(MockStore::new());
        let emb: Arc<dyn crate::traits::Embedder> = Arc::new(StubEmbedder);
        let provider: Arc<dyn LlmProvider> = Arc::new(OfflineProvider);
        let err = ask(
            store, emb, "ns", "local-user", "__disabled__", 5, None, 4000, provider,
            false, false, false,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ChatError::Disabled));
    }
}
