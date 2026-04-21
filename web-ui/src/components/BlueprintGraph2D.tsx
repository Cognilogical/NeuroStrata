import ForceGraph2D from 'react-force-graph-2d';
import type { GraphData, MemoryNode, MemoryLink } from '../types';

interface Props {
  data: GraphData;
  onNodeClick: (node: MemoryNode) => void;
  onLinkClick: (link: MemoryLink) => void;
}

// Minimalistic blueprint/architectural SVG paths (24x24 viewBox equivalent)
const ICON_PATHS: Record<string, string> = {
  directory: 'M10 4H4c-1.1 0-1.99.9-1.99 2L2 18c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V8c0-1.1-.9-2-2-2h-8l-2-2z', // Folder
  markdown: 'M14 2H6c-1.1 0-1.99.9-1.99 2L4 20c0 1.1.89 2 1.99 2H18c1.1 0 2-.9 2-2V8l-6-6zm2 16H8v-2h8v2zm0-4H8v-2h8v2zm-3-5V3.5L18.5 9H13z', // File text
  code_ast: 'M9.4 16.6L4.8 12l4.6-4.6L8 6l-6 6 6 6 1.4-1.4zm5.2 0l4.6-4.6-4.6-4.6L16 6l6 6-6 6-1.4-1.4z', // Code brackets
  rule: 'M12 1L3 5v6c0 5.55 3.84 10.74 9 12 5.16-1.26 9-6.45 9-12V5l-9-4zm0 10.99h7c-.53 4.12-3.28 7.79-7 8.94V12H5V6.3l7-3.11v8.8z', // Shield
  persona: 'M12 12c2.21 0 4-1.79 4-4s-1.79-4-4-4-4 1.79-4 4 1.79 4 4 4zm0 2c-2.67 0-8 1.34-8 4v2h16v-2c0-2.66-5.33-4-8-4z', // User
  bootstrap: 'M7 2v11h3v9l7-12h-4l4-8z', // Flash/Lightning
  preference: 'M12 17.27L18.18 21l-1.64-7.03L22 9.24l-7.19-.61L12 2 9.19 8.63 2 9.24l5.46 4.73L5.82 21z', // Star
  context: 'M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-6h2v6zm0-8h-2V7h2v2z' // Info circle
};

// Cache the Path2D objects so they aren't parsed every frame
const cachedPaths: Record<string, Path2D> = {};
if (typeof Path2D !== 'undefined') {
  Object.keys(ICON_PATHS).forEach(k => {
    cachedPaths[k] = new Path2D(ICON_PATHS[k]);
  });
}

export const BlueprintGraph2D: React.FC<Props> = ({ data, onNodeClick, onLinkClick }) => {
  return (
    <div className="absolute inset-0 bg-[#0a192f] z-0" style={{ backgroundImage: 'linear-gradient(rgba(255,255,255,0.05) 1px, transparent 1px), linear-gradient(90deg, rgba(255,255,255,0.05) 1px, transparent 1px)', backgroundSize: '20px 20px' }}>
      <ForceGraph2D
        graphData={data}
        backgroundColor="transparent"
        nodeCanvasObject={(node, ctx, globalScale) => {
          const mNode = node as MemoryNode;
          if (mNode.x === undefined || mNode.y === undefined) return;
          
          const label = mNode.name;
          const fontSize = 12 / globalScale;
          const baseSize = Math.max(12, (mNode.degree || 1) * 2); 
          const color = mNode.memory_type === 'markdown' ? '#ffffff' : mNode.memory_type === 'directory' ? '#888888' : mNode.namespace === 'global' ? '#64ffda' : '#e6f1ff';
          
          ctx.save();
          ctx.translate(mNode.x, mNode.y);

          // Neon/Blueprint Glow effect
          ctx.shadowBlur = 12;
          ctx.shadowColor = color;
          ctx.fillStyle = color;

          // Scale icon to fit node size (SVG paths are 24x24)
          const scale = baseSize / 12;
          ctx.scale(scale, scale);
          ctx.translate(-12, -12); // Center the 24x24 path

          const path = cachedPaths[mNode.memory_type] || cachedPaths.context;
          if (path) ctx.fill(path);

          ctx.restore();
          
          // Draw Text Label
          ctx.shadowBlur = 0; // Reset shadow for clean text
          ctx.font = `${fontSize}px Sans-Serif`;
          ctx.textAlign = 'center';
          ctx.textBaseline = 'top';
          ctx.fillStyle = '#a8b2d1';
          ctx.fillText(label, mNode.x, mNode.y + baseSize + 4);
        }}
        linkDirectionalParticles={2}
        linkDirectionalParticleWidth={2} // Extremely faint, small blur on 2D
        linkDirectionalParticleColor={(link: any) => {
          if (link.type === 'contains') return 'rgba(100, 150, 255, 0.15)';
          if (link.type === 'links_to') return 'rgba(255, 100, 255, 0.25)';
          return 'rgba(100, 255, 218, 0.15)';
        }}
        linkDirectionalParticleSpeed={0.003}
        linkColor={(link: any) => {
          // Physical lines between nodes
          if (link.type === 'contains') return 'rgba(100, 150, 255, 0.1)';
          if (link.type === 'links_to') return 'rgba(255, 100, 255, 0.2)';
          return 'rgba(100, 255, 218, 0.1)';
        }}
        linkWidth={(link: any) => link.type === 'links_to' ? 3 : 1.5} // Fatter physical lines
        onNodeClick={(n) => onNodeClick(n as MemoryNode)}
        onLinkClick={(l) => onLinkClick(l as MemoryLink)}
      />
    </div>
  );
};
