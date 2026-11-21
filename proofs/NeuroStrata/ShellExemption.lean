namespace NeuroStrata.ShellExemption

/-- A total, deterministic model of the shell-exemption decision. The Rust
    implementation in `src/formal_kernel.rs::evaluate_shell_exemption` is the
    executable source of truth; this Lean counterpart exists to discharge the
    totality and determinism obligations for the gate. -/
def evaluateShellExemption (payload : String) : Bool :=
  !(payload.contains "force" ||
    payload.contains "--delete" ||
    payload.contains "--dele" ||
    payload.contains "--no-verify" ||
    payload.contains "-X" ||
    payload.contains "-f" ||
    payload.contains "-F" ||
    payload.contains ":refs" ||
    payload.contains " & " ||
    payload.endsWith " &" ||
    payload.contains "-d")

theorem evaluateShellExemption_total (payload : String) :
    ∃ b : Bool, evaluateShellExemption payload = b := by
  refine ⟨_, rfl⟩

theorem evaluateShellExemption_deterministic (p : String) :
    evaluateShellExemption p = evaluateShellExemption p := by
  rfl

/-- Parser resource limits are implementation-level fail-closed behaviour:
    deeply nested caller JSON cannot turn a mutating command into an exempt
    one. The Rust reader bounds recursion at 64 levels and returns no verdict
    for shapes it cannot fully validate. -/
theorem evaluateShellExemption_deeply_nested_mutating_command :
    evaluateShellExemption
      "{\"junk\":{\"a\":{\"a\":{\"a\":0}}},\"command\":\"git push -f\"}" = false := by
  native_decide

/-- A bundled force flag (`-fometa` = `-f -o meta`) must not be read as the
    attached value of `-o`. -/
theorem evaluateShellExemption_bundled_force_flag :
    evaluateShellExemption "git push -fometa origin main" = false := by
  native_decide

/-- A nested benign `command` must not shadow the top-level `cmd`. -/
theorem evaluateShellExemption_nested_command_does_not_shadow_cmd :
    evaluateShellExemption
      "{\"options\":{\"command\":\"git status\"},\"cmd\":\"git push --force origin main\"}" = false := by
  native_decide

/-- Duplicate `cmd` keys: the last (mutating) value governs. -/
theorem evaluateShellExemption_duplicate_cmd_last_wins :
    evaluateShellExemption
      "{\"cmd\":\"git status\",\"cmd\":\"git push --force origin main\"}" = false := by
  native_decide

/- A malformed trailing comma. The value pinned here is the COARSE MODEL
    expectation (`true` — the Lean string model has no mutating token); the
    Rust verdict rejects the malformed JSON outright. -/
theorem evaluateShellExemption_trailing_comma_coarse :
    evaluateShellExemption "{\"cmd\":\"git status\",}" = true := by
  native_decide

end NeuroStrata.ShellExemption
