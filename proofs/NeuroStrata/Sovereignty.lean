namespace NeuroStrata.Sovereignty

inductive SovereigntyDecision where
  | allow
  | forbiddenMemoryType
  | notShareable
  | forbiddenRuntimeCounter
  | unauthorizedRecipient
  deriving DecidableEq, Repr

def SovereigntyDecision.asTag : SovereigntyDecision → String
  | .allow => "allow"
  | .forbiddenMemoryType => "forbidden_memory_type"
  | .notShareable => "not_shareable"
  | .forbiddenRuntimeCounter => "forbidden_runtime_counter"
  | .unauthorizedRecipient => "unauthorized_recipient"

@[simp]
def evaluateSovereigntyGate
    (permittedMemoryType isShareable hasRuntimeCounter aclEmpty recipientInAcl : Bool)
    : SovereigntyDecision :=
  if !permittedMemoryType then .forbiddenMemoryType
  else if !isShareable then .notShareable
  else if hasRuntimeCounter then .forbiddenRuntimeCounter
  else if !aclEmpty && !recipientInAcl then .unauthorizedRecipient
  else .allow

inductive ImportPrecheck where
  | freshInsert
  | skipSameUpdateId
  | refusePrivateSovereign
  | proceedMeshMerge
  deriving DecidableEq, Repr

def ImportPrecheck.asTag : ImportPrecheck → String
  | .freshInsert => "fresh_insert"
  | .skipSameUpdateId => "skip_same_update_id"
  | .refusePrivateSovereign => "refuse_private_sovereign"
  | .proceedMeshMerge => "proceed_mesh_merge"

@[simp]
def evaluateImportPrecheck (rowExists hasMeshProvenance sameUpdateId : Bool) : ImportPrecheck :=
  if !rowExists then .freshInsert
  else if !hasMeshProvenance then .refusePrivateSovereign
  else if sameUpdateId then .skipSameUpdateId
  else .proceedMeshMerge

theorem private_never_egresses :
    ∀ (perm counter aclEmpty inAcl : Bool),
      evaluateSovereigntyGate perm false counter aclEmpty inAcl ≠ .allow := by
  intro perm counter aclEmpty inAcl
  cases perm <;> cases counter <;> cases aclEmpty <;> cases inAcl <;> decide

theorem runtime_counters_never_cross_wire :
    ∀ (perm shareable aclEmpty inAcl : Bool),
      evaluateSovereigntyGate perm shareable true aclEmpty inAcl ≠ .allow := by
  intro perm shareable aclEmpty inAcl
  cases perm <;> cases shareable <;> cases aclEmpty <;> cases inAcl <;> decide

theorem unlisted_recipient_blocked_when_acl_nonempty :
    ∀ (perm shareable counter : Bool),
      evaluateSovereigntyGate perm shareable counter false false ≠ .allow := by
  intro perm shareable counter
  cases perm <;> cases shareable <;> cases counter <;> decide

theorem private_local_row_never_overwritten :
    ∀ (sameUpd : Bool),
      evaluateImportPrecheck true false sameUpd = .refusePrivateSovereign := by
  intro sameUpd
  cases sameUpd <;> rfl

end NeuroStrata.Sovereignty