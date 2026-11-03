#!/usr/bin/env node
const fs = require('fs');
const path = require('path');

const PKG_DIR = '/home/kenton/Documents/NeuroStrata';
const directories = ['openclaw', 'claude', 'hermes', 'codex', 'github-cli'];

for (const dir of directories) {
  const pkgPath = path.join(PKG_DIR, 'plugins', dir, 'package.json');
  const pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
  
  // Preserve original name and description
  if (!pkg.name) {
    pkg.name = `@neurostrata/${dir}-plugin`;
  }
  if (pkg.description) pkg.description = pkg.description;
  
  // Handle special case for openclaw - remove peerDependency on @openclaw/plugin
  if (dir === 'openclaw' && pkg.peerDependencies && pkg.peerDependencies['@openclaw/plugin']) {
    delete pkg.peerDependencies['@openclaw/plugin'];
  }
  
  // Apply template
  pkg.name = pkg.name || `@neurostrata/${dir}-plugin`;
  pkg.version = "1.14.3";
  pkg.type = "module";
  pkg.main = "./dist/index.js";
  pkg.types = "./dist/index.d.ts";
  
  pkg.scripts = {
    "build": "tsc",
    "typecheck": "tsc --noEmit",
    "test": "node --test test.mjs"
  };
  
  pkg.devDependencies = {
    "@types/node": "^22.0.0",
    "typescript": "^5.6.0"
  };
  
  pkg.exports = {
    ".": {
      "types": "./dist/index.d.ts",
      "default": "./dist/index.js"
    }
  };
  
  fs.writeFileSync(pkgPath, JSON.stringify(pkg, null, 2));
  console.log(`✓ Updated ${dir}`);
}