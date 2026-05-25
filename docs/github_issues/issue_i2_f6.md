Title: [REMEDIATED] [I2-F6] CLI vs Server Duality Refactor with Clap Subcommands
Labels: enhancement, architecture
Body:
### Description
The NeuroStrata command-line parser in `src/main.rs` was refactored to use `clap` subcommands cleanly separating CLI actions (`namespaces`, `list`, `ingest`, `export-graph`, `delete`, `add`, `edit`) from `daemon` mode.

### Changes Made
- Added `clap = { version = "4", features = ["derive"] }` to `Cargo.toml`.
- Refactored `src/main.rs` to parse options using a derive-based `Cli` struct.
- Implemented robust fallback logic that dynamically executes unrecognized subcommands as external plugins, maintaining 100% backward compatibility.
- Safely integrated daemon lock checks before executing database-modifying commands.

### Status
- **REMEDIATED** in local branch `remediate-p4-p5-p6`.
