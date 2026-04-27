import { listen } from '@tauri-apps/api/event';
listen('load-project-path', (e) => console.log('load-project-path', e));
