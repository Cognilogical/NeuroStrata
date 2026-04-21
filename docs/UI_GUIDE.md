# 🖌️ NeuroStrata UI User Guide (Obsidian Integration)

The NeuroStrata Obsidian plugin provides a native, seamless interface for curating and visualizing your AI's 3-Tier Dual-Temporal-Memory directly within your knowledge base. It connects securely to the Dual-Mode NeuroStrata Go Server (`http://localhost:8005/api/memory`) to ensure your Obsidian notes and your AI's latent space remain perfectly synchronized.

---

## 1. The NeuroStrata Inspector (Sidebar UI)

The **NeuroStrata Inspector** is a dedicated sidebar panel that gives you full x-ray vision into the AI's vector database. It removes the opaque "black box" nature of AI memory and gives you direct CRUD (Create, Read, Update, Delete) control.

![Placeholder: NeuroStrata Inspector Sidebar Panel](./assets/neurostrata-inspector.png)
*(Drop your screenshot of the NeuroStrata Inspector sidebar here)*

**Key Features:**
* **Namespace Filtering:** Instantly filter memories by their 3 Tiers. View all `global` infrastructure mandates, or drill down into specific Domain rules (e.g., `nibble_server`) or Task contexts.
* **Live Semantic Search:** Type a natural language query into the inspector search bar to instantly find matching architectural rules or prior decisions across the database.
* **Full CRUD Control:** Did an agent hallucinate a rule? Did a project requirement change? You can edit the text of any memory directly in the sidebar, and the plugin will automatically re-embed and save it back to Qdrant. You can also permanently delete obsolete memories with a single click.

---

## 2. Context-Aware Memory Creation (Right-Click)

While agents can save memories autonomously, you can also easily inject human insight into the latent space using the editor's right-click context menu.

![Placeholder: Right-Click Context Menu in Obsidian Editor](./assets/neurostrata-right-click.png)
*(Drop your screenshot of the "Create NeuroStrata Memory (Paragraph)" context menu here)*

**Key Features:**
* **"Create NeuroStrata Memory (Paragraph)":** Simply right-click anywhere inside a narrative document (e.g., `docs/architecture/domains/ml_training.md`) and select this option.
* **Auto-Boundary Detection:** The plugin is intelligent enough to automatically detect the exact paragraph boundaries (from blank line to blank line) surrounding your cursor.
* **Structured Pointers (Pointer-Wiki):** It extracts the exact physical file path and line numbers (e.g., `lines 42-49`) and automatically attaches them as a structured JSON `refs` array inside the Qdrant payload. When an agent retrieves this memory later, it uses these coordinates to fetch only the necessary context window (Compact Reading).

---

## 3. The MemorySpace Canvas (Visual Latent Space)

Vectors are notoriously difficult for humans to understand. NeuroStrata solves this by turning the mathematical latent space into a physical, spatial graph that you can interact with.

![Placeholder: NeuroStrata MemorySpace.canvas Visualization](./assets/neurostrata-canvas.png)
*(Drop your screenshot of the generated Obsidian Canvas here)*

**Key Features:**
* **Programmatic Auto-Generation:** Whether triggered by you in Obsidian or autonomously by an agent via the `neurostrata_generate_canvas` tool, NeuroStrata pulls down the entire vector database and plots it onto an infinite canvas (`NeuroStrata MemorySpace.canvas`).
* **Collision-Aware Layout:** Memories are automatically organized into non-overlapping spatial nodes.
* **Orphan Quarantine:** "Orphaned" memories (those missing valid document pointers or containing raw legacy rules) are automatically isolated into a separate quarantine box at the bottom of the canvas, making it easy to identify technical debt and clean up your database.
* **Custom Branding:** Nodes are styled with Obsidian's native color palette and custom SVG branding (the neurostrata-brain icon) to clearly distinguish them from standard markdown notes.

---

## 4. Local OS Editor Integration (Web UI)

If you are using the standalone browser-based **NeuroStrata Web UI** (via `/graph.json`), it includes a native file explorer to browse the physical codebase and the memory graph side-by-side. 

Because the web UI runs in a browser sandbox, we rely on custom OS URL protocols to launch your local editor.

**How it works:**
1. When the `neurostrata-mcp export-graph` command runs, it computes the absolute path for every file and memory node and injects it into the graph as `absolute_path`.
2. When you click a file in the Web UI's **File Explorer** (or click a memory node linked to a file), its details appear in the right-hand **Details & Viewer** panel.
3. You can select your preferred editor from the dropdown (VS Code, Cursor, or Obsidian) which is saved locally to your browser's `localStorage`.
4. Clicking **"Open File"** triggers the respective custom protocol:
   - **VS Code:** `vscode://file/{absolute_path}`
   - **Cursor:** `cursor://file/{absolute_path}`
   - **Obsidian:** `obsidian://open?path={encoded_absolute_path}`

---

## Getting Started

1. Ensure the **NeuroStrata Native Go Server** is running in the background (it automatically starts when your AI client launches via MCP).
2. Enable the NeuroStrata Plugin in your Obsidian Community Plugins settings.
3. Open the command palette (`Ctrl+P` or `Cmd+P`) and type `NeuroStrata: Open Inspector` to launch the sidebar!
