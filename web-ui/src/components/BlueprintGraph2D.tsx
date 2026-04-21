import ForceGraph2D from 'react-force-graph-2d';
import type { GraphData, MemoryNode, MemoryLink } from '../types';

interface Props {
  data: GraphData;
  onNodeClick: (node: MemoryNode) => void;
  onLinkClick: (link: MemoryLink) => void;
}

export const BlueprintGraph2D: React.FC<Props> = ({ data, onNodeClick, onLinkClick }) => {
  return (
    <div className="absolute inset-0 bg-[#0a192f] z-0" style={{ backgroundImage: 'linear-gradient(rgba(255,255,255,0.05) 1px, transparent 1px), linear-gradient(90deg, rgba(255,255,255,0.05) 1px, transparent 1px)', backgroundSize: '20px 20px' }}>
      <ForceGraph2D
        graphData={data}
        backgroundColor="transparent"
        nodeCanvasObject={(node, ctx, globalScale) => {
          const mNode = node as MemoryNode;
          const label = mNode.name;
          const fontSize = 12 / globalScale;
          const size = Math.max(8, mNode.degree * 2);
          
          ctx.beginPath();
          ctx.arc(mNode.x || 0, mNode.y || 0, size, 0, 2 * Math.PI, false);
          ctx.fillStyle = mNode.memory_type === 'markdown' ? '#ffffff' : mNode.memory_type === 'directory' ? '#555555' : mNode.namespace === 'global' ? '#64ffda' : '#e6f1ff';
          
          // Add soft glow to nodes in 2D
          ctx.shadowBlur = 10;
          ctx.shadowColor = ctx.fillStyle;
          ctx.fill();
          
          ctx.lineWidth = 1;
          ctx.strokeStyle = '#0a192f';
          ctx.stroke();
          
          // Reset shadow for text
          ctx.shadowBlur = 0;
          ctx.font = `${fontSize}px Sans-Serif`;
          ctx.textAlign = 'center';
          ctx.textBaseline = 'middle';
          ctx.fillStyle = '#a8b2d1';
          ctx.fillText(label, mNode.x || 0, (mNode.y || 0) + size + fontSize);
        }}
        linkDirectionalParticles={2}
        linkDirectionalParticleWidth={4} // Larger for blurred plasma look
        linkDirectionalParticleColor={(link: any) => {
          if (link.type === 'contains') return 'rgba(100, 150, 255, 0.4)';
          if (link.type === 'links_to') return 'rgba(255, 100, 255, 0.4)';
          return 'rgba(100, 255, 218, 0.4)';
        }}
        linkDirectionalParticleSpeed={0.005}
        linkColor={(link: any) => {
          // Physical lines between nodes
          if (link.type === 'contains') return 'rgba(100, 150, 255, 0.2)';
          if (link.type === 'links_to') return 'rgba(255, 100, 255, 0.6)';
          return 'rgba(100, 255, 218, 0.3)';
        }}
        linkWidth={(link: any) => link.type === 'links_to' ? 2 : 1}
        onNodeClick={(n) => onNodeClick(n as MemoryNode)}
        onLinkClick={(l) => onLinkClick(l as MemoryLink)}
      />
    </div>
  );
};
