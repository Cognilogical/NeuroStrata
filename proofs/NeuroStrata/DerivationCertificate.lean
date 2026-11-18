import NeuroStrata.TaskMachine

namespace NeuroStrata.DerivationCertificate

/-- OWL 2 RL / RDFS rule identifiers for derivation certificates.
    Mirrors every rule the Rust adapter can emit. -/
inductive Owl2RlRuleId where
  | prpDom | prpRng | prpTrp | prpSymp | prpInv | prpSpo1 | prpSpo2 | prpEqp
  | caxSco | caxEqc | clsHv | clsSvf | clsAvf | caxDwViolation
  | owlUniverse | scmEqc | rdfsScoTrans | eqSym | eqTrans | eqRepS | eqRepP | eqRepO
  | clsInt1 | clsInt2 | clsUni | rdfsContainer
  deriving DecidableEq, Repr

def Owl2RlRuleId.asStr : Owl2RlRuleId → String
  | .prpDom => "prp-dom"
  | .prpRng => "prp-rng"
  | .prpTrp => "prp-trp"
  | .prpSymp => "prp-symp"
  | .prpInv => "prp-inv"
  | .prpSpo1 => "prp-spo1"
  | .prpSpo2 => "prp-spo2"
  | .prpEqp => "prp-eqp"
  | .caxSco => "cax-sco"
  | .caxEqc => "cax-eqc"
  | .clsHv => "cls-hv"
  | .clsSvf => "cls-svf"
  | .clsAvf => "cls-avf"
  | .caxDwViolation => "cax-dw"
  | .owlUniverse => "owl-universe"
  | .scmEqc => "scm-eqc"
  | .rdfsScoTrans => "rdfs11-subClassOf"
  | .eqSym => "eq-sym"
  | .eqTrans => "eq-trans"
  | .eqRepS => "eq-rep-s"
  | .eqRepP => "eq-rep-p"
  | .eqRepO => "eq-rep-o"
  | .clsInt1 => "cls-int1"
  | .clsInt2 => "cls-int2"
  | .clsUni => "cls-uni"
  | .rdfsContainer => "rdfs-container-membership"

/-- Minimum number of premises needed to fire this rule. -/
def minPremiseCount (rule : Owl2RlRuleId) : Nat :=
  match rule with
  | .owlUniverse => 0
  | .scmEqc | .eqSym | .rdfsContainer => 1
  | .prpTrp | .prpSpo2 | .clsHv | .clsSvf | .caxDwViolation => 3
  | .clsAvf | .clsInt1 | .clsInt2 | .clsUni => 4
  | _ => 2

@[simp]
def expectedPremiseCount (rule : Owl2RlRuleId) : Nat := minPremiseCount rule

/-- Verification verdict for a single derivation step. -/
inductive StepVerdict where
  | valid | invalidRule | premiseCountMismatch | invalidPremises | invalidConclusion
  deriving DecidableEq, Repr

def StepVerdict.asStr : StepVerdict → String
  | .valid => "valid"
  | .invalidRule => "invalid_rule"
  | .premiseCountMismatch => "premise_count_mismatch"
  | .invalidPremises => "invalid_premises"
  | .invalidConclusion => "invalid_conclusion"

/-- Verify a single derivation step. Premise count must be >= the rule minimum. -/
@[simp]
def verifyDerivationStepStructure (rule : Owl2RlRuleId) (premiseCount : Nat) : StepVerdict :=
  if premiseCount >= minPremiseCount rule then .valid else .premiseCountMismatch

/-- Certificate-chain verification verdict. -/
inductive CertVerdict where
  | valid | unknownRule | brokenChain | missingProof | hashMismatch
  deriving DecidableEq, Repr

def CertVerdict.asStr : CertVerdict → String
  | .valid => "valid"
  | .unknownRule => "unknown_rule"
  | .brokenChain => "broken_chain"
  | .missingProof => "missing_proof"
  | .hashMismatch => "hash_mismatch"

def MAX_CERT_STEPS : Nat := 64

/-- Walk numbered steps. Missing entries default to `false` (fail-closed). -/
def chainCheckFrom
    (i : Nat) (fuel : Nat)
    (ruleValidity : List Bool)
    (premiseCountOk : List Bool)
    (chainContinuity : List Bool) : CertVerdict :=
  match fuel with
  | 0 => .valid
  | n + 1 =>
      if ruleValidity.getD i false = false then .unknownRule
      else if premiseCountOk.getD i false = false || chainContinuity.getD i false = false then
        .brokenChain
      else
        chainCheckFrom (i + 1) n ruleValidity premiseCountOk chainContinuity

/-- Chain verification (legacy window). -/
@[simp]
def verifyCertificateChain
    (stepCount : Nat)
    (ruleValidity : List Bool)
    (premiseCountOk : List Bool)
    (chainContinuity : List Bool)
    (allInferredHaveProof : Bool) : CertVerdict :=
  if !allInferredHaveProof then .missingProof
  else if stepCount > MAX_CERT_STEPS then .brokenChain
  else chainCheckFrom 0 stepCount ruleValidity premiseCountOk chainContinuity

/-- Certificate hash verification result. -/
inductive HashVerdict where | valid | mismatch deriving DecidableEq, Repr

def HashVerdict.asStr : HashVerdict → String
  | .valid => "valid"
  | .mismatch => "mismatch"

@[simp]
def verifyCertificateHash (expectedHash : UInt64) (actualHash : UInt64) : HashVerdict :=
  if expectedHash = actualHash then .valid else .mismatch

/-- Full certificate verdict (mirrors Rust KernelCertificateVerdict). -/
inductive CertificateVerdict where
  | valid | invalidProfile | digestMismatch | missingProof
  | unknownRule | premiseCountMismatch | brokenChain
  deriving DecidableEq, Repr

