import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const [source, input, output] = process.argv.slice(2);
const { Context } = await import(pathToFileURL(join(source, 'vendor/cordis/lib/index.js')));
const { default: SessionStore, SessionId } = await import(pathToFileURL(join(source, 'packages/core/session/src/index.ts')));
const observations = [];
for (const scenario of JSON.parse(await readFile(input, 'utf8'))) {
  const context = new Context();
  const fiber = await context.plugin(SessionStore);
  context.logger.warn = () => {};
  const session = context.sessions.prepare(SessionId('publication'), { meta: { createdAt: 99 } });
  const detach = context.sessions.enter(session);
  context.sessions.announce(session);
  const heard = [];
  const errors = [];
  const order = [];
  const state = () => context.sessions.get(session.id) === session ? 'live' : 'detached';
  context.on('internal/dispatch', (_mode, name) => {
    if (name !== 'session/event') return;
    order.push(`resolve:${state()}`);
    if (scenario.detach === 'dispatch') detach();
  });
  context.on('session/event', (observed, event) => {
    if (event.type !== 'turn/start') return;
    order.push(`attempt:${state()}`);
    try {
      observed.append(scenario.nestedType, scenario.nestedData);
    } catch (error) {
      errors.push(error.message);
      throw error;
    }
  });
  context.on('session/event', () => {
    if (scenario.detach === 'observer') {
      order.push(`request-detach:${state()}`);
      detach();
    }
  });
  context.on('session/event', (_observed, event) => {
    heard.push(event.type);
    order.push(`observe:${state()}`);
  });
  context.on('session/disposed', (observed) => {
    order.push(`dispose:${state()}`);
    observed.append('todo/write', { todos: [] });
  });
  const originalNow = Date.now;
  let clock = 100;
  Date.now = () => clock++;
  try {
    session.append('turn/start', { turn: 1 });
    if (scenario.detach === 'none') session.append('todo/write', { todos: [] });
    observations.push({
      events: [...session.events], heard: [...heard], errors: [...errors], order: [...order],
      clock, attached: state() === 'live',
    });
  } finally {
    Date.now = originalNow;
    detach();
    await fiber.dispose();
  }
}
await writeFile(output, JSON.stringify(observations));
