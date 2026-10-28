use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LanguageSchema {
    pub extensions: Vec<String>,
    pub queries: HashMap<String, String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ParserSchema {
    pub languages: HashMap<String, LanguageSchema>,
}

impl ParserSchema {
    pub fn load(config_str: &str) -> Result<Self, anyhow::Error> {
        let schema: ParserSchema = serde_json::from_str(config_str)?;
        Ok(schema)
    }

    /// Stable identity of the parser configuration.
    ///
    /// The config is a `HashMap`, so its iteration order (and therefore the
    /// raw file bytes) is not stable. The fingerprint is taken over a canonical
    /// form instead: languages sorted by name, extensions normalized and
    /// sorted, queries sorted by key. Two semantically identical schemas then
    /// hash identically, and only a real change moves the fingerprint.
    pub fn fingerprint(&self) -> String {
        let mut languages = Map::new();
        let mut names: Vec<&String> = self.languages.keys().collect();
        names.sort();
        for name in names {
            let language = &self.languages[name];
            let mut extensions: Vec<String> = language
                .extensions
                .iter()
                .map(|extension| extension.trim_start_matches('.').to_lowercase())
                .collect();
            extensions.sort();
            let mut queries = Map::new();
            let mut keys: Vec<&String> = language.queries.keys().collect();
            keys.sort();
            for key in keys {
                queries.insert(key.clone(), Value::String(language.queries[key].clone()));
            }
            let mut entry = Map::new();
            entry.insert(
                "extensions".to_string(),
                Value::Array(extensions.into_iter().map(Value::String).collect()),
            );
            entry.insert("queries".to_string(), Value::Object(queries));
            languages.insert(name.clone(), Value::Object(entry));
        }
        let mut canonical = Map::new();
        canonical.insert("languages".to_string(), Value::Object(languages));
        let bytes = serde_json::to_vec(&Value::Object(canonical))
            .expect("the canonical form is JSON-serializable by construction");
        hex::encode(Sha256::digest(&bytes))
    }
}
