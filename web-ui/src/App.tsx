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

  useEffect(() => {
    // Listen for the native menu event to trigger the dialog
    const unlistenMenu = listen('open-project-dialog', async () => {
      try {
        const selected = await open({
          directory: true,
          multiple: false,
        });
        if (selected && typeof selected === 'string') {
          setProjectPath(selected);
          await invoke('save_project_path', { path: selected });
        }
      } catch (e) {
        console.error('Failed to open project dialog', e);
      }
    });

    // Listen for the initial load of the project path from Rust
    const unlistenPath = listen<{path: string}>('load-project-path', (event) => {
      setProjectPath(event.payload.path);
    });

    return () => {
      unlistenMenu.then(f => f());
      unlistenPath.then(f => f());
    };
  }, []);

  useEffect(() => {
    fetch('/graph.json')
      .then(res => res.json())
      .then((data: GraphData) => {
        
        // Backwards compatibility & new mechanics parsing
        data.nodes.forEach(n => {
          // If node doesn't have a name, derive it from the ID
          if (!n.name) {
            n.name = n.id.split(/[/\\]/).pop() || n.id;
          }
          n.degree = 0;
        });

        // Compute degree for node sizing based on Kuzu edges
        data.links.forEach(l => {
          const sourceId = typeof l.source === 'object' ? (l.source as any).id : l.source;
          const targetId = typeof l.target === 'object' ? (l.target as any).id : l.target;
          
          const sNode = data.nodes.find(n => n.id === sourceId);
          const tNode = data.nodes.find(n => n.id === targetId);
          if (sNode) sNode.degree++;
          if (tNode) tNode.degree++;
        });

        setGraphData(data);
        
        // Dynamically initialize namespace filters based on data
        const uniqueNamespaces = Array.from(new Set(data.nodes.map(n => n.namespace).filter(Boolean))) as string[];
        setNamespaceFilters(prev => {
          const next = { ...prev };
          uniqueNamespaces.forEach(ns => {
            if (next[ns] === undefined) next[ns] = true;
          });
          return next;
        });

        // Dynamically initialize type filters
        const uniqueTypes = Array.from(new Set(data.nodes.map(n => n.memory_type).filter(Boolean))) as string[];
        setTypeFilters(prev => {
          const next = { ...prev };
          uniqueTypes.forEach(t => {
            // Default to true
            if (next[t] === undefined) {
              next[t] = true;
            }
          });
          return next;
        });
      })
      .catch(e => console.error("Failed to load graph data", e));
  }, []);

  const filteredData = useMemo(() => {
    const nodes = graphData.nodes.filter(n => {
      // If namespace filter exists and is false, hide the node
      if (n.namespace && namespaceFilters[n.namespace] === false) return false;
      // If type filter is false, hide the node
      if (typeFilters[n.memory_type] === false) return false;
      
      // Filter by projectPath if set
      if (projectPath) {
        // Assume global memories have namespace 'global' or no namespace
        // For project-specific memories, we might need a way to match path.
        // If a node's path starts with projectPath, or it's global.
        const isGlobal = n.namespace === 'global' || !n.namespace;
        // If it's not global, check if its path starts with the projectPath or namespace matches
        // The prompt says: "filter the loaded graph to only show global memories and memories specific to that project."
        // Usually project specific memory has namespace equal to project name or path.
        // For simplicity, let's keep ones where n.namespace is global, or if they have a path that contains the project name.
        // Let's just do a simple filter for now.
        const projectName = projectPath.split(/[/\\]/).pop();
        if (!isGlobal && n.namespace !== projectName) {
           // Also allow if there's no namespace but path matches
           if (n.path && !n.path.includes(projectName || projectPath)) {
               // well, if we have namespace, it should match the project name
               return false;
           }
        }
      }
      return true;
    });
    
    const nodeIds = new Set(nodes.map(n => n.id));
    const links = graphData.links.filter(l => 
      nodeIds.has(typeof l.source === 'object' ? (l.source as any).id : l.source) && 
      nodeIds.has(typeof l.target === 'object' ? (l.target as any).id : l.target)
    );
    
    return { nodes, links };
  }, [graphData, typeFilters, namespaceFilters, projectPath]);

  const handleNodeClick = (node: MemoryNode) => {
    setSelectedNode(node);
    setSelectedLink(null);
  };

  const handleLinkClick = (link: MemoryLink) => {
    setSelectedLink(link);
    setSelectedNode(null);
  };

  return (
    <div className="relative w-full h-screen overflow-hidden flex">
      <div className="absolute inset-0 z-0">
        {viewMode === '3d' ? (
          <GalaxyGraph3D data={filteredData} selectedNode={selectedNode} onNodeClick={handleNodeClick} onLinkClick={handleLinkClick} />
        ) : (
          <BlueprintGraph2D data={filteredData} selectedNode={selectedNode} onNodeClick={handleNodeClick} onLinkClick={handleLinkClick} />
        )}
      </div>

      {/* Left Pane: File Explorer */}
      <div className="z-10 p-6 pointer-events-none h-full">
        <FileExplorer 
          nodes={graphData.nodes} 
          selectedNode={selectedNode} 
          onNodeSelect={handleNodeClick} 
        />
      </div>

      {/* Right Pane: Filters and Details */}
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
