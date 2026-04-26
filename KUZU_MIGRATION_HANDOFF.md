# NeuroStrata Memory Architecture: FalkorDB Vision -> Kuzu Implementation

## 1. The Inspiration: Why We Looked at FalkorDB
Before the system crash, we evaluated [FalkorDB](https://github.com/falkordb/falkordb) to upgrade our memory architecture from simple vector search (LanceDB) to true **GraphRAG**. We loved these specific concepts from FalkorDB:
*   **Hybrid Graph+Vector Search:** The ability to store high-dimensional vector embeddings *directly* as properties on graph nodes. This allows us to do semantic search to find a starting node, and then use graph traversals to find multi-hop context (e.g., "Find memory X via semantic search -> traverse `DEPENDS_ON` edges -> return related architectural rules").
*   **Cypher Query Language:** Standardized, expressive queries for complex relationships.
*   **Typed Entities and Edges:** Moving away from flat document retrieval to structured knowledge (Nodes: `Memory`, `File`, `Concept`. Edges: `RELATES_TO`, `GOVERNS`, `IMPLEMENTS`).

## 2. The Pivot: Why Kuzu?
**The Constraint:** FalkorDB requires a standalone Redis server to run. This strictly violates NeuroStrata's core constraint: **Zero-deploy, embedded MVP**. 
**The Solution:** [Kuzu](https://kuzudb.com) provides the exact same Cypher-based property graph and vector storage capabilities, but it runs fully embedded in the Rust process (like SQLite or DuckDB).

## 3. How We Will Use Kuzu (The Implementation Plan)
In the new session, we need to implement Kuzu to replace LanceDB entirely:
1.  **Node Tables:** Create a `Memory` node table. It will store the `content` (string), `type` (string), and an `embedding` property (using Kuzu's `FLOAT[]` or `VECTOR` types).
2.  **Rel Tables:** Create relationship tables like `RELATES_TO` (From `Memory` to `Memory`).
3.  **Vector Search:** Use Kuzu's vector extension or array distance functions to do semantic similarity searches natively in Cypher.
4.  **Graph Traversal:** Retrieve memories not just by text similarity, but by walking the graph to pull in adjacent, relevant context to feed the LLM.

## 4. The Technical Blocker (Read This First)
*Note: In the previous session, adding `kuzu = "0.11.3"` to the main `neurostrata-mcp` workspace caused catastrophic C++ FFI linker errors (`rust-lld: error: undefined symbol: kuzu_rs$cxxbridge1$new_database`).*

**The root cause:** Dependency hell. The `kuzu` crate uses the `cxx` macro for C++ bindings. Because our main project has heavy dependencies, Cargo resolved `cxx` to a newer version than Kuzu's pre-compiled C++ binaries expected, breaking the ABI mangling during the final linking step. 

## 5. Execution Steps for the New Session
To avoid the endless compilation timeouts and linker errors from last time, follow this strict protocol:
1.  **DO NOT** add `kuzu` to `neurostrata-mcp/Cargo.toml` yet.
2.  Create a completely isolated testbed project outside the workspace (`cargo new kuzu_testbed`).
3.  Add `kuzu` to the testbed and ensure it compiles on this Linux machine (you may need to tweak `libc++` flags or pin the `cxx` crate version).
4.  Once the testbed compiles, write the LanceDB -> Kuzu migration script *inside the testbed*. Point it at `.NeuroStrata/lancedb` to extract the existing memories and insert them into a new Kuzu database.
5.  Only when the graph is successfully populated and queryable should you integrate it back into the main `neurostrata-mcp` application.
