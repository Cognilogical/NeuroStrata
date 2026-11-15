namespace NeuroStrata.DataConnector

inductive ConnectorOp where
  | registerPointer
  | resolveSlice
  | applyWriteBack
  deriving DecidableEq, Repr

def ConnectorOp.asTag : ConnectorOp → String
  | .registerPointer => "register_pointer"
  | .resolveSlice => "resolve_slice"
  | .applyWriteBack => "apply_write_back"

inductive ActionReceiptState where
  | notRequired
  | activeBound
  | missingOrNotFound
  | revertedOrSuperseded
  | targetMismatch
  deriving DecidableEq, Repr

def ActionReceiptState.asStr : ActionReceiptState → String
  | .notRequired => "not_required"
  | .activeBound => "active_bound"
  | .missingOrNotFound => "missing_or_not_found"
  | .revertedOrSuperseded => "reverted_or_superseded"
  | .targetMismatch => "target_mismatch"

inductive ConnectorPreVerdict where
  | allowPreRegister
  | allowPreResolve
  | allowPreWriteBack
  | denyUnregisteredConnector
  | denyRebacAccess
  | denyReadOnlyPool
  | denyInvalidActionReceipt
  | denySliceLimitExceeded
  | denyClockSkewFuture
  | denyPointerStale
  deriving DecidableEq, Repr

def ConnectorPreVerdict.asTag : ConnectorPreVerdict → String
  | .allowPreRegister => "allow_pre_register"
  | .allowPreResolve => "allow_pre_resolve"
  | .allowPreWriteBack => "allow_pre_write_back"
  | .denyUnregisteredConnector => "deny_unregistered_connector"
  | .denyRebacAccess => "deny_rebac_access"
  | .denyReadOnlyPool => "deny_read_only_pool"
  | .denyInvalidActionReceipt => "deny_invalid_action_receipt"
  | .denySliceLimitExceeded => "deny_slice_limit_exceeded"
  | .denyClockSkewFuture => "deny_clock_skew_future"
  | .denyPointerStale => "deny_pointer_stale"

@[simp]
def evaluateConnectorPreGate
    (connectorRegistered rebacAllowed poolWritable clockSkewFuture : Bool)
    (op : ConnectorOp)
    (receiptState : ActionReceiptState)
    (requestedRows maxSliceRows requestedBytes maxSliceBytes : Nat)
    (ageSecs maxStalenessSecs : Nat)
    : ConnectorPreVerdict :=
  if !connectorRegistered then .denyUnregisteredConnector
  else if !rebacAllowed then .denyRebacAccess
  else if op = .registerPointer then .allowPreRegister
  else if op = .applyWriteBack && !poolWritable then .denyReadOnlyPool
  else if op = .applyWriteBack && receiptState ≠ .activeBound then .denyInvalidActionReceipt
  else if op = .resolveSlice &&
      (requestedRows == 0 || requestedRows > maxSliceRows ||
       requestedBytes == 0 || requestedBytes > maxSliceBytes) then .denySliceLimitExceeded
  else if clockSkewFuture then .denyClockSkewFuture
  else if maxStalenessSecs > 0 && ageSecs > maxStalenessSecs then .denyPointerStale
  else
    match op with
    | .resolveSlice => .allowPreResolve
    | .applyWriteBack => .allowPreWriteBack
    | _ => .allowPreResolve

theorem evaluateConnectorPreGate_deterministic
    (cr ra pw csf : Bool) (op : ConnectorOp) (rs : ActionReceiptState)
    (rr sr rb sb ag ms : Nat) :
    evaluateConnectorPreGate cr ra pw csf op rs rr sr rb sb ag ms =
    evaluateConnectorPreGate cr ra pw csf op rs rr sr rb sb ag ms := by
  rfl

theorem unregistered_always_denied
    (ra pw csf : Bool) (op : ConnectorOp) (rs : ActionReceiptState)
    (rr sr rb sb ag ms : Nat) :
    evaluateConnectorPreGate false ra pw csf op rs rr sr rb sb ag ms =
      .denyUnregisteredConnector := by
  rfl

theorem rebac_denied_always_denied
    (pw csf : Bool) (op : ConnectorOp) (rs : ActionReceiptState)
    (rr sr rb sb ag ms : Nat) :
    evaluateConnectorPreGate true false pw csf op rs rr sr rb sb ag ms =
      .denyRebacAccess := by
  rfl

