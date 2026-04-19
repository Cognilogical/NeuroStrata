use serde::{Deserialize, Serialize};
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
}
