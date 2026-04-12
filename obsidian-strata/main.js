var __defProp = Object.defineProperty;
var __getOwnPropDesc = Object.getOwnPropertyDescriptor;
var __getOwnPropNames = Object.getOwnPropertyNames;
var __hasOwnProp = Object.prototype.hasOwnProperty;
var __export = (target, all) => {
  for (var name in all)
    __defProp(target, name, { get: all[name], enumerable: true });
};
var __copyProps = (to, from, except, desc) => {
  if (from && typeof from === "object" || typeof from === "function") {
    for (let key of __getOwnPropNames(from))
      if (!__hasOwnProp.call(to, key) && key !== except)
        __defProp(to, key, { get: () => from[key], enumerable: !(desc = __getOwnPropDesc(from, key)) || desc.enumerable });
  }
  return to;
};
var __toCommonJS = (mod) => __copyProps(__defProp({}, "__esModule", { value: true }), mod);

// main.ts
var main_exports = {};
__export(main_exports, {
  default: () => StrataPlugin
});
module.exports = __toCommonJS(main_exports);
var import_obsidian = require("obsidian");
var DEFAULT_SETTINGS = {
  qdrantUrl: "http://127.0.0.1:6333",
  collectionName: "strata",
  embedderUrl: "http://127.0.0.1:8004/v1/embeddings",
  autoUpdateCanvas: false
};
var VIEW_TYPE_STRATA = "strata-view";
var CANVAS_FILE_NAME = "Strata MemorySpace.canvas";
var STRATA_ICON_SVG = `<svg viewBox="0 0 100 100" fill="none" stroke="currentColor" stroke-width="4" stroke-linecap="round" stroke-linejoin="round">
  <circle cx="50" cy="50" r="45" stroke-dasharray="4 4" opacity="0.3"/>
  <circle cx="30" cy="35" r="8"/>
  <circle cx="70" cy="35" r="8"/>
  <circle cx="50" cy="65" r="8"/>
  <circle cx="20" cy="65" r="5"/>
  <circle cx="80" cy="65" r="5"/>
  <path d="M30 43 L50 57"/>
  <path d="M70 43 L50 57"/>
  <path d="M38 35 L62 35"/>
  <path d="M25 65 L42 65"/>
  <path d="M75 65 L58 65"/>
  <path d="M30 43 L20 60"/>
  <path d="M70 43 L80 60"/>
</svg>`;
var StrataView = class extends import_obsidian.ItemView {
  constructor(leaf, plugin) {
    super(leaf);
    this.currentNamespace = "fish";
    this.allCurrentPoints = [];
    this.plugin = plugin;
  }
  getViewType() {
    return VIEW_TYPE_STRATA;
  }
  getDisplayText() {
    return "Strata Inspector";
  }
  getIcon() {
    return "strata-brain";
  }
  get qdrantPointsUrl() {
    const baseUrl = this.plugin.settings.qdrantUrl.replace(/\/$/, "");
    return `${baseUrl}/collections/${this.plugin.settings.collectionName}/points`;
  }
  get embedderUrl() {
    return this.plugin.settings.embedderUrl;
  }
  async onOpen() {
    const container = this.containerEl.children[1];
    container.empty();
    container.createEl("h4", { text: "Strata Curation" });
    const controls = container.createDiv({ cls: "strata-header-controls" });
    controls.style.flexWrap = "wrap";
    const select = controls.createEl("select", { cls: "strata-select" });
    const namespaces = ["fish", "global", "system_architecture"];
    namespaces.forEach((ns) => {
      const option = select.createEl("option", { text: ns, value: ns });
      if (ns === this.currentNamespace)
        option.selected = true;
    });
    select.onchange = (e) => {
      this.currentNamespace = e.target.value;
      this.loadMemories();
    };
    const addBtn = controls.createEl("button", { text: "+ Add Memory", cls: "strata-btn" });
    const refreshBtn = controls.createEl("button", { text: "Refresh", cls: "strata-btn" });
    const canvasBtn = controls.createEl("button", { text: "View MemorySpace", cls: "strata-btn" });
    canvasBtn.style.backgroundColor = "var(--interactive-accent)";
    canvasBtn.style.color = "var(--text-on-accent)";
    canvasBtn.onclick = async () => {
      canvasBtn.innerText = "Generating...";
      canvasBtn.disabled = true;
      await this.plugin.generateCanvas(this.allCurrentPoints);
      canvasBtn.innerText = "View MemorySpace";
      canvasBtn.disabled = false;
    };
    this.searchInput = container.createEl("input", {
      type: "text",
      placeholder: "Filter memories...",
      cls: "strata-memory-edit-area"
    });
    this.searchInput.style.minHeight = "auto";
    this.searchInput.style.padding = "6px";
    this.searchInput.style.marginBottom = "15px";
    this.searchInput.oninput = () => {
      this.renderFilteredMemories();
    };
    this.addForm = container.createDiv({ cls: "strata-memory-card" });
    this.addForm.style.display = "none";
    this.addForm.style.borderColor = "var(--interactive-accent)";
    this.addForm.createEl("div", { text: "New Memory Content:", cls: "strata-memory-text" });
    this.addTextarea = this.addForm.createEl("textarea", { cls: "strata-memory-edit-area" });
    const metaContainer = this.addForm.createDiv();
    metaContainer.style.display = "grid";
    metaContainer.style.gridTemplateColumns = "1fr 1fr";
    metaContainer.style.gap = "8px";
    metaContainer.style.marginTop = "10px";
    const refDiv = metaContainer.createDiv();
    const refLabel = refDiv.createEl("div", { text: "File:", cls: "strata-memory-text" });
    refLabel.style.fontSize = "var(--font-ui-smaller)";
    this.refInput = refDiv.createEl("input", { type: "text", cls: "strata-memory-edit-area" });
    this.refInput.style.minHeight = "auto";
    this.refInput.style.padding = "5px";
    const linesDiv = metaContainer.createDiv();
    const linesLabel = linesDiv.createEl("div", { text: "Lines (e.g. 42-49):", cls: "strata-memory-text" });
    linesLabel.style.fontSize = "var(--font-ui-smaller)";
    this.linesInput = linesDiv.createEl("input", { type: "text", cls: "strata-memory-edit-area" });
    this.linesInput.style.minHeight = "auto";
    this.linesInput.style.padding = "5px";
    const addBtnGroup = this.addForm.createDiv({ cls: "strata-button-group" });
    addBtnGroup.style.marginTop = "10px";
    const saveNewBtn = addBtnGroup.createEl("button", { text: "Save Memory", cls: "strata-btn" });
    const cancelNewBtn = addBtnGroup.createEl("button", { text: "Cancel", cls: "strata-btn" });
    addBtn.onclick = () => {
      this.addForm.style.display = "block";
      this.addTextarea.value = "";
      const activeFile = this.app.workspace.getActiveFile();
      this.refInput.value = activeFile ? activeFile.path : "";
      this.linesInput.value = "";
    };
    cancelNewBtn.onclick = () => {
      this.addForm.style.display = "none";
    };
    saveNewBtn.onclick = async () => {
      const content = this.addTextarea.value.trim();
      if (!content) {
        new import_obsidian.Notice("Memory content cannot be empty.");
        return;
      }
      saveNewBtn.innerText = "Saving...";
      saveNewBtn.disabled = true;
      const refFile = this.refInput.value.trim();
      const refLines = this.linesInput.value.trim();
      let vector = Array(768).fill(0);
      try {
        const embRes = await fetch(this.embedderUrl, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ input: content })
        });
        if (embRes.ok) {
          const embData = await embRes.json();
          if (embData.data && embData.data[0] && embData.data[0].embedding) {
            vector = embData.data[0].embedding;
          }
        }
      } catch (e) {
        console.warn("Strata Embedder failed. Falling back to zero-vector.", e);
      }
      const pointId = crypto.randomUUID();
      const payload = {
        data: content,
        user_id: this.currentNamespace
      };
      if (refFile) {
        payload.refs = [{ file: refFile }];
        if (refLines)
          payload.refs[0].lines = refLines;
      }
      try {
        await fetch(`${this.qdrantPointsUrl}?wait=true`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            points: [{
              id: pointId,
              payload,
              vector
            }]
          })
        });
        new import_obsidian.Notice(`Memory added to ${this.currentNamespace}`);
        this.addForm.style.display = "none";
        setTimeout(() => {
          this.loadMemories();
        }, 250);
      } catch (e) {
        new import_obsidian.Notice(`Failed to save memory: ${e}`);
      }
      saveNewBtn.innerText = "Save Memory";
      saveNewBtn.disabled = false;
    };
    refreshBtn.onclick = () => {
      this.searchInput.value = "";
      this.loadMemories();
    };
    this.memoriesContainer = container.createDiv();
    await this.loadMemories();
  }
  openAddMemoryForm(initialText, referencePath, linesStr) {
    if (!this.addForm)
      return;
    this.addForm.style.display = "block";
    this.addTextarea.value = initialText;
    this.refInput.value = referencePath;
    this.linesInput.value = linesStr;
    this.addTextarea.focus();
  }
  async loadMemories() {
    var _a;
    this.memoriesContainer.empty();
    this.memoriesContainer.createEl("p", { text: "Loading..." });
    try {
      const res = await fetch(`${this.qdrantPointsUrl}/scroll`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          filter: { must: [{ key: "user_id", match: { value: this.currentNamespace } }] },
          limit: 100,
          with_payload: true
        })
      });
      const data = await res.json();
      this.allCurrentPoints = ((_a = data.result) == null ? void 0 : _a.points) || [];
      this.renderFilteredMemories();
      if (this.plugin.settings.autoUpdateCanvas) {
        await this.plugin.generateCanvas(this.allCurrentPoints, true);
      }
    } catch (e) {
      this.memoriesContainer.empty();
      this.memoriesContainer.createEl("p", { text: `Connection Error: Check settings. (${e})` });
    }
  }
  renderFilteredMemories() {
    this.memoriesContainer.empty();
    if (this.allCurrentPoints.length === 0) {
      this.memoriesContainer.createEl("p", { text: `No memories found in namespace: ${this.currentNamespace}` });
      return;
    }
    const filterText = this.searchInput.value.toLowerCase();
    const filteredPoints = this.allCurrentPoints.filter((point) => {
      var _a;
      const textData = ((_a = point.payload) == null ? void 0 : _a.data) || "";
      return textData.toLowerCase().includes(filterText);
    });
    if (filteredPoints.length === 0) {
      this.memoriesContainer.createEl("p", { text: "No memories match your filter." });
      return;
    }
    filteredPoints.forEach((point) => this.renderMemoryCard(point));
  }
  renderMemoryCard(point) {
    var _a, _b;
    const card = this.memoriesContainer.createDiv({ cls: "strata-memory-card" });
    let textData = ((_a = point.payload) == null ? void 0 : _a.data) || "";
    let refs = ((_b = point.payload) == null ? void 0 : _b.refs) || [];
    const textDisplay = card.createDiv({ cls: "strata-memory-text", text: textData });
    textDisplay.style.whiteSpace = "pre-wrap";
    textDisplay.style.marginBottom = "8px";
    const badgeContainer = card.createDiv({ cls: "strata-refs-container" });
    const renderBadges = (currentRefs) => {
      badgeContainer.empty();
      if (currentRefs.length > 0) {
        currentRefs.forEach((r) => {
          if (!r.file)
            return;
          const badge = badgeContainer.createEl("span", { cls: "internal-link" });
          badge.style.display = "inline-block";
          badge.style.fontSize = "var(--font-ui-smaller)";
          badge.style.padding = "2px 8px";
          badge.style.backgroundColor = "var(--background-modifier-border)";
          badge.style.borderRadius = "12px";
          badge.style.cursor = "pointer";
          badge.style.marginRight = "5px";
          badge.style.marginBottom = "5px";
          const linesTxt = r.lines ? ` (Lines ${r.lines})` : "";
          badge.innerText = `\u{1F4C4} ${r.file.split("/").pop()}${linesTxt}`;
          badge.onclick = (e) => {
            e.preventDefault();
            this.app.workspace.openLinkText(r.file, "", true);
          };
        });
      }
    };
    renderBadges(refs);
    const editContainer = card.createDiv();
    editContainer.style.display = "none";
    const editArea = editContainer.createEl("textarea", { cls: "strata-memory-edit-area" });
    editArea.value = textData;
    const editMetaContainer = editContainer.createDiv();
    editMetaContainer.style.display = "grid";
    editMetaContainer.style.gridTemplateColumns = "1fr 1fr";
    editMetaContainer.style.gap = "8px";
    editMetaContainer.style.marginTop = "10px";
    const editRefDiv = editMetaContainer.createDiv();
    editRefDiv.createEl("div", { text: "File:", cls: "strata-memory-text" }).style.fontSize = "var(--font-ui-smaller)";
    const editRefInput = editRefDiv.createEl("input", { type: "text", cls: "strata-memory-edit-area" });
    editRefInput.style.minHeight = "auto";
    editRefInput.style.padding = "5px";
    const editLinesDiv = editMetaContainer.createDiv();
    editLinesDiv.createEl("div", { text: "Lines:", cls: "strata-memory-text" }).style.fontSize = "var(--font-ui-smaller)";
    const editLinesInput = editLinesDiv.createEl("input", { type: "text", cls: "strata-memory-edit-area" });
    editLinesInput.style.minHeight = "auto";
    editLinesInput.style.padding = "5px";
    if (refs.length > 0) {
      editRefInput.value = refs[0].file || "";
      editLinesInput.value = refs[0].lines || "";
    }
    const btnGroup = card.createDiv({ cls: "strata-button-group" });
    btnGroup.style.marginTop = "10px";
    const editBtn = btnGroup.createEl("button", { text: "Edit", cls: "strata-btn" });
    const deleteBtn = btnGroup.createEl("button", { text: "Delete", cls: "strata-btn strata-btn-delete" });
    const saveBtn = btnGroup.createEl("button", { text: "Save", cls: "strata-btn" });
    const cancelBtn = btnGroup.createEl("button", { text: "Cancel", cls: "strata-btn" });
    saveBtn.style.display = "none";
    cancelBtn.style.display = "none";
    editBtn.onclick = () => {
      textDisplay.style.display = "none";
      badgeContainer.style.display = "none";
      editContainer.style.display = "block";
      editBtn.style.display = "none";
      deleteBtn.style.display = "none";
      saveBtn.style.display = "inline-block";
      cancelBtn.style.display = "inline-block";
    };
    cancelBtn.onclick = () => {
      textDisplay.style.display = "block";
      badgeContainer.style.display = "block";
      editContainer.style.display = "none";
      saveBtn.style.display = "none";
      cancelBtn.style.display = "none";
      editBtn.style.display = "inline-block";
      deleteBtn.style.display = "inline-block";
    };
    saveBtn.onclick = async () => {
      const newText = editArea.value.trim();
      saveBtn.innerText = "Saving & Re-embedding...";
      saveBtn.disabled = true;
      cancelBtn.disabled = true;
      const newPayload = { data: newText, user_id: this.currentNamespace };
      if (editRefInput.value.trim()) {
        newPayload.refs = [{ file: editRefInput.value.trim() }];
        if (editLinesInput.value.trim()) {
          newPayload.refs[0].lines = editLinesInput.value.trim();
        }
      }
      let newVector = null;
      try {
        const embRes = await fetch(this.embedderUrl, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ input: newText })
        });
        if (embRes.ok) {
          const embData = await embRes.json();
          if (embData.data && embData.data[0] && embData.data[0].embedding) {
            newVector = embData.data[0].embedding;
          }
        }
      } catch (e) {
        console.warn("Re-embedding failed. Fallback to updating payload only.", e);
      }
      try {
        if (newVector) {
          await fetch(`${this.qdrantPointsUrl}?wait=true`, {
            method: "PUT",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({
              points: [{
                id: point.id,
                payload: newPayload,
                vector: newVector
              }]
            })
          });
        } else {
          await fetch(`${this.qdrantPointsUrl}/payload?wait=true`, {
            method: "PUT",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({
              payload: newPayload,
              points: [point.id]
            })
          });
        }
        new import_obsidian.Notice("Memory updated successfully.");
        textData = newText;
        refs = newPayload.refs || [];
        point.payload = newPayload;
        textDisplay.innerText = textData;
        renderBadges(refs);
        textDisplay.style.display = "block";
        badgeContainer.style.display = "block";
        editContainer.style.display = "none";
        saveBtn.style.display = "none";
        cancelBtn.style.display = "none";
        editBtn.style.display = "inline-block";
        deleteBtn.style.display = "inline-block";
        if (this.plugin.settings.autoUpdateCanvas) {
          await this.plugin.generateCanvas(this.allCurrentPoints, true);
        }
      } catch (e) {
        new import_obsidian.Notice(`Failed to update: ${e}`);
      }
      saveBtn.disabled = false;
      cancelBtn.disabled = false;
      saveBtn.innerText = "Save";
    };
    deleteBtn.onclick = async () => {
      if (confirm("Are you sure you want to delete this memory?")) {
        deleteBtn.innerText = "Deleting...";
        deleteBtn.disabled = true;
        try {
          await fetch(`${this.qdrantPointsUrl}/delete?wait=true`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ points: [point.id] })
          });
          card.remove();
          this.allCurrentPoints = this.allCurrentPoints.filter((p) => p.id !== point.id);
          new import_obsidian.Notice("Memory deleted.");
          if (this.plugin.settings.autoUpdateCanvas) {
            await this.plugin.generateCanvas(this.allCurrentPoints, true);
          }
        } catch (e) {
          new import_obsidian.Notice(`Failed to delete: ${e}`);
          deleteBtn.innerText = "Delete";
          deleteBtn.disabled = false;
        }
      }
    };
  }
};
var StrataSettingTab = class extends import_obsidian.PluginSettingTab {
  constructor(app, plugin) {
    super(app, plugin);
    this.plugin = plugin;
  }
  display() {
    const { containerEl } = this;
    containerEl.empty();
    containerEl.createEl("h2", { text: "Strata Settings" });
    new import_obsidian.Setting(containerEl).setName("Qdrant Base URL").setDesc("The base URL for your local Qdrant vector database").addText((text) => text.setPlaceholder("http://127.0.0.1:6333").setValue(this.plugin.settings.qdrantUrl).onChange(async (value) => {
      this.plugin.settings.qdrantUrl = value;
      await this.plugin.saveSettings();
    }));
    new import_obsidian.Setting(containerEl).setName("Collection Name").setDesc("The name of the Qdrant collection used by Strata").addText((text) => text.setPlaceholder("strata").setValue(this.plugin.settings.collectionName).onChange(async (value) => {
      this.plugin.settings.collectionName = value;
      await this.plugin.saveSettings();
    }));
    new import_obsidian.Setting(containerEl).setName("Embedder URL").setDesc("The endpoint for your local embedding model").addText((text) => text.setPlaceholder("http://127.0.0.1:8004/v1/embeddings").setValue(this.plugin.settings.embedderUrl).onChange(async (value) => {
      this.plugin.settings.embedderUrl = value;
      await this.plugin.saveSettings();
    }));
    new import_obsidian.Setting(containerEl).setName("Auto-Update Canvas").setDesc('Automatically regenerate the "Strata MemorySpace.canvas" file when modifying memories.').addToggle((toggle) => toggle.setValue(this.plugin.settings.autoUpdateCanvas).onChange(async (value) => {
      this.plugin.settings.autoUpdateCanvas = value;
      await this.plugin.saveSettings();
    }));
  }
};
var StrataPlugin = class extends import_obsidian.Plugin {
  async onload() {
    await this.loadSettings();
    (0, import_obsidian.addIcon)("strata-brain", STRATA_ICON_SVG);
    this.addSettingTab(new StrataSettingTab(this.app, this));
    this.registerView(
      VIEW_TYPE_STRATA,
      (leaf) => new StrataView(leaf, this)
    );
    this.addRibbonIcon("strata-brain", "Open Strata Inspector", () => {
      this.activateView();
    });
    this.registerEvent(
      this.app.workspace.on("editor-menu", (menu, editor, view) => {
        const selection = editor.getSelection();
        if (selection && selection.trim().length > 0) {
          menu.addItem((item) => {
            item.setTitle("Create Strata Memory (Paragraph)").setIcon("strata-brain").onClick(async () => {
              const selections = editor.listSelections();
              let linesStr = "";
              if (selections.length > 0) {
                let startLine = Math.min(selections[0].anchor.line, selections[0].head.line);
                let endLine = Math.max(selections[0].anchor.line, selections[0].head.line);
                while (startLine > 0 && editor.getLine(startLine - 1).trim() !== "") {
                  startLine--;
                }
                const lineCount = editor.lineCount();
                while (endLine < lineCount - 1 && editor.getLine(endLine + 1).trim() !== "") {
                  endLine++;
                }
                linesStr = `${startLine + 1}-${endLine + 1}`;
              }
              const viewInstance = await this.activateView();
              if (viewInstance && viewInstance instanceof StrataView) {
                const filePath = view.file ? view.file.path : "";
                viewInstance.openAddMemoryForm(selection, filePath, linesStr);
              }
            });
          });
        }
      })
    );
  }
  async generateCanvas(points, silent = false) {
    const domainMap = /* @__PURE__ */ new Map();
    const orphanedMemories = [];
    points.forEach((p) => {
      var _a;
      const refs = ((_a = p.payload) == null ? void 0 : _a.refs) || [];
      let foundDoc = false;
      if (refs.length > 0) {
        refs.forEach((r) => {
          if (r.file) {
            const docPath = r.file;
            if (!domainMap.has(docPath))
              domainMap.set(docPath, []);
            domainMap.get(docPath).push(p);
            foundDoc = true;
          }
        });
      }
      if (!foundDoc) {
        orphanedMemories.push(p);
      }
    });
    const nodes = [];
    const edges = [];
    let startX = -1e3;
    let startY = -1e3;
    const DOC_WIDTH = 400;
    const DOC_HEIGHT = 400;
    const MEM_WIDTH = 300;
    const MEM_HEIGHT = 150;
    const X_GAP = 500;
    const Y_GAP = 200;
    let maxStructuredY = startY + DOC_HEIGHT;
    Array.from(domainMap.keys()).forEach((docPath, colIndex) => {
      const x = startX + colIndex * (DOC_WIDTH + X_GAP);
      const y = startY;
      const docNodeId = `doc-${colIndex}`;
      nodes.push({
        id: docNodeId,
        type: "file",
        file: docPath,
        x,
        y,
        width: DOC_WIDTH,
        height: DOC_HEIGHT,
        color: "4"
      });
      const memories = domainMap.get(docPath);
      memories.forEach((mem, rowIndex) => {
        var _a, _b;
        const memNodeId = `mem-${mem.id}`;
        const memX = x + DOC_WIDTH + 100;
        const memY = y + rowIndex * (MEM_HEIGHT + 50);
        const rawText = ((_a = mem.payload) == null ? void 0 : _a.data) || "";
        nodes.push({
          id: memNodeId,
          type: "text",
          text: `**Memory [${((_b = mem.payload) == null ? void 0 : _b.user_id) || "unknown"}]**

${rawText}`,
          x: memX,
          y: memY,
          width: MEM_WIDTH,
          height: MEM_HEIGHT,
          color: "3"
        });
        edges.push({
          id: `edge-${mem.id}`,
          fromNode: memNodeId,
          fromSide: "left",
          toNode: docNodeId,
          toSide: "right",
          color: "5"
        });
      });
      const columnMaxY = y + memories.length * (MEM_HEIGHT + 50);
      if (columnMaxY > maxStructuredY) {
        maxStructuredY = columnMaxY;
      }
    });
    if (orphanedMemories.length > 0) {
      const orphStartY = maxStructuredY + 400;
      const cols = 4;
      const rows = Math.ceil(orphanedMemories.length / cols);
      const orphGroupWidth = cols * MEM_WIDTH + (cols - 1) * 50 + 100;
      const orphGroupHeight = rows * MEM_HEIGHT + (rows - 1) * 50 + 100;
      nodes.push({
        id: "group-orphans",
        type: "group",
        x: startX - 50,
        y: orphStartY - 50,
        width: orphGroupWidth,
        height: orphGroupHeight,
        label: "Orphaned Memories (Needs Linking or Deletion)",
        color: "1"
      });
      orphanedMemories.forEach((mem, index) => {
        var _a, _b;
        const col = index % cols;
        const row = Math.floor(index / cols);
        nodes.push({
          id: `mem-${mem.id}`,
          type: "text",
          text: `**Orphaned [${((_a = mem.payload) == null ? void 0 : _a.user_id) || "unknown"}]**

${((_b = mem.payload) == null ? void 0 : _b.data) || ""}`,
          x: startX + col * (MEM_WIDTH + 50),
          y: orphStartY + row * (MEM_HEIGHT + 50),
          width: MEM_WIDTH,
          height: MEM_HEIGHT,
          color: "1"
        });
      });
    }
    const canvasData = {
      nodes,
      edges
    };
    const fileData = JSON.stringify(canvasData, null, 2);
    let file = this.app.vault.getAbstractFileByPath(CANVAS_FILE_NAME);
    try {
      if (file) {
        await this.app.vault.modify(file, fileData);
      } else {
        file = await this.app.vault.create(CANVAS_FILE_NAME, fileData);
      }
      if (!silent) {
        new import_obsidian.Notice("Strata MemorySpace generated successfully!");
        this.app.workspace.getLeaf(false).openFile(file);
      }
    } catch (e) {
      console.error("Failed to generate MemorySpace:", e);
      if (!silent)
        new import_obsidian.Notice("Failed to generate MemorySpace.");
    }
  }
  async loadSettings() {
    this.settings = Object.assign({}, DEFAULT_SETTINGS, await this.loadData());
  }
  async saveSettings() {
    await this.saveData(this.settings);
    const leaves = this.app.workspace.getLeavesOfType(VIEW_TYPE_STRATA);
    for (const leaf of leaves) {
      if (leaf.view instanceof StrataView) {
        leaf.view.loadMemories();
      }
    }
  }
  async activateView() {
    const { workspace } = this.app;
    let leaf = null;
    const leaves = workspace.getLeavesOfType(VIEW_TYPE_STRATA);
    if (leaves.length > 0) {
      leaf = leaves[0];
    } else {
      leaf = workspace.getRightLeaf(false);
      if (leaf) {
        await leaf.setViewState({ type: VIEW_TYPE_STRATA, active: true });
      }
    }
    if (leaf) {
      workspace.revealLeaf(leaf);
      return leaf.view;
    }
    return null;
  }
};
