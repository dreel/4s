// A client that opens an undo group, edits while another user edits too,
// and disconnects without ending the group (scripts/e2e-cli.sh).
// Usage: node undo-group.mjs <4s binary>
import { readFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { join } from 'node:path';

const bin = process.argv[2];
const info = JSON.parse(readFileSync(join(process.env.FOURS_DATA_DIR, '4sd.json'), 'utf8'));
const ws = new WebSocket(info.url);
let id = 0;
const pending = new Map();
ws.onmessage = (m) => {
  const msg = JSON.parse(m.data);
  const p = pending.get(msg.id);
  if (!p) return;
  pending.delete(msg.id);
  msg.error ? p.reject(new Error(msg.error.message)) : p.resolve(msg.result);
};
const call = (method, params = {}) =>
  new Promise((resolve, reject) => {
    pending.set(++id, { resolve, reject });
    ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }));
  });
await new Promise((r) => (ws.onopen = r));
await call('session.hello', { client_name: 'assistant', protocol_version: info.protocol_version, user: process.env.FOURS_USER });
await call('history.group_begin', { label: 'assistant: punchier kick' });
await call('param.set', { path: 'drums.kick.decay', value: 0.3 });
// Another user edits meanwhile, on their own connection.
execFileSync(bin, ['--user', 'bob', 'set', 'mixer.1.pan', '0.25']);
await call('param.set', { path: 'drums.kick.tone', value: 0.7 });
await call('device.add', { type: 'compressor', channel: 1 });
try {
  await call('history.group_begin', { label: 'again' });
  console.log('nested begin accepted');
} catch (e) {
  console.log(`nested begin refused: ${e.message}`);
}
ws.close(); // no history.group_end: the daemon ends the group
await new Promise((r) => (ws.onclose = r));
