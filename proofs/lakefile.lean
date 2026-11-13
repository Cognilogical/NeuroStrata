import Lake
open Lake DSL

package «neurostrata-proofs» where
  version := v!"0.1.0"

lean_lib «NeuroStrata» where
  roots := #[`NeuroStrata]

@[default_target]
lean_exe «neurostrata-proofs» where
  root := `Main
