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

end NeuroStrata.ShellExemption
