import { App, Plugin, PluginSettingTab, Setting, ItemView, WorkspaceLeaf, Notice, TFile, addIcon } from 'obsidian';
import * as mqtt from 'mqtt';

interface NeuroStrataPluginSettings {
    mqttUrl: string;
    autoUpdateCanvas: boolean;
}

const DEFAULT_SETTINGS: NeuroStrataPluginSettings = {
    mqttUrl: 'ws://127.0.0.1:8081',
    autoUpdateCanvas: false
}

const VIEW_TYPE_NEUROSTRATA = "neurostrata-view";
const CANVAS_FILE_NAME = "NeuroStrata MemorySpace.canvas";

const NEUROSTRATA_ICON_SVG = `<svg viewBox="0 0 100 100" fill="none" stroke="currentColor" stroke-width="4" stroke-linecap="round" stroke-linejoin="round">
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

class MqttClientWrapper {
    client: mqtt.MqttClient;
    pendingRequests: Map<string, {resolve: Function, reject: Function, timeout: any}> = new Map();

    constructor(url: string) {
        this.client = mqtt.connect(url);
        
        this.client.on('connect', () => {
            console.log('NeuroStrata: Connected to MQTT broker');
            this.client.subscribe('neurostrata/response', { qos: 0 });
        });

        this.client.on('message', (topic, message) => {
            if (topic === 'neurostrata/response') {
                try {
                    const res = JSON.parse(message.toString());
                    const reqId = res.request_id;
                    if (reqId && this.pendingRequests.has(reqId)) {
                        const { resolve, reject, timeout } = this.pendingRequests.get(reqId)!;
                        clearTimeout(timeout);
                        this.pendingRequests.delete(reqId);
                        
                        if (res.success) {
                            resolve(res.data);
                        } else {
                            reject(new Error(res.error || 'Unknown error'));
                        }
                    }
                } catch (e) {
                    console.error('NeuroStrata: Failed to parse MQTT response', e);
                }
            }
        });
    }

    async request(action: string, payload: any = {}): Promise<any> {
        return new Promise((resolve, reject) => {
            if (!this.client.connected) {
                return reject(new Error("Not connected to NeuroStrata MQTT broker"));
            }

            const reqId = crypto.randomUUID();
            const timeout = setTimeout(() => {
                this.pendingRequests.delete(reqId);
                reject(new Error(`MQTT Request timeout for action: ${action}`));
            }, 10000);

            this.pendingRequests.set(reqId, { resolve, reject, timeout });

            const req = {
                request_id: reqId,
                action: action,
                payload: payload
            };

            this.client.publish('neurostrata/request', JSON.stringify(req), { qos: 0 });
        });
    }
}

class NeuroStrataView extends ItemView {
    plugin: NeuroStrataPlugin;
    currentNamespace: string = 'global';
    memoriesContainer: HTMLElement;
    allCurrentPoints: any[] = [];
    searchInput: HTMLInputElement;

    addForm: HTMLElement;
    addTextarea: HTMLTextAreaElement;
    refInput: HTMLInputElement;
    linesInput: HTMLInputElement;
    namespaceSelect: HTMLSelectElement;

    constructor(leaf: WorkspaceLeaf, plugin: NeuroStrataPlugin) {
        super(leaf);
        this.plugin = plugin;
    }

    getViewType() {
        return VIEW_TYPE_NEUROSTRATA;
    }

    getDisplayText() {
        return "NeuroStrata Inspector";
    }

    getIcon() {
        return "neurostrata-brain";
    }

    async onOpen() {
        const container = this.containerEl.children[1];
        container.empty();
        container.createEl("h4", { text: "NeuroStrata Curation" });

        const controls = container.createDiv({ cls: "neurostrata-header-controls" });
        controls.style.flexWrap = "wrap";
        
        this.namespaceSelect = controls.createEl("select", { cls: "neurostrata-select" });
        
        this.namespaceSelect.onchange = (e) => {
            this.currentNamespace = (e.target as HTMLSelectElement).value;
            this.loadMemories();
        };

        const addBtn = controls.createEl("button", { text: "+ Add Memory", cls: "neurostrata-btn" });
        const refreshBtn = controls.createEl("button", { text: "Refresh", cls: "neurostrata-btn" });
        
        const canvasBtn = controls.createEl("button", { text: "View MemorySpace", cls: "neurostrata-btn" });
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
            cls: "neurostrata-memory-edit-area" 
        });
        this.searchInput.style.minHeight = "auto";
        this.searchInput.style.padding = "6px";
        this.searchInput.style.marginBottom = "15px";

        this.searchInput.oninput = () => {
            this.renderFilteredMemories();
        };

        this.addForm = container.createDiv({ cls: "neurostrata-memory-card" });
        this.addForm.style.display = 'none';
        this.addForm.style.borderColor = "var(--interactive-accent)";

        this.addForm.createEl("div", { text: "New Memory Content:", cls: "neurostrata-memory-text" });
        this.addTextarea = this.addForm.createEl("textarea", { cls: "neurostrata-memory-edit-area" });

        const metaContainer = this.addForm.createDiv();
        metaContainer.style.display = "grid";
        metaContainer.style.gridTemplateColumns = "1fr 1fr";
        metaContainer.style.gap = "8px";
        metaContainer.style.marginTop = "10px";

        const refDiv = metaContainer.createDiv();
        const refLabel = refDiv.createEl("div", { text: "File:", cls: "neurostrata-memory-text" });
        refLabel.style.fontSize = "var(--font-ui-smaller)";
        this.refInput = refDiv.createEl("input", { type: "text", cls: "neurostrata-memory-edit-area" });
        this.refInput.style.minHeight = "auto";
        this.refInput.style.padding = "5px";

        const linesDiv = metaContainer.createDiv();
        const linesLabel = linesDiv.createEl("div", { text: "Lines (e.g. 42-49):", cls: "neurostrata-memory-text" });
        linesLabel.style.fontSize = "var(--font-ui-smaller)";
        this.linesInput = linesDiv.createEl("input", { type: "text", cls: "neurostrata-memory-edit-area" });
        this.linesInput.style.minHeight = "auto";
        this.linesInput.style.padding = "5px";

        const addBtnGroup = this.addForm.createDiv({ cls: "neurostrata-button-group" });
        addBtnGroup.style.marginTop = "10px";
        const saveNewBtn = addBtnGroup.createEl("button", { text: "Save Memory", cls: "neurostrata-btn" });
        const cancelNewBtn = addBtnGroup.createEl("button", { text: "Cancel", cls: "neurostrata-btn" });

        addBtn.onclick = () => {
            this.addForm.style.display = 'block';
            this.addTextarea.value = '';
            
            const activeFile = this.app.workspace.getActiveFile();
            this.refInput.value = activeFile ? activeFile.path : '';
            this.linesInput.value = '';
        };

        cancelNewBtn.onclick = () => {
            this.addForm.style.display = 'none';
        };

        saveNewBtn.onclick = async () => {
            const content = this.addTextarea.value.trim();
            if (!content) {
                new Notice("Memory content cannot be empty.");
                return;
            }

            saveNewBtn.innerText = "Saving...";
            saveNewBtn.disabled = true;

            const refFile = this.refInput.value.trim();
            const refLines = this.linesInput.value.trim();
            
            let vector = Array(768).fill(0); 
            
            try {
                if (this.plugin.mqtt) {
                    const embRes = await this.plugin.mqtt.request('embed', { input: content });
                    if (embRes && embRes.embedding) {
                        vector = embRes.embedding;
                    }
                }
            } catch (e) {
                console.warn("NeuroStrata Embedder failed via MQTT. Falling back to zero-vector.", e);
            }

            const pointId = crypto.randomUUID();
            
            const payloadData: any = {
                content: content,
                user_id: this.currentNamespace,
                metadata: {}
            };
            
            if (refFile) {
                payloadData.metadata.refs = [{ file: refFile }];
                if (refLines) payloadData.metadata.refs[0].lines = refLines;
            }

            try {
                if (this.plugin.mqtt) {
                    await this.plugin.mqtt.request('add', {
                        id: pointId,
                        vector: vector,
                        payload: payloadData
                    });
                    new Notice(`Memory added to ${this.currentNamespace}`);
                    this.addForm.style.display = 'none';
                    
                    setTimeout(() => {
                        this.loadMemories();
                    }, 250);
                }
            } catch (e) {
                new Notice(`Failed to save memory: ${e}`);
            }

            saveNewBtn.innerText = "Save Memory";
            saveNewBtn.disabled = false;
        };

        refreshBtn.onclick = async () => {
            this.searchInput.value = '';
            await this.loadNamespaces();
            this.loadMemories();
        };

        this.memoriesContainer = container.createDiv();
        
        setTimeout(async () => {
            await this.loadNamespaces();
            await this.loadMemories();
        }, 500); // Give MQTT time to connect
    }

    public openAddMemoryForm(initialText: string, referencePath: string, linesStr: string) {
        if (!this.addForm) return;
        
        this.addForm.style.display = 'block';
        this.addTextarea.value = initialText;
        this.refInput.value = referencePath;
        this.linesInput.value = linesStr;
        this.addTextarea.focus();
    }

    async loadNamespaces() {
        try {
            if (!this.plugin.mqtt) return;
            const data = await this.plugin.mqtt.request('list', {});
            
            const uniqueNamespaces = new Set<string>();
            uniqueNamespaces.add('global'); // Always ensure 'global' exists
            
            if (data && Array.isArray(data)) {
                data.forEach((p: any) => {
                    if (p.payload && p.payload.user_id) {
                        uniqueNamespaces.add(p.payload.user_id);
                    }
                });
            }
            
            const namespaces = Array.from(uniqueNamespaces).sort();
            
            this.namespaceSelect.empty();
            namespaces.forEach(ns => {
                const option = this.namespaceSelect.createEl("option", { text: ns, value: ns });
                if (ns === this.currentNamespace) {
                    option.selected = true;
                }
            });
            
            if (!namespaces.includes(this.currentNamespace)) {
                this.currentNamespace = 'global';
                this.namespaceSelect.value = 'global';
            }
            
        } catch (e) {
            console.error("NeuroStrata: Failed to load namespaces from MQTT", e);
            this.namespaceSelect.empty();
            this.namespaceSelect.createEl("option", { text: "global", value: "global" });
            this.currentNamespace = 'global';
        }
    }

    async loadMemories() {
        this.memoriesContainer.empty();
        this.memoriesContainer.createEl("p", { text: "Loading..." });

        try {
            if (!this.plugin.mqtt) throw new Error("MQTT client not ready");
            const data = await this.plugin.mqtt.request('list', { namespace: this.currentNamespace });
            this.allCurrentPoints = Array.isArray(data) ? data : [];

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
        
        const filteredPoints = this.allCurrentPoints.filter(point => {
            const textData = point.payload?.content || "";
            return textData.toLowerCase().includes(filterText);
        });

        if (filteredPoints.length === 0) {
            this.memoriesContainer.createEl("p", { text: "No memories match your filter." });
            return;
        }

        filteredPoints.forEach((point: any) => this.renderMemoryCard(point));
    }

    renderMemoryCard(point: any) {
        const card = this.memoriesContainer.createDiv({ cls: "neurostrata-memory-card" });
        let textData = point.payload?.content || "";
        let refs = point.payload?.metadata?.refs || [];

        const textDisplay = card.createDiv({ cls: "neurostrata-memory-text", text: textData });
        textDisplay.style.whiteSpace = "pre-wrap";
        textDisplay.style.marginBottom = "8px";
        
        const badgeContainer = card.createDiv({ cls: "neurostrata-refs-container" });
        
        const renderBadges = (currentRefs: any[]) => {
            badgeContainer.empty();
            if (currentRefs.length > 0) {
                currentRefs.forEach((r: any) => {
                    if (!r.file) return;
                    
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
                    badge.innerText = `📄 ${r.file.split('/').pop()}${linesTxt}`;
                    
                    badge.onclick = (e) => {
                        e.preventDefault();
                        this.app.workspace.openLinkText(r.file, "", true);
                    };
                });
            }
        };
        renderBadges(refs);

        const editContainer = card.createDiv();
        editContainer.style.display = 'none';

        const editArea = editContainer.createEl("textarea", { cls: "neurostrata-memory-edit-area" });
        editArea.value = textData;

        const editMetaContainer = editContainer.createDiv();
        editMetaContainer.style.display = "grid";
        editMetaContainer.style.gridTemplateColumns = "1fr 1fr";
        editMetaContainer.style.gap = "8px";
        editMetaContainer.style.marginTop = "10px";

        const editRefDiv = editMetaContainer.createDiv();
        editRefDiv.createEl("div", { text: "File:", cls: "neurostrata-memory-text" }).style.fontSize = "var(--font-ui-smaller)";
        const editRefInput = editRefDiv.createEl("input", { type: "text", cls: "neurostrata-memory-edit-area" });
        editRefInput.style.minHeight = "auto"; editRefInput.style.padding = "5px";
        
        const editLinesDiv = editMetaContainer.createDiv();
        editLinesDiv.createEl("div", { text: "Lines:", cls: "neurostrata-memory-text" }).style.fontSize = "var(--font-ui-smaller)";
        const editLinesInput = editLinesDiv.createEl("input", { type: "text", cls: "neurostrata-memory-edit-area" });
        editLinesInput.style.minHeight = "auto"; editLinesInput.style.padding = "5px";

        if (refs.length > 0) {
            editRefInput.value = refs[0].file || "";
            editLinesInput.value = refs[0].lines || "";
        }

        const btnGroup = card.createDiv({ cls: "neurostrata-button-group" });
        btnGroup.style.marginTop = "10px";
        
        const editBtn = btnGroup.createEl("button", { text: "Edit", cls: "neurostrata-btn" });
        const deleteBtn = btnGroup.createEl("button", { text: "Delete", cls: "neurostrata-btn neurostrata-btn-delete" });
        const saveBtn = btnGroup.createEl("button", { text: "Save", cls: "neurostrata-btn" });
        const cancelBtn = btnGroup.createEl("button", { text: "Cancel", cls: "neurostrata-btn" });

        saveBtn.style.display = 'none';
        cancelBtn.style.display = 'none';

        editBtn.onclick = () => {
            textDisplay.style.display = 'none';
            badgeContainer.style.display = 'none';
            editContainer.style.display = 'block';
            
            editBtn.style.display = 'none';
            deleteBtn.style.display = 'none';
            saveBtn.style.display = 'inline-block';
            cancelBtn.style.display = 'inline-block';
        };

        cancelBtn.onclick = () => {
            textDisplay.style.display = 'block';
            badgeContainer.style.display = 'block';
            editContainer.style.display = 'none';
            
            saveBtn.style.display = 'none';
            cancelBtn.style.display = 'none';
            editBtn.style.display = 'inline-block';
            deleteBtn.style.display = 'inline-block';
        };

        saveBtn.onclick = async () => {
            const newText = editArea.value.trim();
            saveBtn.innerText = "Saving & Re-embedding...";
            saveBtn.disabled = true;
            cancelBtn.disabled = true;
            
            const newPayload: any = { content: newText, user_id: this.currentNamespace, metadata: {} };
            
            if (editRefInput.value.trim()) {
                newPayload.metadata.refs = [{ file: editRefInput.value.trim() }];
                if (editLinesInput.value.trim()) {
                    newPayload.metadata.refs[0].lines = editLinesInput.value.trim();
                }
            }

            let newVector = Array(768).fill(0);
            try {
                if (this.plugin.mqtt) {
                    const embRes = await this.plugin.mqtt.request('embed', { input: newText });
                    if (embRes && embRes.embedding) {
                        newVector = embRes.embedding;
                    }
                }
            } catch (e) {
                console.warn("Re-embedding failed via MQTT. Fallback to zero-vector.", e);
            }

            try {
                if (this.plugin.mqtt) {
                    await this.plugin.mqtt.request('update', {
                        id: point.id,
                        vector: newVector,
                        payload: newPayload
                    });
                }
                
                new Notice("Memory updated successfully.");
                
                textData = newText;
                refs = newPayload.metadata.refs || [];
                point.payload = newPayload; 
                
                textDisplay.innerText = textData;
                renderBadges(refs);
                
                textDisplay.style.display = 'block';
                badgeContainer.style.display = 'block';
                editContainer.style.display = 'none';
                
                saveBtn.style.display = 'none';
                cancelBtn.style.display = 'none';
                editBtn.style.display = 'inline-block';
                deleteBtn.style.display = 'inline-block';
                
                if (this.plugin.settings.autoUpdateCanvas) {
                    await this.plugin.generateCanvas(this.allCurrentPoints, true);
                }

            } catch(e) {
                new Notice(`Failed to update: ${e}`);
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
                    if (this.plugin.mqtt) {
                        await this.plugin.mqtt.request('delete', { id: point.id });
                    }
                    card.remove();
                    this.allCurrentPoints = this.allCurrentPoints.filter(p => p.id !== point.id);
                    new Notice("Memory deleted.");
                    
                    if (this.plugin.settings.autoUpdateCanvas) {
                        await this.plugin.generateCanvas(this.allCurrentPoints, true);
                    }
                } catch (e) {
                    new Notice(`Failed to delete: ${e}`);
                    deleteBtn.innerText = "Delete";
                    deleteBtn.disabled = false;
                }
            }
        };
    }
}

class NeuroStrataSettingTab extends PluginSettingTab {
    plugin: NeuroStrataPlugin;

    constructor(app: App, plugin: NeuroStrataPlugin) {
        super(app, plugin);
        this.plugin = plugin;
    }

    display(): void {
        const {containerEl} = this;
        containerEl.empty();
        containerEl.createEl('h2', {text: 'NeuroStrata Settings'});

        new Setting(containerEl)
            .setName('MQTT Broker URL (WebSocket)')
            .setDesc('The WebSocket URL for the embedded NeuroStrata MQTT broker')
            .addText(text => text
                .setPlaceholder('ws://127.0.0.1:8081')
                .setValue(this.plugin.settings.mqttUrl)
                .onChange(async (value) => {
                    this.plugin.settings.mqttUrl = value;
                    await this.plugin.saveSettings();
                    this.plugin.initMqtt();
                }));
                
        new Setting(containerEl)
            .setName('Auto-Update Canvas')
            .setDesc('Automatically regenerate the "NeuroStrata MemorySpace.canvas" file when modifying memories.')
            .addToggle(toggle => toggle
                .setValue(this.plugin.settings.autoUpdateCanvas)
                .onChange(async (value) => {
                    this.plugin.settings.autoUpdateCanvas = value;
                    await this.plugin.saveSettings();
                }));
    }
}

export default class NeuroStrataPlugin extends Plugin {
    settings: NeuroStrataPluginSettings;
    mqtt: MqttClientWrapper | null = null;

    async onload() {
        await this.loadSettings();
        this.initMqtt();
        
        addIcon('neurostrata-brain', NEUROSTRATA_ICON_SVG);

        this.addSettingTab(new NeuroStrataSettingTab(this.app, this));

        this.registerView(
            VIEW_TYPE_NEUROSTRATA,
            (leaf) => new NeuroStrataView(leaf, this)
        );

        this.addRibbonIcon('neurostrata-brain', 'Open NeuroStrata Inspector', () => {
            this.activateView();
        });

        this.registerEvent(
            this.app.workspace.on("editor-menu", (menu, editor, view) => {
                const selection = editor.getSelection();
                if (selection && selection.trim().length > 0) {
                    menu.addItem((item) => {
                        item
                            .setTitle("Create NeuroStrata Memory (Paragraph)")
                            .setIcon("neurostrata-brain")
                            .onClick(async () => {
                                const selections = editor.listSelections();
                                let linesStr = "";
                                
                                if (selections.length > 0) {
                                    let startLine = Math.min(selections[0].anchor.line, selections[0].head.line);
                                    let endLine = Math.max(selections[0].anchor.line, selections[0].head.line);
                                    
                                    while(startLine > 0 && editor.getLine(startLine - 1).trim() !== "") {
                                        startLine--;
                                    }
                                    
                                    const lineCount = editor.lineCount();
                                    while(endLine < lineCount - 1 && editor.getLine(endLine + 1).trim() !== "") {
                                        endLine++;
                                    }
                                    
                                    linesStr = `${startLine + 1}-${endLine + 1}`;
                                }
                                
                                const viewInstance = await this.activateView();
                                if (viewInstance && viewInstance instanceof NeuroStrataView) {
                                    const filePath = view.file ? view.file.path : "";
                                    viewInstance.openAddMemoryForm(selection, filePath, linesStr);
                                }
                            });
                    });
                }
            })
        );
    }

    initMqtt() {
        if (this.mqtt) {
            this.mqtt.client.end();
        }
        this.mqtt = new MqttClientWrapper(this.settings.mqttUrl);
    }

    onunload() {
        if (this.mqtt) {
            this.mqtt.client.end();
        }
    }

    async generateCanvas(points: any[], silent: boolean = false) {
        const domainMap = new Map<string, any[]>();
        const orphanedMemories: any[] = [];

        points.forEach(p => {
            const refs = p.payload?.metadata?.refs || [];
            let foundDoc = false;
            
            if (refs.length > 0) {
                refs.forEach((r: any) => {
                    if (r.file) {
                        const docPath = r.file;
                        if (!domainMap.has(docPath)) domainMap.set(docPath, []);
                        domainMap.get(docPath)!.push(p);
                        foundDoc = true;
                    }
                });
            }
            
            if (!foundDoc) {
                orphanedMemories.push(p);
            }
        });

        const nodes: any[] = [];
        const edges: any[] = [];
        
        let startX = -1000;
        let startY = -1000;
        
        const DOC_WIDTH = 400;
        const DOC_HEIGHT = 400;
        const MEM_WIDTH = 300;
        const MEM_HEIGHT = 150;
        const X_GAP = 500;
        const Y_GAP = 200;

        let maxStructuredY = startY + DOC_HEIGHT;

        Array.from(domainMap.keys()).forEach((docPath, colIndex) => {
            const x = startX + (colIndex * (DOC_WIDTH + X_GAP));
            const y = startY;
            
            const docNodeId = `doc-${colIndex}`;
            nodes.push({
                id: docNodeId,
                type: "file",
                file: docPath,
                x: x,
                y: y,
                width: DOC_WIDTH,
                height: DOC_HEIGHT,
                color: "4" 
            });
            
            const memories = domainMap.get(docPath)!;
            memories.forEach((mem, rowIndex) => {
                const memNodeId = `mem-${mem.id}`;
                const memX = x + DOC_WIDTH + 100;
                const memY = y + (rowIndex * (MEM_HEIGHT + 50));
                
                const rawText = mem.payload?.content || "";

                nodes.push({
                    id: memNodeId,
                    type: "text",
                    text: `**Memory [${mem.payload?.user_id || 'unknown'}]**\n\n${rawText}`,
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

            const columnMaxY = y + (memories.length * (MEM_HEIGHT + 50));
            if (columnMaxY > maxStructuredY) {
                maxStructuredY = columnMaxY;
            }
        });
        
        if (orphanedMemories.length > 0) {
            const orphStartY = maxStructuredY + 400; 
            const cols = 4;
            const rows = Math.ceil(orphanedMemories.length / cols);
            
            const orphGroupWidth = (cols * MEM_WIDTH) + ((cols - 1) * 50) + 100;
            const orphGroupHeight = (rows * MEM_HEIGHT) + ((rows - 1) * 50) + 100;

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
                const col = index % cols;
                const row = Math.floor(index / cols);
                
                nodes.push({
                    id: `mem-${mem.id}`,
                    type: "text",
                    text: `**Orphaned [${mem.payload?.user_id || 'unknown'}]**\n\n${mem.payload?.content || ""}`,
                    x: startX + (col * (MEM_WIDTH + 50)),
                    y: orphStartY + (row * (MEM_HEIGHT + 50)),
                    width: MEM_WIDTH,
                    height: MEM_HEIGHT,
                    color: "1" 
                });
            });
        }

        const canvasData = {
            nodes: nodes,
            edges: edges
        };

        const fileData = JSON.stringify(canvasData, null, 2);
        
        let file = this.app.vault.getAbstractFileByPath(CANVAS_FILE_NAME) as TFile;
        
        try {
            if (file) {
                await this.app.vault.modify(file, fileData);
            } else {
                file = await this.app.vault.create(CANVAS_FILE_NAME, fileData);
            }
            if (!silent) {
                new Notice("NeuroStrata MemorySpace generated successfully!");
                this.app.workspace.getLeaf(false).openFile(file);
            }
        } catch (e) {
            console.error("Failed to generate MemorySpace:", e);
            if (!silent) new Notice("Failed to generate MemorySpace.");
        }
    }

    async loadSettings() {
        this.settings = Object.assign({}, DEFAULT_SETTINGS, await this.loadData());
    }

    async saveSettings() {
        await this.saveData(this.settings);
        
        const leaves = this.app.workspace.getLeavesOfType(VIEW_TYPE_NEUROSTRATA);
        for (const leaf of leaves) {
            if (leaf.view instanceof NeuroStrataView) {
                leaf.view.loadMemories();
            }
        }
    }

    async activateView(): Promise<NeuroStrataView | null> {
        const { workspace } = this.app;
        
        let leaf: WorkspaceLeaf | null = null;
        const leaves = workspace.getLeavesOfType(VIEW_TYPE_NEUROSTRATA);
        
        if (leaves.length > 0) {
            leaf = leaves[0];
        } else {
            leaf = workspace.getRightLeaf(false);
            if (leaf) {
                await leaf.setViewState({ type: VIEW_TYPE_NEUROSTRATA, active: true });
            }
        }
        
        if (leaf) {
            workspace.revealLeaf(leaf);
            return leaf.view as NeuroStrataView;
        }
        return null;
    }
}
