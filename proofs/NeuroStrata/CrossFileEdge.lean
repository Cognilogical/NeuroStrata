/-
  NeuroStrata.CrossFileEdge
  Formal verification of Tier-1 Cross-File Call Resolution (`neurostrata-rbde`).
  Mirrors `resolve_cross_file_call` in `src/formal_kernel.rs`.
-/

namespace NeuroStrata

inductive CrossFileResolution where
  | sameFile
  | importedFile
  | workspaceUnique
  | unresolvedSelfCall
  | unresolvedAmbiguousOrStd
  deriving DecidableEq, Repr

def CrossFileResolution.asTag : CrossFileResolution → String
  | .sameFile => "SAME_FILE"
  | .importedFile => "IMPORTED_FILE"
  | .workspaceUnique => "WORKSPACE_UNIQUE"
  | .unresolvedSelfCall => "UNRESOLVED_SELF_CALL"
  | .unresolvedAmbiguousOrStd => "UNRESOLVED_AMBIGUOUS_OR_STD"

def resolveCrossFileCall
    (isSelfCall : Bool)
    (inSameFile : Bool)
    (inImportedFile : Bool)
    (workspaceMatchCount : Nat)
    (identLen : Nat)
    (isCommonStd : Bool) : CrossFileResolution :=
  if isSelfCall then
    .unresolvedSelfCall
  else if inSameFile then
    .sameFile
  else if inImportedFile then
    .importedFile
  else if workspaceMatchCount == 1 && decide (identLen ≥ 4) && !isCommonStd then
    .workspaceUnique
  else
    .unresolvedAmbiguousOrStd

theorem cross_file_never_self_loops
    (inSameFile inImportedFile : Bool)
    (workspaceMatchCount identLen : Nat)
    (isCommonStd : Bool) :
    resolveCrossFileCall true inSameFile inImportedFile workspaceMatchCount identLen isCommonStd =
      .unresolvedSelfCall := by
  rfl

theorem cross_file_same_file_takes_priority
    (inImportedFile : Bool)
    (workspaceMatchCount identLen : Nat)
    (isCommonStd : Bool) :
    resolveCrossFileCall false true inImportedFile workspaceMatchCount identLen isCommonStd =
      .sameFile := by
  rfl

theorem cross_file_workspace_fallback_requires_unique_non_std
    (isSelfCall inSameFile inImportedFile : Bool)
    (workspaceMatchCount identLen : Nat)
    (isCommonStd : Bool)
    (h :
      resolveCrossFileCall isSelfCall inSameFile inImportedFile workspaceMatchCount identLen isCommonStd =
        .workspaceUnique) :
    isSelfCall = false ∧
    inSameFile = false ∧
    inImportedFile = false ∧
    workspaceMatchCount = 1 ∧
    identLen ≥ 4 ∧
    isCommonStd = false := by
  unfold resolveCrossFileCall at h
  cases isSelfCall <;> simp at h
  cases inSameFile <;> simp at h
  cases inImportedFile <;> simp at h
  cases isCommonStd <;> simp at h
  exact ⟨rfl, rfl, rfl, h.1, h.2, rfl⟩

end NeuroStrata
