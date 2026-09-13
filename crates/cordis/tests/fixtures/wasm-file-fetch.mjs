import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const fetchNetwork = globalThis.fetch;
globalThis.fetch = async (input, init) => {
  const url = input instanceof URL ? input : typeof input === 'string' ? new URL(input) : new URL(input.url);
  if (url.protocol !== 'file:') return fetchNetwork(input, init);
  return new Response(await readFile(fileURLToPath(url)), {
    headers: { 'content-type': url.pathname.endsWith('.wasm') ? 'application/wasm' : 'application/octet-stream' },
  });
};
