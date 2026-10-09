/// One storage mutation, as it reaches the bus.
///
/// A pulse is what the thalamus does with a signal: it carries the fact that
/// something changed and nothing else. No payload, no diff, no outcome -- a
/// subscriber that needs the memory itself reads it back through the store on
/// its [`SubscriberContext`](crate::events::SubscriberContext), rather than
/// being handed a second copy here.
///
/// `#[non_exhaustive]` because a new event kind should be an addition here and
/// a matched-and-ignored arm everywhere, never a breaking change to every
/// subscriber already written.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum ThalamicPulse {
    /// A memory was written for the first time. `kind` is its `memory_type`.
    ///
    /// `String` rather than `&'static str`: the kind is read out of a stored
    /// record and travels with it, so it has the same lifetime as the pulse.
    Created { id: String, namespace: String, kind: String },
    /// `old_id` gave way to `new_id`. Both are still stored; nothing was deleted.
    Superseded { old_id: String, new_id: String, namespace: String },
    /// A producer tombstoned a memory. It is archived, not destroyed.
    Archived { id: String, namespace: String },
    /// `neurostrata_guard_validate` returned a verdict. Nothing was stored and
    /// nothing changed -- but the call happened, and the audit trail for the
    /// deferred `--causal` export is a record of calls, so the pulse exists to
    /// say so.
    ///
    /// `payload_hash` is a 32-bit digest of the action's payload, not the
    /// payload. A guard call carries whatever the agent was about to do --
    /// file contents, shell arguments, credentials in a diff -- and this pulse
    /// is the thing most likely to be logged, exported and shipped, so the
    /// audit row is a size-bounded commitment ("this exact payload") rather
    /// than a second copy of the sensitive thing. Two calls with the same
    /// payload hash are the same payload far more often than they are
    /// collisions, and a collision costs an audit row's precision, not its
    /// existence.
    ///
    /// `rule_ids_triggered` is empty for an allow, and names the rules that
    /// fired otherwise. `trace_id` is the validator's own correlation id, so a
    /// row can be joined back to the response the caller received.
    GuardedActionFired {
        trace_id: String,
        action_type: String,
        payload_hash: u32,
        verdict: String,
        rule_ids_triggered: Vec<String>,
        namespace: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thalamic_pulse_variants_construct() {
        // Real assertions, not just `let _ =`. The test must catch a type change
        // (e.g. `String` -> `Cow`) silently slipping through.
        let c =
            ThalamicPulse::Created { id: "x".into(), namespace: "n".into(), kind: "task".into() };
        let s = ThalamicPulse::Superseded {
            old_id: "a".into(),
            new_id: "b".into(),
            namespace: "n".into(),
        };
        let a = ThalamicPulse::Archived { id: "x".into(), namespace: "n".into() };
        let g = ThalamicPulse::GuardedActionFired {
            trace_id: "t".into(),
            action_type: "shell".into(),
            payload_hash: 0xDEAD_BEEF,
            verdict: "deny".into(),
            rule_ids_triggered: vec!["r1".into(), "r2".into()],
            namespace: "n".into(),
        };
        assert!(matches!(c, ThalamicPulse::Created { .. }));
        assert!(matches!(s, ThalamicPulse::Superseded { .. }));
        assert!(matches!(a, ThalamicPulse::Archived { .. }));
        assert!(matches!(g, ThalamicPulse::GuardedActionFired { .. }));
        // Field-level checks: the strings are preserved, not silently dropped.
        match c {
            ThalamicPulse::Created { id, namespace, kind } => {
                assert_eq!(id, "x");
                assert_eq!(namespace, "n");
                assert_eq!(kind, "task");
            }
            _ => unreachable!(),
        }
        match s {
            ThalamicPulse::Superseded { old_id, new_id, namespace } => {
                assert_eq!(old_id, "a");
                assert_eq!(new_id, "b");
                assert_eq!(namespace, "n");
            }
            _ => unreachable!(),
        }
        match a {
            ThalamicPulse::Archived { id, namespace } => {
                assert_eq!(id, "x");
                assert_eq!(namespace, "n");
            }
            _ => unreachable!(),
        }
        match g {
            ThalamicPulse::GuardedActionFired {
                trace_id,
                action_type,
                payload_hash,
                verdict,
                rule_ids_triggered,
                namespace,
            } => {
                assert_eq!(trace_id, "t");
                assert_eq!(action_type, "shell");
                // The hash is a `u32`, so it cannot have widened into a `u64`
                // or a string without this failing.
                assert_eq!(payload_hash, 0xDEAD_BEEF_u32);
                assert_eq!(verdict, "deny");
                assert_eq!(rule_ids_triggered, vec!["r1".to_string(), "r2".to_string()]);
                assert_eq!(namespace, "n");
            }
            _ => unreachable!(),
        }
    }
}
