import { useState, useMemo, useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { GalaxyGraph3D } from './components/GalaxyGraph3D';
import { BlueprintGraph2D } from './components/BlueprintGraph2D';
import { UIPanel } from './components/UIPanel';
import { FileExplorer } from './components/FileExplorer';
import type { GraphData, MemoryNode, MemoryLink } from './types';

function App() {
  const [graphData, setGraphData] = useState<GraphData>({ nodes: [], links: [] });
  const [viewMode, setViewMode] = useState<'2d' | '3d'>('3d');
  const [selectedNode, setSelectedNode] = useState<MemoryNode | null>(null);
  const [selectedLink, setSelectedLink] = useState<MemoryLink | null>(null);
  
  const [namespaceFilters, setNamespaceFilters] = useState<Record<string, boolean>>({});
  const [typeFilters, setTypeFilters] = useState<Record<string, boolean>>({});
  const [projectPath, setProjectPath] = useState<string | null>(null);
  const [isIngesting, setIsIngesting] = useState(false);

  // Context Menu State
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; node: MemoryNode } | null>(null);

  // Edit Modal State
  const [editModal, setEditModal] = useState<{ isOpen: boolean; node: MemoryNode | null; namespace: string; content: string; location: string }>({
    isOpen: false,
    node: null,
    namespace: '',
    content: '',
    location: ''
  });

  const loadGraph = async (path: string | null) => {
    try {
      const data: GraphData = await invoke('get_graph', { projectPath: path });
      
      data.nodes.forEach(n => {
        if (!n.name) {
          n.name = n.id.split(/[/\\]/).pop() || n.id;
        }
        n.degree = 0;
      });

      data.links.forEach(l => {
        const sourceId = typeof l.source === 'object' ? (l.source as any).id : l.source;
        const targetId = typeof l.target === 'object' ? (l.target as any).id : l.target;
        
        const sNode = data.nodes.find(n => n.id === sourceId);
        const tNode = data.nodes.find(n => n.id === targetId);
        if (sNode) sNode.degree++;
        if (tNode) tNode.degree++;
      });

      setGraphData(data);
      invoke('log_message', { msg: `Loaded graph: ${data.nodes.length} nodes, ${data.links.length} links` });
      invoke('log_message', { msg: `Sample node: ${JSON.stringify(data.nodes[0])}` });
      invoke('log_message', { msg: `Sample link: ${JSON.stringify(data.links[0])}` });
      
      const uniqueNamespaces = Array.from(new Set(data.nodes.map(n => n.namespace).filter(Boolean))) as string[];
      setNamespaceFilters(prev => {
        const next: Record<string, boolean> = {};
        uniqueNamespaces.forEach(ns => {
          next[ns] = prev[ns] !== undefined ? prev[ns] : true;
        });
        return next;
      });

      const uniqueTypes = Array.from(new Set(data.nodes.map(n => n.memory_type).filter(Boolean))) as string[];
      setTypeFilters(prev => {
        const next: Record<string, boolean> = {};
        uniqueTypes.forEach(t => {
          next[t] = prev[t] !== undefined ? prev[t] : true;
        });
        return next;
      });
    } catch (e) {
      invoke('log_message', { msg: `Failed to load graph from Ladybug: ${e}` });
      console.error("Failed to load graph from Ladybug", e);
    }
  };

  useEffect(() => {
    invoke('log_message', { msg: "Registering open-project-dialog listener" });
    const unlistenMenu = listen('open-project-dialog', async (e) => {
      invoke('log_message', { msg: `RECEIVED open-project-dialog: ${JSON.stringify(e)}` });
      try {
        const selected = await open({
          directory: true,
          multiple: false,
        });
        invoke('log_message', { msg: `Selected directory: ${selected}` });
        if (selected && typeof selected === 'string') {
          setProjectPath(selected);
          await invoke('save_project_path', { path: selected });
          
          // Trigger AST ingest automatically on load
          setIsIngesting(true);
          try {
            await invoke('ingest_ast', { projectPath: selected });
            invoke('log_message', { msg: "AST ingestion completed successfully." });
          } catch (e) {
            invoke('log_message', { msg: `AST Ingestion failed: ${e}` });
          } finally {
            setIsIngesting(false);
          }
          await loadGraph(selected);
        }
      } catch (e) {
        invoke('log_message', { msg: `Failed to open project dialog: ${e}` });
      }
    });

    return () => {
      invoke('log_message', { msg: "Unregistering open-project-dialog listener" });
      unlistenMenu.then(f => f());
    };
  }, []);

  // Initial load
  useEffect(() => {
    const init = async () => {
      try {
        const path: string | null = await invoke('get_last_project_path');
        if (path) {
          setProjectPath(path);
          setIsIngesting(true);
          try {
            await invoke('ingest_ast', { projectPath: path });
            console.log("AST ingestion completed successfully.");
          } catch (e) {
            console.error("AST Ingestion failed", e);
          } finally {
            setIsIngesting(false);
          }
          await loadGraph(path);
        } else {
          await loadGraph(null);
        }
      } catch (e) {
        console.error("Failed to init", e);
        await loadGraph(null);
      }
    };
    init();
  }, []);

  const filteredData = useMemo(() => {
    const nodes = graphData.nodes.filter(n => {
      if (n.namespace && namespaceFilters[n.namespace] === false) return false;
      if (typeFilters[n.memory_type] === false) return false;
      return true;
    });
    
    const nodeIds = new Set(nodes.map(n => n.id));
    const links = graphData.links.filter(l => 
      nodeIds.has(typeof l.source === 'object' ? (l.source as any).id : l.source) && 
      nodeIds.has(typeof l.target === 'object' ? (l.target as any).id : l.target)
    );
    
    invoke('log_message', { msg: `Filtered data has ${nodes.length} nodes and ${links.length} links` });
    return { nodes, links };
  }, [graphData, typeFilters, namespaceFilters]);

  const handleNodeClick = (node: MemoryNode) => {
    setSelectedNode(node);
    setSelectedLink(null);
    setContextMenu(null);
  };

  const handleLinkClick = (link: MemoryLink) => {
    setSelectedLink(link);
    setSelectedNode(null);
    setContextMenu(null);
  };

  const handleContextMenu = (e: React.MouseEvent, node: MemoryNode) => {
    e.preventDefault();
    setContextMenu({ x: e.clientX, y: e.clientY, node });
  };

  const handleDelete = async () => {
    if (!contextMenu) return;
    const node = contextMenu.node;
    setContextMenu(null);
    if (window.confirm(`Are you sure you want to delete this memory?`)) {
      try {
        setIsIngesting(true);
        await invoke('delete_memory', { namespace: node.namespace || 'global', id: node.id });
        await loadGraph(projectPath);
      } catch (e) {
        console.error("Delete failed", e);
        alert("Failed to delete memory: " + e);
      } finally {
        setIsIngesting(false);
      }
    }
  };

  const openEditModal = () => {
    if (!contextMenu) return;
    const node = contextMenu.node;
    setEditModal({
      isOpen: true,
      node,
      namespace: node.namespace || 'global',
      content: node.content || '',
      location: node.location || ''
    });
    setContextMenu(null);
  };

  const handleEditSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!editModal.node) return;
    try {
      setIsIngesting(true);
      setEditModal(prev => ({ ...prev, isOpen: false }));
      await invoke('edit_memory', {
        oldNamespace: editModal.node.namespace || 'global',
        id: editModal.node.id,
        newNamespace: editModal.namespace,
        content: editModal.content,
        location: editModal.location
      });
      await loadGraph(projectPath);
    } catch (err) {
      console.error("Edit failed", err);
      alert("Failed to edit memory: " + err);
    } finally {
      setIsIngesting(false);
    }
  };

  return (
    <div className="relative w-full h-screen overflow-hidden flex" onClick={() => setContextMenu(null)}>
      {isIngesting && (
        <div className="absolute top-4 left-1/2 -translate-x-1/2 z-50 bg-indigo-500/20 backdrop-blur-md border border-indigo-500/30 text-indigo-200 px-4 py-2 rounded-full font-mono text-sm flex items-center gap-2">
          <div className="w-4 h-4 rounded-full border-2 border-indigo-400 border-t-transparent animate-spin" />
          Ingesting AST & refreshing Graph...
        </div>
      )}
      
      <div className="absolute inset-0 z-0">
        {viewMode === '3d' ? (
          <GalaxyGraph3D data={filteredData} selectedNode={selectedNode} onNodeClick={handleNodeClick} onLinkClick={handleLinkClick} />
        ) : (
          <BlueprintGraph2D data={filteredData} selectedNode={selectedNode} onNodeClick={handleNodeClick} onLinkClick={handleLinkClick} />
        )}
      </div>

      <div className="absolute inset-y-0 left-0 w-96 p-6 z-10 pointer-events-none">
        <FileExplorer 
          nodes={graphData.nodes} 
          selectedNode={selectedNode} 
          onNodeSelect={handleNodeClick} 
          onContextMenu={handleContextMenu}
        />
      </div>

      {/* Context Menu */}
      {contextMenu && (
        <div 
          className="absolute z-50 bg-[#0f172a] backdrop-blur-xl border border-white/20 rounded-lg shadow-xl overflow-hidden py-1 min-w-[150px]"
          style={{ top: Math.min(contextMenu.y, window.innerHeight - 100), left: Math.min(contextMenu.x, window.innerWidth - 200) }}
          onClick={(e) => e.stopPropagation()}
        >
          <button 
            className="w-full text-left px-4 py-2 text-sm text-white hover:bg-white/10 transition-colors font-medium flex items-center gap-2"
            onClick={(e) => { e.stopPropagation(); openEditModal(); }}
          >
            <span>✏️</span> Edit Memory
          </button>
          <button 
            className="w-full text-left px-4 py-2 text-sm text-red-400 hover:bg-red-500/20 transition-colors font-medium flex items-center gap-2 border-t border-white/10 mt-1 pt-2"
            onClick={(e) => { e.stopPropagation(); handleDelete(); }}
          >
            <span>🗑️</span> Delete Memory
          </button>
        </div>
      )}

      {/* Edit Modal */}
      {editModal.isOpen && (
        <div className="absolute inset-0 z-[100] flex items-center justify-center bg-black/60 backdrop-blur-sm p-4">
          <div className="bg-[#0f172a] border border-white/20 p-6 rounded-2xl shadow-2xl w-full max-w-2xl" onClick={e => e.stopPropagation()}>
            <h2 className="text-2xl font-bold mb-6 text-blue-300">Edit Memory</h2>
            <form onSubmit={handleEditSubmit} className="flex flex-col gap-4">
              <div>
                <label className="block text-sm font-medium text-gray-300 mb-1">Namespace</label>
                <input 
                  type="text" 
                  value={editModal.namespace} 
                  onChange={e => setEditModal(m => ({ ...m, namespace: e.target.value }))}
                  className="w-full bg-black/40 border border-white/10 rounded px-3 py-2 text-white focus:outline-none focus:border-blue-500"
                />
              </div>
              <div>
                <label className="block text-sm font-medium text-gray-300 mb-1">Location / Link</label>
                <input 
                  type="text" 
                  value={editModal.location} 
                  onChange={e => setEditModal(m => ({ ...m, location: e.target.value }))}
                  className="w-full bg-black/40 border border-white/10 rounded px-3 py-2 text-white focus:outline-none focus:border-blue-500"
                />
              </div>
              <div>
                <label className="block text-sm font-medium text-gray-300 mb-1">Content</label>
                <textarea 
                  rows={8}
                  value={editModal.content} 
                  onChange={e => setEditModal(m => ({ ...m, content: e.target.value }))}
                  className="w-full bg-black/40 border border-white/10 rounded px-3 py-2 text-white focus:outline-none focus:border-blue-500 custom-scrollbar"
                />
              </div>
              <div className="flex justify-end gap-3 mt-4">
                <button 
                  type="button"
                  onClick={() => setEditModal(m => ({ ...m, isOpen: false }))}
                  className="px-4 py-2 rounded text-gray-300 hover:bg-white/10 transition-colors"
                >
                  Cancel
                </button>
                <button 
                  type="submit"
                  className="px-4 py-2 rounded bg-blue-600 hover:bg-blue-500 text-white font-medium transition-colors shadow-lg"
                >
                  Save Changes
                </button>
              </div>
            </form>
          </div>
        </div>
      )}

      <UIPanel 
        viewMode={viewMode}
        setViewMode={setViewMode}
        selectedNode={selectedNode}
        selectedLink={selectedLink}
        typeFilters={typeFilters}
        setTypeFilters={setTypeFilters}
        namespaceFilters={namespaceFilters}
        setNamespaceFilters={setNamespaceFilters}
      />
    </div>
  );
}

export default App;
