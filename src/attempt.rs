use serde_json::Value;

pub const ATTEMPT_KEYS: [&str; 4] = ["approach", "outcome", "failure_mode", "context"];
pub const FAILED_ATTEMPT_PREFIX: &str = "[FAILED ATTEMPT] ";

pub fn attempt_defects(metadata: &Value) -> Vec<String> {
    let attempt = match metadata.get("attempt") {
        None => return Vec::new(),
        Some(v) => v,
    };

    let mut defects: Vec<String> = Vec::new();
    let fields = match attempt.as_object() {
        Some(o) => o,
        None => {
            defects.push("must be an object".to_string());
            return defects;
        }
    };

    for key in ATTEMPT_KEYS {
        match fields.get(key) {
            None => defects.push(format!("is missing the required key {:?}", key)),
            Some(Value::String(s)) => {
                if s.trim().is_empty() {
                    defects.push(format!("key {:?} must be a non-empty string", key));
                } else if key == "outcome" && s != "failed" {
                    defects.push(format!("key {:?} must be \"failed\"", key));
                }
            }
            Some(_) => defects.push(format!("key {:?} must be a string", key)),
        }
    }

    // outcome check only applies when present as string; handled above.
    defects
}

pub fn is_failed_attempt(metadata: &Value) -> bool {
    metadata.get("attempt").is_some() && attempt_defects(metadata).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_well_formed_failed_attempt_is_a_failed_attempt() {
        let metadata = serde_json::json!({
            "attempt": {
                "approach": "retry with backoff",
                "outcome": "failed",
                "failure_mode": "timeout",
                "context": "under load"
            }
        });
        assert!(is_failed_attempt(&metadata));
        assert!(attempt_defects(&metadata).is_empty());
    }

    #[test]
    fn a_malformed_attempt_is_neither_a_failed_attempt_nor_defect_free() {
        // missing key
        let missing = serde_json::json!({
            "attempt": {
                "approach": "a",
                "outcome": "failed",
                "failure_mode": "b"
            }
        });
        assert!(!is_failed_attempt(&missing));
        assert!(!attempt_defects(&missing).is_empty());

        // empty string
        let empty = serde_json::json!({
            "attempt": {
                "approach": "",
                "outcome": "failed",
                "failure_mode": "b",
                "context": "c"
            }
        });
        assert!(!is_failed_attempt(&empty));
        assert!(!attempt_defects(&empty).is_empty());

        // non-string
        let nonstr = serde_json::json!({
            "attempt": {
                "approach": 42,
                "outcome": "failed",
                "failure_mode": "b",
                "context": "c"
            }
        });
        assert!(!is_failed_attempt(&nonstr));
        assert!(!attempt_defects(&nonstr).is_empty());

        // outcome other than failed
        let badoutcome = serde_json::json!({
            "attempt": {
                "approach": "a",
                "outcome": "succeeded",
                "failure_mode": "b",
                "context": "c"
            }
        });
        assert!(!is_failed_attempt(&badoutcome));
        assert!(!attempt_defects(&badoutcome).is_empty());
    }

    #[test]
    fn an_absent_attempt_is_not_a_failed_attempt() {
        let metadata = serde_json::json!({});
        assert!(!is_failed_attempt(&metadata));
        assert!(attempt_defects(&metadata).is_empty());
    }

    #[test]
    fn not_an_object_is_a_defect() {
        let metadata = serde_json::json!({ "attempt": "nope" });
        assert!(!attempt_defects(&metadata).is_empty());
        assert_eq!(attempt_defects(&metadata)[0], "must be an object");
    }

    #[test]
    fn whitespace_string_is_a_defect() {
        let metadata = serde_json::json!({
            "attempt": {
                "approach": "   ",
                "outcome": "failed",
                "failure_mode": "b",
                "context": "c"
            }
        });
        assert!(!attempt_defects(&metadata).is_empty());
    }
}
