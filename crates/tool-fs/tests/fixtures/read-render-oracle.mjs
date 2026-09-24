import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const [source, input, output] = process.argv.slice(2);
const moduleUrl = (name) => pathToFileURL(join(source, 'packages/fs/tool-fs/src', name));
const { buildWindow, formatReadOutput, langFromPath, readMetaFromMeta } = await import(moduleUrl('read-render.ts'));
const { applyReadTool } = await import(moduleUrl('read.ts'));
let definition;
applyReadTool({
  systemPrompt: { section() {} },
  tools: { register(value) { definition = value; } },
}, { limit: 2000, maxLineLength: 2000, maxBytes: 51200, streamMinSize: 10485760 });

const requests = JSON.parse(await readFile(input, 'utf8'));
const windows = [];
for (const { chunks, request, path } of requests.windows) {
  try {
    const value = await buildWindow(chunks, request, path);
    windows.push({ value, render: formatReadOutput(path, { offset: request.offset, ...value }) });
  } catch (error) {
    windows.push({ error: { message: error.message, code: error.code } });
  }
}
await writeFile(output, JSON.stringify({
  windows,
  metadata: requests.metadata.map(value => readMetaFromMeta(value) ?? null),
  languages: requests.languages.map(path => langFromPath(path) ?? null),
  presentations: requests.presentations.map(result => definition.presentResult({ file_path: 'file.rs' }, result) ?? null),
}));
