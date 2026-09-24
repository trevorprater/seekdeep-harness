import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const [source, input, output] = process.argv.slice(2);
const { Session, SessionId } = await import(pathToFileURL(join(source, 'packages/core/session/src/index.ts')));
const observations = [];
for (const scenario of JSON.parse(await readFile(input, 'utf8'))) {
  const originalNow = Date.now;
  let clock = 100;
  Date.now = () => clock++;
  try {
    let session;
    let error = null;
    try {
      session = Session.create(SessionId('admission'), scenario.mode === 'seed' ? [scenario.event] : undefined, scenario.header ?? undefined);
      if (scenario.mode === 'append') session.append(scenario.event.type, scenario.event.data, scenario.event);
    } catch (caught) {
      error = caught.message;
    }
    observations.push({ error, events: session ? [...session.events] : null, nodes: session ? [...session.surface.nodes] : null, clock });
  } finally {
    Date.now = originalNow;
  }
}
await writeFile(output, JSON.stringify(observations));
