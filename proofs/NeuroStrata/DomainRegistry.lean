namespace NeuroStrata.DomainRegistry

structure DomainEntry where
  slug : Nat
  parent : Option Nat
  aliases : List Nat
  deriving DecidableEq, Repr

def findEntry (slug : Nat) (entries : List DomainEntry) : Option DomainEntry :=
  entries.find? (fun e => e.slug == slug)

def allDistinct : List Nat → Bool
  | [] => true
  | x :: xs => !(xs.elem x) && allDistinct xs

def slugsUnique (entries : List DomainEntry) : Bool :=
  allDistinct (entries.map (·.slug))

def parentDeclared (entries : List DomainEntry) : Bool :=
  entries.all (fun e =>
    match e.parent with
    | none => true
    | some p => entries.any (fun d => d.slug == p))

def reachesRoot : Nat → List Nat → Nat → List DomainEntry → Bool
  | 0, _, _, _ => false
  | fuel + 1, seen, currentSlug, entries =>
    if currentSlug ∈ seen then false
    else
      match findEntry currentSlug entries with
      | none => false
      | some e =>
          match e.parent with
          | none => true
          | some p => reachesRoot fuel (currentSlug :: seen) p entries

def allReachRoot (entries : List DomainEntry) : Bool :=
  entries.all (fun e => reachesRoot (entries.length + 1) [] e.slug entries)

def allIdentifiers (entries : List DomainEntry) : List Nat :=
  entries.foldr (fun e acc => e.slug :: (e.aliases ++ acc)) []

def noAliasCollisions (entries : List DomainEntry) : Bool :=
  allDistinct (allIdentifiers entries)

def validateDomainEntries (entries : List DomainEntry) : Bool :=
  slugsUnique entries && parentDeclared entries && allReachRoot entries && noAliasCollisions entries

theorem reachesRoot_rejects_self_loop : ∀ (fuel : Nat) (s : Nat),
    reachesRoot fuel [] s [⟨s, some s, []⟩] = false := by
  intro fuel s
  cases fuel with
  | zero => rfl
  | succ f => cases f with
    | zero => simp [reachesRoot, findEntry]
    | succ f' => simp [reachesRoot, findEntry]

theorem self_loop_passes_parentDeclared_but_fails_validate (s : Nat) :
    parentDeclared [⟨s, some s, []⟩] = true ∧
    validateDomainEntries [⟨s, some s, []⟩] = false := by
  simp [parentDeclared, validateDomainEntries, allReachRoot, reachesRoot, findEntry]

theorem valid_registry_has_declared_parents : ∀ (entries : List DomainEntry),
    validateDomainEntries entries = true → parentDeclared entries = true := by
  intro entries h
  simp [validateDomainEntries, Bool.and_eq_true] at h
  exact h.1.1.2

theorem valid_registry_acyclic : ∀ (entries : List DomainEntry),
    validateDomainEntries entries = true → allReachRoot entries = true := by
  intro entries h
  simp [validateDomainEntries, Bool.and_eq_true] at h
  exact h.1.2

theorem valid_registry_disjoint_identifiers : ∀ (entries : List DomainEntry),
    validateDomainEntries entries = true → slugsUnique entries = true ∧ noAliasCollisions entries = true := by
  intro entries h
  simp [validateDomainEntries, Bool.and_eq_true] at h
  exact ⟨h.1.1.1, h.2⟩

end NeuroStrata.DomainRegistry