def CertificateVerdict.asStr : CertificateVerdict → String
  | .valid => "valid"
  | .invalidProfile => "invalid_profile"
  | .digestMismatch => "digest_mismatch"
  | .missingProof => "missing_proof"
  | .unknownRule => "unknown_rule"
  | .premiseCountMismatch => "premise_count_mismatch"
  | .brokenChain => "broken_chain"

/-- A step witness: rule tag (255 = unknown), premise count, premises available. -/
structure CertificateStep where
  ruleTag : Nat
  premiseCount : Nat
  premisesAvailable : Bool
  deriving DecidableEq, Repr

/-- Resolve a rule tag (canonical order); 255 means unknown. -/
def ruleFromTag : Nat → Option Owl2RlRuleId
  | 0 => some .prpDom | 1 => some .prpRng | 2 => some .prpTrp | 3 => some .prpSymp
  | 4 => some .prpInv | 5 => some .prpSpo1 | 6 => some .prpSpo2 | 7 => some .prpEqp
  | 8 => some .caxSco | 9 => some .caxEqc | 10 => some .clsHv | 11 => some .clsSvf
  | 12 => some .clsAvf | 13 => some .caxDwViolation | 14 => some .owlUniverse
  | 15 => some .scmEqc | 16 => some .rdfsScoTrans | 17 => some .eqSym | 18 => some .eqTrans
  | 19 => some .eqRepS | 20 => some .eqRepP | 21 => some .eqRepO | 22 => some .clsInt1
  | 23 => some .clsInt2 | 24 => some .clsUni | 25 => some .rdfsContainer
  | _ => none

/-- Walk every step of a full certificate; no 64-step bypass. -/
def fullCheckFrom
    (i : Nat) (steps : List CertificateStep) : CertificateVerdict :=
  match steps with
  | [] => .valid
  | st :: rest =>
      match ruleFromTag st.ruleTag with
      | none => .unknownRule
      | some r =>
          if st.premiseCount < minPremiseCount r then .premiseCountMismatch
          else if !st.premisesAvailable then .brokenChain
          else fullCheckFrom (i + 1) rest

/-- Independently verify a complete derivation certificate.
    profile: 0 = rdfs, 1 = owl-rl. -/
@[simp]
def verifyDerivationCertificate
    (profile : Nat) (digestMatches : Bool)
    (steps : List CertificateStep) (allInferredHaveProof : Bool) : CertificateVerdict :=
  if profile > 1 then .invalidProfile
  else if !digestMatches then .digestMismatch
  else if !allInferredHaveProof then .missingProof
  else fullCheckFrom 0 steps

theorem verify_step_at_minimum
    (rule : Owl2RlRuleId) (pc : Nat)
    (h : pc >= minPremiseCount rule) :
    verifyDerivationStepStructure rule pc = .valid := by
  unfold verifyDerivationStepStructure
  simp only [if_pos h]

theorem verify_step_below_minimum
    (rule : Owl2RlRuleId) (pc : Nat)
    (h : ¬pc >= minPremiseCount rule) :
    verifyDerivationStepStructure rule pc = .premiseCountMismatch := by
  unfold verifyDerivationStepStructure
  simp only [if_neg h]

theorem cert_chain_missing_proof
    (stepCount : Nat) (rv pc cc : List Bool) :
    verifyCertificateChain stepCount rv pc cc false = .missingProof := by
  simp [verifyCertificateChain]

theorem cert_chain_broken_chain
    (rv pc cc : List Bool) :
    verifyCertificateChain 1 (true :: rv) (false :: pc) cc true = .brokenChain := by
  unfold verifyCertificateChain MAX_CERT_STEPS
  simp [chainCheckFrom]

theorem cert_chain_short_list_fails_closed
    (cc : List Bool) :
    verifyCertificateChain 1 [] [] cc true = .unknownRule := by
  unfold verifyCertificateChain MAX_CERT_STEPS
  simp [chainCheckFrom]

theorem full_cert_valid_sco :
    verifyDerivationCertificate 1 true [⟨8, 2, true⟩] true = .valid := by
  decide

theorem full_cert_valid_over_64_steps :
    verifyDerivationCertificate 1 true (List.replicate 70 ⟨8, 2, true⟩) true = .valid := by
  decide

theorem full_cert_unknown_rule :
    verifyDerivationCertificate 1 true [⟨255, 2, true⟩] true = .unknownRule := by
  decide

theorem full_cert_premise_mismatch :
    verifyDerivationCertificate 1 true [⟨8, 1, true⟩] true = .premiseCountMismatch := by
  decide

theorem full_cert_broken_chain :
    verifyDerivationCertificate 1 true [⟨8, 2, false⟩] true = .brokenChain := by
  decide

theorem full_cert_digest_mismatch :
    verifyDerivationCertificate 1 false [⟨8, 2, true⟩] true = .digestMismatch := by
  decide

theorem full_cert_invalid_profile :
    verifyDerivationCertificate 2 true [⟨8, 2, true⟩] true = .invalidProfile := by
  decide

theorem full_cert_missing_proof :
    verifyDerivationCertificate 1 true [⟨8, 2, true⟩] false = .missingProof := by
  decide

theorem verify_hash_match (h : UInt64) :
    verifyCertificateHash h h = .valid := by
  simp [verifyCertificateHash]

theorem verify_hash_mismatch (h1 h2 : UInt64) (h : h1 ≠ h2) :
    verifyCertificateHash h1 h2 = .mismatch := by
  simp [verifyCertificateHash, h]

end NeuroStrata.DerivationCertificate
