import { useRef, useMemo } from 'react';
import ForceGraph3D from 'react-force-graph-3d';
import * as THREE from 'three';
import type { GraphData, MemoryNode, MemoryLink } from '../types';

interface Props {
  data: GraphData;
  onNodeClick: (node: MemoryNode) => void;
  onLinkClick: (link: MemoryLink) => void;
}

const colorMap: Record<string, string> = {
  rule: '#ff4b4b',
  preference: '#00ffcc',
  bootstrap: '#ffaa00',
  persona: '#cc00ff',
  context: '#4b9dff',
  directory: '#555555',
  markdown: '#ffffff',
  code_ast: '#ffcc00',
};

const getGlowTexture = () => {
  const canvas = document.createElement('canvas');
  canvas.width = 64;
  canvas.height = 64;
  const ctx = canvas.getContext('2d');
  if (ctx) {
    const gradient = ctx.createRadialGradient(32, 32, 0, 32, 32, 32);
    gradient.addColorStop(0, 'rgba(255, 255, 255, 1)');
    gradient.addColorStop(0.2, 'rgba(255, 255, 255, 0.8)');
    gradient.addColorStop(0.5, 'rgba(255, 255, 255, 0.2)');
    gradient.addColorStop(1, 'rgba(0, 0, 0, 0)');
    ctx.fillStyle = gradient;
    ctx.fillRect(0, 0, 64, 64);
  }
  return new THREE.CanvasTexture(canvas);
};

export const GalaxyGraph3D = ({ data, onNodeClick, onLinkClick }: Props) => {
  const fgRef = useRef<any>(null);
  const glowTexture = useMemo(() => getGlowTexture(), []);

  return (
    <div className="absolute inset-0 bg-black z-0">
      <ForceGraph3D
        ref={fgRef}
        graphData={data}
        backgroundColor="#000000"
        nodeThreeObject={(node: any) => {
          const mNode = node as MemoryNode;
          const size = Math.max(16, mNode.degree * 3);
          const color = colorMap[mNode.memory_type] || '#888888';
          
          const material = new THREE.SpriteMaterial({
            map: glowTexture,
            color: color,
            transparent: true,
            blending: THREE.AdditiveBlending,
            depthWrite: false
          });
          const sprite = new THREE.Sprite(material);
          sprite.scale.set(size, size, 1);
          return sprite;
        }}
        linkDirectionalParticles={3}
        linkDirectionalParticleSpeed={0.004}
        linkDirectionalParticleThreeObject={(link: any) => {
          let color = '#ffffff';
          if (link.type === 'contains') color = '#6496ff';
          else if (link.type === 'links_to') color = '#ff64ff';
          else if (link.type === 'related_to') color = '#64ffda';

          const material = new THREE.SpriteMaterial({
            map: glowTexture,
            color: color,
            transparent: true,
            opacity: 0.4, // Dropped opacity for the plasma feel
            blending: THREE.AdditiveBlending,
            depthWrite: false
          });
          const sprite = new THREE.Sprite(material);
          sprite.scale.set(10, 10, 1); // Large heavy blur/glow
          return sprite;
        }}
        linkColor={(link: any) => {
          // Physical lines between nodes
          if (link.type === 'contains') return 'rgba(100, 150, 255, 0.25)';
          if (link.type === 'links_to') return 'rgba(255, 100, 255, 0.5)';
          return 'rgba(255, 255, 255, 0.15)';
        }}
        linkWidth={(link: any) => link.type === 'links_to' ? 2 : 1}
        onNodeClick={(n) => onNodeClick(n as MemoryNode)}
        onLinkClick={(l) => onLinkClick(l as MemoryLink)}
      />
    </div>
  );
};
