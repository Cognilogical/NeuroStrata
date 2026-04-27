import React, { useState, useMemo } from 'react';
import type { MemoryNode } from '../types';

interface Props {
  nodes: MemoryNode[];
  selectedNode: MemoryNode | null;
  onNodeSelect: (node: MemoryNode) => void;
  onContextMenu?: (e: React.MouseEvent, node: MemoryNode) => void;
}

interface TreeItem {
  normalizedPath: string;
  originalNode: MemoryNode | null;
  displayName: string;
  isDirectory: boolean;
  type: string;
  tooltip?: string;
}

function getCommonPrefix(paths: string[]) {
  if (paths.length === 0) return '';
  let prefix = paths[0];
  for (let i = 1; i < paths.length; i++) {
    while (paths[i].indexOf(prefix) !== 0) {
      prefix = prefix.substring(0, prefix.length - 1);
      if (prefix === '') return '';
    }
  }
  if (!prefix.endsWith('/')) {
    const lastSlash = prefix.lastIndexOf('/');
    if (lastSlash !== -1) {
      prefix = prefix.substring(0, lastSlash + 1);
    }
  }
  return prefix;
}

export const FileExplorer: React.FC<Props> = ({ nodes, selectedNode, onNodeSelect, onContextMenu }) => {
  const [expandedDirs, setExpandedDirs] = useState<Record<string, boolean>>({});

  const panelGlassClass = "p-5 rounded-2xl bg-black/60 backdrop-blur-xl border border-white/20 shadow-[0_8px_32px_rgba(0,0,0,0.5),inset_0_1px_1px_rgba(255,255,255,0.4),inset_0_-1px_1px_rgba(255,255,255,0.1)] text-white";

  const items = useMemo(() => {
    const isPhysical = (type: string) => ['directory', 'markdown', 'code_ast', 'file'].includes(type);
    
    const physicalNodes = nodes.filter(n => isPhysical(n.memory_type));
    const memoryNodes = nodes.filter(n => !isPhysical(n.memory_type));
    
    const commonPrefix = getCommonPrefix(physicalNodes.map(n => n.id));
    const rootName = commonPrefix.split('/').filter(Boolean).pop() || 'Project';

    const treeItems: TreeItem[] = [];
    const synthesizedDirs = new Set<string>();

    // 1. Process Physical Nodes
    physicalNodes.forEach(n => {
      // e.g. "/home/kenton/NeuroStrata/src/main.rs" -> "NeuroStrata/src/main.rs"
      let relPath = n.id.substring(commonPrefix.length);
      if (relPath.startsWith('/')) relPath = relPath.substring(1);
      
      const normalizedPath = `${rootName}/${relPath}`;
      treeItems.push({
        normalizedPath,
        originalNode: n,
        displayName: n.id.split(/[/\\]/).pop() || n.id,
        isDirectory: n.memory_type === 'directory',
        type: n.memory_type,
        tooltip: n.id
      });
    });

    // 2. Process Memory Nodes
    if (memoryNodes.length > 0) {
      synthesizedDirs.add('Memories');
      
      memoryNodes.forEach(n => {
        // e.g. "Memories/Global/1234-abcd"
        const ns = n.namespace || 'Global';
        // Capitalize namespace if it's "global"
        const folderName = ns.toLowerCase() === 'global' ? 'Global' : ns;
        const parentDir = `Memories/${folderName}`;
        
        synthesizedDirs.add(parentDir);

        let shortContent = (n.content || '').substring(0, 150);
        if (n.content && n.content.length > 150) shortContent += '...';

        let tooltip = shortContent || 'Memory Node';
        if (n.location) tooltip += `\n\nLink: ${n.location}`;
        tooltip += `\nNamespace: ${ns}`;

        treeItems.push({
          normalizedPath: `${parentDir}/${n.id}`,
          originalNode: n,
          displayName: n.name || n.id,
          isDirectory: false,
          type: 'memory',
          tooltip: tooltip
        });
      });
    }

    // 3. Add Synthesized Directories (like 'Memories', 'Memories/Global', etc.)
    synthesizedDirs.forEach(dir => {
      treeItems.push({
        normalizedPath: dir,
        originalNode: null,
        displayName: dir.split('/').pop() || dir,
        isDirectory: true,
        type: 'directory'
      });
    });
    
    // Also synthesize the physical root directory if it's missing
    if (physicalNodes.length > 0 && !treeItems.find(t => t.normalizedPath === rootName)) {
      treeItems.push({
        normalizedPath: rootName,
        originalNode: null,
        displayName: rootName,
        isDirectory: true,
        type: 'directory'
      });
    }

    treeItems.sort((a, b) => a.normalizedPath.localeCompare(b.normalizedPath));
    return treeItems;
  }, [nodes]);

  const validDirPaths = new Set(items.filter(n => n.isDirectory).map(n => n.normalizedPath));

  const getAncestorPaths = (path: string) => {
    const ancestors = [];
    const parts = path.split('/');
    
    let current = parts[0];
    for (let i = 1; i < parts.length; i++) {
      if (validDirPaths.has(current)) {
        ancestors.push(current);
      }
      current += '/' + parts[i];
    }
    return ancestors;
  };

  const handleItemClick = (item: TreeItem) => {
    if (item.isDirectory) {
      setExpandedDirs(prev => ({
        ...prev,
        [item.normalizedPath]: !prev[item.normalizedPath]
      }));
    } else if (item.originalNode) {
      onNodeSelect(item.originalNode);
    }
  };

  return (
    <div className={`w-80 h-full flex flex-col ${panelGlassClass} pointer-events-auto overflow-hidden`}>
      <h2 className="text-xl font-bold mb-4 drop-shadow-md text-blue-300">File Explorer</h2>
      <div className="flex-1 overflow-y-auto custom-scrollbar pr-2 flex flex-col gap-1 text-sm">
        {items.length === 0 ? (
          <div className="text-gray-400 italic">No files or memories found in graph.</div>
        ) : (
          items.map(item => {
            const ancestors = getAncestorPaths(item.normalizedPath);
            const isVisible = ancestors.every(anc => expandedDirs[anc]);
            
            if (!isVisible) return null;

            const isSelected = item.originalNode && selectedNode?.id === item.originalNode.id;
            
            const parts = item.normalizedPath.split('/');
            const depth = parts.length - 1;
            const isExpanded = expandedDirs[item.normalizedPath];

            let icon = '📄';
            if (item.isDirectory) icon = isExpanded ? '📂' : '📁';
            else if (item.type === 'markdown') icon = '📝';
            else if (item.type === 'memory') icon = '🧠';

            return (
              <button
                key={item.normalizedPath}
                onClick={() => handleItemClick(item)}
                onContextMenu={(e) => {
                  if (item.type === 'memory' && item.originalNode && onContextMenu) {
                    onContextMenu(e, item.originalNode);
                  }
                }}
                style={{ paddingLeft: `${depth * 16 + 8}px` }}
                className={`text-left py-1.5 min-h-[32px] w-full rounded transition-all flex items-center ${isSelected ? 'bg-blue-500/30 text-blue-100 font-bold border-l-2 border-blue-500' : 'hover:bg-white/10 text-gray-300 border-l-2 border-transparent'}`}
                title={item.tooltip}
              >
                <span className="mr-2 opacity-70 flex-shrink-0 text-base leading-none w-4 text-center inline-block">
                  {icon}
                </span>
                <span className="truncate leading-tight mt-0.5">{item.displayName}</span>
              </button>
            );
          })
        )}
      </div>
    </div>
  );
};