theorem readonly_never_allows_writeback
    (cr ra csf : Bool) (rs : ActionReceiptState)
    (rr sr rb sb ag ms : Nat) :
    evaluateConnectorPreGate cr ra false csf .applyWriteBack rs rr sr rb sb ag ms ≠
      .allowPreWriteBack := by
  cases cr <;> cases ra <;> simp [evaluateConnectorPreGate]

theorem writeback_requires_active_bound_receipt
    (cr ra pw csf : Bool) (rs : ActionReceiptState)
    (rr sr rb sb ag ms : Nat) :
    (evaluateConnectorPreGate cr ra pw csf .applyWriteBack rs rr sr rb sb ag ms =
      .allowPreWriteBack) →
    rs = .activeBound := by
  intro h
  cases cr <;> cases ra <;> cases pw <;> cases csf <;> cases rs <;>
    simp [evaluateConnectorPreGate] at h ⊢

theorem future_clock_denied
    (rr sr rb sb ag ms : Nat) :
    evaluateConnectorPreGate true true true true .applyWriteBack .activeBound
      rr sr rb sb ag ms = .denyClockSkewFuture := by
  rfl

theorem stale_pointer_denied
    (rr sr rb sb : Nat) (ag ms : Nat)
    (hpos : ms > 0) (hstale : ag > ms) :
    evaluateConnectorPreGate true true true false .applyWriteBack .activeBound
      rr sr rb sb ag ms = .denyPointerStale := by
  simp [evaluateConnectorPreGate, hpos, hstale]

inductive ConnectorPostVerdict where
  | allowPostOutput
  | denyObservedBoundsExceeded
  | denySourceDigestMismatch
  | denyCasConflict
  deriving DecidableEq, Repr

def ConnectorPostVerdict.asTag : ConnectorPostVerdict → String
  | .allowPostOutput => "allow_post_output"
  | .denyObservedBoundsExceeded => "deny_observed_bounds_exceeded"
  | .denySourceDigestMismatch => "deny_source_digest_mismatch"
  | .denyCasConflict => "deny_cas_conflict"

@[simp]
def evaluateConnectorPostGate
    (op : ConnectorOp)
    (observedRows maxSliceRows observedBytes maxSliceBytes : Nat)
    (sourceDigestMatches casExpectedMatches : Bool)
    : ConnectorPostVerdict :=
  if op = .resolveSlice &&
      (observedRows > maxSliceRows || observedBytes > maxSliceBytes) then
    .denyObservedBoundsExceeded
  else if !sourceDigestMatches then .denySourceDigestMismatch
  else if op = .applyWriteBack && !casExpectedMatches then .denyCasConflict
  else .allowPostOutput

theorem evaluateConnectorPostGate_deterministic
    (op : ConnectorOp) (orw sr ob sb : Nat) (sdm cem : Bool) :
    evaluateConnectorPostGate op orw sr ob sb sdm cem =
    evaluateConnectorPostGate op orw sr ob sb sdm cem := by
  rfl

theorem post_digest_mismatch_denied
    (op : ConnectorOp) (orw sr ob sb : Nat) (cem : Bool)
    (hrows : orw ≤ sr) (hbytes : ob ≤ sb) :
    evaluateConnectorPostGate op orw sr ob sb false cem =
      .denySourceDigestMismatch := by
  cases op <;>
    simp [evaluateConnectorPostGate, Nat.not_lt.mpr hrows, Nat.not_lt.mpr hbytes]

theorem post_cas_conflict_denied
    (orw sr ob sb : Nat) (sdm : Bool) :
    evaluateConnectorPostGate .applyWriteBack orw sr ob sb sdm false =
      (if !sdm then .denySourceDigestMismatch else .denyCasConflict) := by
  rfl

theorem post_cas_conflict_denied_of_digest
    (orw sr ob sb : Nat) :
    evaluateConnectorPostGate .applyWriteBack orw sr ob sb true false =
      .denyCasConflict := by
  rfl

theorem post_observed_bounds_denied
    (orw sr ob sb : Nat) (sdm cem : Bool)
    (hrows : orw > sr) :
    evaluateConnectorPostGate .resolveSlice orw sr ob sb sdm cem =
      .denyObservedBoundsExceeded := by
  simp [evaluateConnectorPostGate, hrows]

end NeuroStrata.DataConnector