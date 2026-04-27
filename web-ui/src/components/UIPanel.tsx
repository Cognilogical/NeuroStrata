import React, { useState } from 'react';
import { open } from '@tauri-apps/plugin-shell';
import type { MemoryNode, MemoryLink } from '../types';

interface Props {
  viewMode: '2d' | '3d';
  setViewMode: (v: '2d' | '3d') => void;
  selectedNode: MemoryNode | null;
  selectedLink: MemoryLink | null;
  typeFilters: Record<string, boolean>;
  setTypeFilters: (f: Record<string, boolean>) => void;
  namespaceFilters: Record<string, boolean>;
  setNamespaceFilters: (f: Record<string, boolean>) => void;
}

export const UIPanel: React.FC<Props> = ({
  viewMode,
  setViewMode,
  selectedNode,
  selectedLink,
  typeFilters,
  setTypeFilters,
  namespaceFilters,
  setNamespaceFilters,
}) => {
  const [editor, setEditor] = useState<'vscode' | 'cursor' | 'obsidian'>(() => {
    const saved = localStorage.getItem('neurostrata_editor');
    if (saved === 'vscode' || saved === 'cursor' || saved === 'obsidian') return saved;
    return 'vscode';
  });

  const launchUrl = async (url: string) => {
    try {
      if ('__TAURI_INTERNALS__' in window) {
        await open(url);
      } else {
        window.open(url, '_self');
      }
    } catch (e) {
      console.error("Failed to open URL:", e);
    }
  };

  const handleOpenInEditor = () => {
    if (!selectedNode || !selectedNode.absolute_path) return;
    const path = selectedNode.absolute_path;
    
    // Attempt to derive project root from absolute path and relative location
    let rootPath = '';
    if (selectedNode.location) {
      // Remove leading ./ or / from location
      const loc = selectedNode.location.replace(/^(\.\/|\/)/, '');
      if (path.endsWith(loc)) {
         rootPath = path.slice(0, -loc.length);
         // remove trailing slash
         if (rootPath.endsWith('/')) rootPath = rootPath.slice(0, -1);
      }
    }

    if (editor === 'vscode' || editor === 'cursor') {
      const scheme = editor === 'vscode' ? 'vscode://file' : 'cursor://file';
      if (rootPath) {
        // Open the workspace folder first
        launchUrl(`${scheme}${encodeURI(rootPath)}`);
        // Give the editor a moment to focus the workspace, then open the specific file
        setTimeout(() => {
          launchUrl(`${scheme}${encodeURI(path)}`);
        }, 1000);
      } else {
        launchUrl(`${scheme}${encodeURI(path)}`);
      }
    } else {
      const url = `obsidian://open?path=${encodeURIComponent(path)}`;
      launchUrl(url);
    }
  };

  // Candy glassmorphism bevel effect class
  const panelGlassClass = "p-5 rounded-2xl bg-black/60 backdrop-blur-xl border border-white/20 shadow-[0_8px_32px_rgba(0,0,0,0.5),inset_0_1px_1px_rgba(255,255,255,0.4),inset_0_-1px_1px_rgba(255,255,255,0.1)] text-white";

  return (
    <div className="absolute inset-0 pointer-events-none flex flex-row-reverse p-6 z-10 text-white font-sans overflow-hidden">
      
      {/* Right Column Stack */}
      <div className="w-96 flex flex-col gap-4 pointer-events-none max-h-full">
        
        {/* Filters Box */}
        <div className={`pointer-events-auto flex flex-col max-h-[50vh] ${panelGlassClass}`}>
          <h3 className="font-semibold mb-2 text-blue-300 drop-shadow-md text-lg">Namespaces</h3>
          <div className="flex flex-col gap-2 mb-6 overflow-y-auto pr-2 custom-scrollbar">
            {Object.entries(namespaceFilters).map(([ns, checked]) => (
              <label key={ns} className="flex items-center gap-3 cursor-pointer text-sm hover:text-blue-100 transition-colors">
                <input 
                  type="checkbox" 
                  checked={checked} 
                  onChange={(e) => setNamespaceFilters({ ...namespaceFilters, [ns]: e.target.checked })} 
                  className="accent-blue-500 w-4 h-4 rounded-sm border-white/20 bg-white/10 flex-shrink-0" 
                />
                <span className="break-all drop-shadow-sm font-medium leading-tight" title={ns}>{ns}</span>
              </label>
            ))}
          </div>

          <h3 className="font-semibold mb-2 text-purple-300 drop-shadow-md text-lg">Memory Types</h3>
          <div className="flex flex-col gap-2 overflow-y-auto pr-2 custom-scrollbar">
            {Object.entries(typeFilters).map(([type, checked]) => (
              <label key={type} className="flex items-center gap-3 cursor-pointer text-sm hover:text-purple-100 transition-colors">
                <input 
                  type="checkbox" 
                  checked={checked} 
                  onChange={(e) => setTypeFilters({ ...typeFilters, [type]: e.target.checked })} 
                  className="accent-purple-500 w-4 h-4 rounded-sm border-white/20 bg-white/10" 
                />
                <span className="drop-shadow-sm font-medium capitalize">{type}</span>
              </label>
            ))}
          </div>
        </div>

        {/* Details / Viewer Box */}
        {(selectedNode || selectedLink) && (
          <div className={`pointer-events-auto flex flex-col flex-1 max-h-[50vh] ${panelGlassClass}`}>
            <h2 className="text-xl font-bold mb-4 drop-shadow-md">Details & Viewer</h2>
            <div className="overflow-y-auto pr-2 custom-scrollbar">
              {selectedNode && (
                <div className="flex flex-col gap-2">
                  <div className="text-sm text-gray-300">Type: <span className="font-mono text-purple-300 drop-shadow-sm">{selectedNode.memory_type}</span></div>
                  {selectedNode.namespace && <div className="text-sm text-gray-300">Namespace: <span className="font-mono text-blue-300 drop-shadow-sm">{selectedNode.namespace}</span></div>}
                  
                  {selectedNode.absolute_path && (
                    <div className="mt-2 flex flex-col gap-2 p-3 bg-white/5 rounded-lg border border-white/10">
                      <div className="text-xs text-gray-400 mb-1">Location: <span className="break-all inline-block mt-1 font-mono text-gray-300">{selectedNode.location}</span></div>
                      <div className="flex flex-col gap-2 mt-2">
                        <div className="flex bg-black/40 p-1 rounded-md border border-white/10 w-full">
                          {(['vscode', 'cursor', 'obsidian'] as const).map(ed => (
                            <button
                              key={ed}
                              onClick={() => {
                                setEditor(ed);
                                localStorage.setItem('neurostrata_editor', ed);
                              }}
                              className={`flex-1 text-xs font-bold py-1.5 px-2 rounded-sm transition-all ${editor === ed ? 'bg-blue-600 text-white shadow-sm' : 'text-gray-400 hover:text-gray-200 hover:bg-white/5'}`}
                            >
                              {ed === 'vscode' ? 'VS Code' : ed === 'cursor' ? 'Cursor' : 'Obsidian'}
                            </button>
                          ))}
                        </div>
                        <button 
                          onClick={handleOpenInEditor}
                          className="w-full bg-blue-600/20 hover:bg-blue-600/40 text-blue-200 border border-blue-500/30 text-xs font-bold py-2 px-2 rounded transition-colors shadow-sm mt-1 flex items-center justify-center gap-2"
                        >
                          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"></path><polyline points="15 3 21 3 21 9"></polyline><line x1="10" y1="14" x2="21" y2="3"></line></svg>
                          Open in {editor === 'vscode' ? 'VS Code' : editor === 'cursor' ? 'Cursor' : 'Obsidian'}
                        </button>
                      </div>
                    </div>
                  )}

                  {/* File / Memory Viewer Content */}
                  <div className="mt-2 text-sm leading-relaxed whitespace-pre-wrap bg-black/30 p-4 rounded-lg shadow-[inset_0_2px_4px_rgba(0,0,0,0.6)] border border-white/5 break-words">
                    {selectedNode.name}
                    {selectedNode.content && (
                      <div className="mt-4 pt-4 border-t border-white/10">
                        <strong className="text-blue-300 block mb-2">Content / Extract:</strong>
                        {selectedNode.content}
                      </div>
                    )}
                  </div>
                </div>
              )}
              {selectedLink && (
                <div className="flex flex-col gap-2">
                  <div className="text-sm text-gray-300">Type: <span className="font-mono text-white drop-shadow-sm">{selectedLink.type || 'Connection'}</span></div>
                  <div className="text-sm text-gray-300">From: <span className="font-mono text-white drop-shadow-sm">{selectedLink.source}</span></div>
                  <div className="text-sm text-gray-300">To: <span className="font-mono text-white drop-shadow-sm">{selectedLink.target}</span></div>
                </div>
              )}
            </div>
          </div>
        )}
      </div>

      <div className="absolute bottom-6 right-6 pointer-events-auto">
        <label className={`${panelGlassClass} !p-3 !px-5 flex items-center gap-3 cursor-pointer hover:bg-white/20 transition-all active:scale-95 group shadow-lg`}>
          <input 
            type="checkbox" 
            checked={viewMode === '3d'} 
            onChange={(e) => setViewMode(e.target.checked ? '3d' : '2d')} 
            className="appearance-none w-5 h-5 border-2 border-white rounded-sm bg-transparent checked:bg-white checked:border-white relative flex items-center justify-center cursor-pointer transition-colors after:content-[''] checked:after:content-['✔'] checked:after:text-black checked:after:text-sm checked:after:font-black checked:after:absolute" 
          />
          <span className="font-bold text-lg tracking-wider drop-shadow-md group-hover:text-white text-gray-200">3D</span>
        </label>
      </div>
    </div>
  );
};

