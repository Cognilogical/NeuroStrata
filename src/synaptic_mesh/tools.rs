//! The MCP tool surface for SynapticMesh.
//!
//! `synaptic_mesh_tool_definitions()` returns the six tool schemas that
//! `src/server.rs` appends to its `tools/list` array. They are built here
//! rather than inline in `server.rs` so the mesh owns its own surface: one
//! function, one test, one place to edit.
//!
//! Every description carries the creation path and the failure posture,
//! because a tool description is the only place an agent learns both before
//! calling. A description that only says what the tool does gets discovered
//! too late — the discovery-lesson rule from 1.8.0.

use serde_json::{json, Value};

/// The six synaptic-mesh tool schemas, in registration order.
pub fn synaptic_mesh_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "neurostrata_synaptic_mesh_init",
            "description": "CREATE THE FIRST MESH HERE — no mesh exists until this call succeeds. Creates a new mesh repo under ~/.NeuroStrata/synaptic-mesh/<mesh_id>/ and joins this project to it by writing .NeuroStrata/synaptic-mesh-ref. Nothing is shared until a teammate runs `neurostrata_synaptic_mesh_join`. Idempotence: one mesh per project; a project that already has a pointer refuses.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Human-readable mesh name, e.g. 'platform-team'." },
                    "namespace": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Namespaces to share, repeatable. Omitted = the current project namespace only; the mesh is opt-in per namespace."
                    },
                    "remote": { "type": "string", "description": "Git remote URL the mesh pushes to. Omitted = local-only mesh; add it later before inviting teammates." },
                    "project_root": { "type": "string", "description": "Project root that holds the pointer file. Defaults to the working directory." },
                    "node_id": { "type": "string", "description": "Stable identity stamped on every record this node publishes. Defaults to the local node id." }
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "neurostrata_synaptic_mesh_join",
            "description": "Join an EXISTING mesh — the creation path is `neurostrata_synaptic_mesh_init`, which makes the mesh; this call only points this project at one that already exists (a teammate's mesh repo path, or a local mesh you are adopting). Writes .NeuroStrata/synaptic-mesh-ref into this project. DISCLOSURE: the mesh is a permanent record shared with the team — tombstones suppress content from search, but full erasure requires a coordinated history rewrite, and an uncooperative peer's clone retains the content. Read this before joining, not after.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "dir": { "type": "string", "description": "Path to the mesh repo to join (e.g. ~/.NeuroStrata/synaptic-mesh/<mesh_id>, or a teammate's checked-out mesh)." },
                    "project_root": { "type": "string", "description": "Project root that receives the pointer file. Defaults to the working directory." }
                },
                "required": ["dir"]
            }
        }),
        json!({
            "name": "neurostrata_synaptic_mesh_push",
            "description": "Publish this node's pending records to the mesh remote. Pull-before-push is automatic and retries a rejected push a bounded number of times; it never force-overwrites. NON-INTERACTIVE: git runs with GIT_TERMINAL_PROMPT=0, so a credential prompt can never wedge the session — it is fail-soft — a rejected or unauthenticated push queues locally and leaves local work intact. With no mesh joined it answers 'no mesh joined' rather than failing hard. Requires `neurostrata_synaptic_mesh_init` (to create) or `neurostrata_synaptic_mesh_join` (to adopt) first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_root": { "type": "string", "description": "Project root whose pointer file names the mesh. Defaults to the working directory." }
                },
                "required": []
            }
        }),
        json!({
            "name": "neurostrata_synaptic_mesh_pull",
            "description": "Fetch the mesh remote and replay changed records into the local store (diff-based and idempotent — replaying the same head is a no-op). NON-INTERACTIVE and fail-soft: git runs with GIT_TERMINAL_PROMPT=0, so an unreachable or unauthenticated remote reports the failure and leaves the local store serving from what it already has. With no mesh joined it answers 'no mesh joined'. Requires `neurostrata_synaptic_mesh_init` or `neurostrata_synaptic_mesh_join` first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_root": { "type": "string", "description": "Project root whose pointer file names the mesh. Defaults to the working directory." }
                },
                "required": []
            }
        }),
        json!({
            "name": "neurostrata_synaptic_mesh_leave",
            "description": "Leave the mesh: removes this project's .NeuroStrata/synaptic-mesh-ref pointer. Non-destructive — the local mesh clone and every published record stay on disk, and the team keeps its copy; re-join with `neurostrata_synaptic_mesh_join` at any time. With no mesh joined it answers 'no mesh joined'.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_root": { "type": "string", "description": "Project root whose pointer file is removed. Defaults to the working directory." }
                },
                "required": []
            }
        }),
        json!({
            "name": "neurostrata_synaptic_mesh_status",
            "description": "Report mesh state: mesh id and name, epoch, joined namespaces with their sync state, outbox pending count and oldest age, last accepted and last published heads, unresolved conflicts, and health ('healthy' or 'paused: <reason>'). Read-only and non-interactive; it never mutates. DISCLOSURE: the mesh is a permanent record shared with the team — tombstones suppress content from search, but full erasure requires a coordinated history rewrite, and an uncooperative peer's clone retains the content. With no mesh joined it answers 'no mesh joined'.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_root": { "type": "string", "description": "Project root whose pointer file names the mesh. Defaults to the working directory." }
                },
                "required": []
            }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED: [&str; 6] = [
        "neurostrata_synaptic_mesh_init",
        "neurostrata_synaptic_mesh_join",
        "neurostrata_synaptic_mesh_push",
        "neurostrata_synaptic_mesh_pull",
        "neurostrata_synaptic_mesh_leave",
        "neurostrata_synaptic_mesh_status",
    ];

    #[test]
    fn tool_definitions_has_six_names_valid_json() {
        let defs = synaptic_mesh_tool_definitions();
        let names: Vec<&str> = defs
            .iter()
            .map(|d| d["name"].as_str().expect("every tool is named"))
            .collect();
        assert_eq!(names, EXPECTED);

        for def in &defs {
            assert!(def["description"].as_str().is_some_and(|d| !d.is_empty()));
            let schema = &def["inputSchema"];
            assert_eq!(schema["type"], "object");
            assert!(schema["properties"].is_object());
            // Round-trips as strict JSON: this is what goes on the wire.
            let encoded = serde_json::to_string(def).expect("tool def is serializable");
            let _: serde_json::Value = serde_json::from_str(&encoded).expect("re-parses");
        }
    }

    #[test]
    fn init_schema_requires_name_and_teaches_creation_path() {
        let defs = synaptic_mesh_tool_definitions();
        let init = defs
            .iter()
            .find(|d| d["name"] == "neurostrata_synaptic_mesh_init")
            .expect("init tool present");
        let required: Vec<&str> = init["inputSchema"]["required"]
            .as_array()
            .expect("required array")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(
            required.contains(&"name"),
            "init requires a mesh name: {required:?}"
        );
        assert!(init["inputSchema"]["properties"]["namespace"].is_object());
        assert!(init["inputSchema"]["properties"]["remote"].is_object());
    }

    #[test]
    fn push_and_pull_descriptions_say_non_interactive_and_fail_soft() {
        let defs = synaptic_mesh_tool_definitions();
        for tool in ["push", "pull"] {
            let def = defs
                .iter()
                .find(|d| d["name"] == format!("neurostrata_synaptic_mesh_{tool}"))
                .expect("tool present");
            let desc = def["description"].as_str().unwrap().to_lowercase();
            assert!(
                desc.contains("non-interactive"),
                "{tool} must say non-interactive"
            );
            assert!(
                desc.contains("fail-soft") || desc.contains("never fails"),
                "{tool} must say fail-soft"
            );
            assert!(
                desc.contains("no mesh joined") || desc.contains("without a mesh"),
                "{tool} must teach the empty case: {desc}"
            );
        }
    }

    #[test]
    fn push_and_pull_take_no_required_arguments() {
        let defs = synaptic_mesh_tool_definitions();
        for tool in ["push", "pull", "status", "leave"] {
            let def = defs
                .iter()
                .find(|d| d["name"] == format!("neurostrata_synaptic_mesh_{tool}"))
                .expect("tool present");
            let required = def["inputSchema"]["required"].as_array();
            assert!(
                required.map(|r| r.is_empty()).unwrap_or(true),
                "{tool} must not require arguments: {required:?}"
            );
        }
    }

    /// Spec §"Deletion & erasure" §Disclosure requires the permanence warning
    /// on join AND status — a status report is where a user learns what they
    /// are already committed to.
    #[test]
    fn status_discloses_permanence_like_join() {
        let defs = synaptic_mesh_tool_definitions();
        for tool in ["join", "status"] {
            let def = defs
                .iter()
                .find(|d| d["name"] == format!("neurostrata_synaptic_mesh_{tool}"))
                .expect("tool present");
            let desc = def["description"].as_str().unwrap();
            assert!(
                desc.contains("permanent record"),
                "{tool} must disclose the mesh is a permanent record: {desc}"
            );
            assert!(
                desc.contains("erasure") || desc.contains("history rewrite"),
                "{tool} must say erasure is not immediate: {desc}"
            );
        }
    }

    #[test]
    fn join_teaches_the_pointer_and_disclosure() {
        let defs = synaptic_mesh_tool_definitions();
        let join = defs
            .iter()
            .find(|d| d["name"] == "neurostrata_synaptic_mesh_join")
            .expect("join tool present");
        let desc = join["description"].as_str().unwrap();
        assert!(
            desc.contains("init"),
            "join must point at the creation path: {desc}"
        );
        assert!(
            desc.contains("permanent record"),
            "join must disclose permanence: {desc}"
        );
        assert!(join["inputSchema"]["required"]
            .as_array()
            .is_some_and(|r| !r.is_empty()));
    }
}
