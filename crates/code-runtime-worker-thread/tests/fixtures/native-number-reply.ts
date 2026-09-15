const { parentPort } = await import('node:worker_threads');
let received;
parentPort.on('message', message => {
  if (message.type === 'reply') received = {
    ok: message.ok,
    hasValue: Object.hasOwn(message, 'value'),
    message: message.message ?? null,
  };
});
let rejection;
try { await host.value(null); } catch (error) { rejection = error.message; }
await new Promise(resolve => setImmediate(resolve));
return { received, rejection };
