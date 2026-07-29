import fs from 'node:fs';
import path from 'node:path';

export function prepareVisualCaptures({
  current = path.join(process.cwd(), 'output', 'playwright', 'current'),
} = {}) {
  fs.rmSync(path.resolve(current), { recursive: true, force: true });
}

export default function globalSetup() {
  prepareVisualCaptures();
}
