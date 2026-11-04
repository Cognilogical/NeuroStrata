# Prefrontal Cortex — Semantic Action Guard & Behavioral Validation

> **Why you care:** Prompt instructions like *"Never run docker"* or *"Never drop production tables"* fail the moment context compacts. The Prefrontal Cortex turns stored architectural rules into an active interceptor (`neurostrata_validate_action`) that evaluates candidate agent actions semantically and returns a deterministic verdict before execution.

---

## 1. How the Prefrontal Cortex Works

Implemented in `src/guard/mod.rs`, the Prefrontal Cortex acts as the inhibitory control center of NeuroStrata. Before an agent executes a shell command, file mutation, or infrastructure operation, the harness plugin or agent calls `neurostrata_validate_action`:

```text
Candidate Action ("docker compose up -d")
       │
       ▼
┌──────────────────────────────────────────────────────────┐
│ 1. FastEmbed ONNX Vector Encoding + Lexical Pattern Scan │
├──────────────────────────────────────────────────────────┤
│ 2. Match Against Active `rule` Nodes & `guard_rule` Spec │
├──────────────────────────────────────────────────────────┤
│ 3. Optional Podman-Sandboxed Dry-Run Verification        │
└──────────────────────────────────────────────────────────┘
       │
       ▼
Named Verdict: ALLOW | WARN | BLOCK (with governing Rule ID & rationale)
```

---

## 2. Structured `guard_rule` Metadata

Any `rule` memory in the `global` or project namespace can carry structured `guard_rule` metadata that upgrades it from passive documentation into an enforced guard (`ENFORCED`):

```json
{
  "name": "neurostrata_add_memory",
  "arguments": {
    "namespace": "global",
    "memory_type": "rule",
    "content": "Always use podman and podman-compose; never invoke docker.",
    "metadata": {
      "enforcement": "ENFORCED",
      "guard_rule": {
        "action_pattern": "docker(\\s+|-compose)",
        "verdict": "block",
        "reason": "Docker is prohibited by global infrastructure policy; use podman or podman-compose."
      }
    }
  }
}
```

### Honesty Verification in `task validate`
NeuroStrata prevents "paper rules": if a memory claims `enforcement: "ENFORCED"` in its metadata without a wired guard specification, `neurostrata-mcp task validate` flags it as `rule_overclaims_enforcement`.

---

## 3. Dual-Stage Evaluation: Pattern + Semantic Similarity

The Prefrontal Cortex combines two complementary checks:
1. **Deterministic Pattern & Command Matching:** Instantly catches banned binaries, destructive flags (`rm -rf /`, `git push --force`, `DROP TABLE`), and protected file paths.
2. **Semantic Vector Similarity:** Encodes the candidate action description and compares it against high-salience `rule` embeddings in LadybugDB. Even if an action rephrases a prohibited operation, semantic proximity surfaces the governing constraint.

---

## 4. Podman-Sandboxed Verification

For high-risk operations or self-reinforced test checks, the guard subsystem integrates with rootless **Podman** containers to verify command behavior in an ephemeral sandbox without exposing the host filesystem or network.
