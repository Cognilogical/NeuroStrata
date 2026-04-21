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
    // Extra diffuse, softer glow profile
    gradient.addColorStop(0, 'rgba(255, 255, 255, 1)');
    gradient.addColorStop(0.1, 'rgba(255, 255, 255, 0.8)');
    gradient.addColorStop(0.4, 'rgba(255, 255, 255, 0.2)');
    gradient.addColorStop(1, 'rgba(0, 0, 0, 0)');
    ctx.fillStyle = gradient;
    ctx.fillRect(0, 0, 64, 64);
  }
  return new THREE.CanvasTexture(canvas);
};

export const GalaxyGraph3D = ({ data, onNodeClick, onLinkClick }: Props) => {
  const fgRef = useRef<any>(null);
  
  const { nodeMaterials, defaultNodeMaterial, plasmaMaterials } = useMemo(() => {
    const tex = getGlowTexture();
    
    const nMats: Record<string, THREE.SpriteMaterial> = {};
    for (const [key, color] of Object.entries(colorMap)) {
      nMats[key] = new THREE.SpriteMaterial({
        map: tex,
        color: color,
        transparent: true,
        blending: THREE.AdditiveBlending,
        depthWrite: false
      });
    }
    
    const defNodeMat = new THREE.SpriteMaterial({
      map: tex,
      color: '#888888',
      transparent: true,
      blending: THREE.AdditiveBlending,
      depthWrite: false
    });

    // Plasma materials pre-cached to prevent WebGL Context Loss
    const pMats: Record<string, THREE.SpriteMaterial> = {
      contains: new THREE.SpriteMaterial({
        map: tex, color: '#6496ff', transparent: true, opacity: 0.15, blending: THREE.AdditiveBlending, depthWrite: false
      }),
      links_to: new THREE.SpriteMaterial({
        map: tex, color: '#ff64ff', transparent: true, opacity: 0.25, blending: THREE.AdditiveBlending, depthWrite: false
      }),
      default: new THREE.SpriteMaterial({
        map: tex, color: '#64ffda', transparent: true, opacity: 0.15, blending: THREE.AdditiveBlending, depthWrite: false
      })
    };

    return { nodeMaterials: nMats, defaultNodeMaterial: defNodeMat, plasmaMaterials: pMats };
  }, []);

  return (
    <div className="absolute inset-0 bg-black z-0">
      <ForceGraph3D
        ref={fgRef}
        graphData={data}
        backgroundColor="#000000"
        nodeThreeObject={(node: any) => {
          const mNode = node as MemoryNode;
          const size = Math.max(16, mNode.degree * 3);
          const material = nodeMaterials[mNode.memory_type] || defaultNodeMaterial;
          const sprite = new THREE.Sprite(material);
          sprite.scale.set(size, size, 1);
          return sprite;
        }}
        linkDirectionalParticles={3}
        linkDirectionalParticleSpeed={0.003}
        linkDirectionalParticleThreeObject={(link: any) => {
          // Re-use pre-cached materials to avoid memory leak
          const material = plasmaMaterials[link.type] || plasmaMaterials.default;
          const sprite = new THREE.Sprite(material);
          // Scale down to make them smaller, but diffuse
          sprite.scale.set(6, 6, 1); 
          return sprite;
        }}
        linkColor={(link: any) => {
          // Faint lines, but fatter physical presence
          if (link.type === 'contains') return 'rgba(100, 150, 255, 0.1)';
          if (link.type === 'links_to') return 'rgba(255, 100, 255, 0.2)';
          return 'rgba(255, 255, 255, 0.08)';
        }}
        linkWidth={(link: any) => link.type === 'links_to' ? 3 : 1.5}
        onNodeClick={(n) => onNodeClick(n as MemoryNode)}
        onLinkClick={(l) => onLinkClick(l as MemoryLink)}
      />
    </div>
  );
};
