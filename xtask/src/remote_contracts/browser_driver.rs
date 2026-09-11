//! Real-browser protocol verification; no registry, gateway, or transport substitutes.

pub(super) const DRIVER: &str = r#"import { createRequire } from 'node:module';
import { readFile, writeFile, mkdtemp, rm } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
const [source, host, output, typedConsumer, loaderMode] = process.argv.slice(2);
const require = createRequire(join(source, 'apps/web/package.json'));
const { chromium } = require('playwright');
const root = process.cwd();
const home = await mkdtemp(join(tmpdir(), 'seekdeep-remote-browser-'));
let server, browser;
try {
  if (loaderMode) await writeFile(join(home, 'cordis.patch.yml'), '- id: directory-picker\n  disabled: true\n- insert:\n    - id: picker-browse\n      name: "@seekdeep-ai/seekdeep-host-directory-picker-browse"\n');
  server = spawn(host, ['web', '--host', '127.0.0.1', '--port', '0'], {
    cwd: root, env: { ...process.env, SEEKDEEP_HOME: home, SEEKDEEP_TELEMETRY_DISABLED: '1' }, stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stderr = '';
  server.stderr.on('data', data => { stderr += data; });
  const origin = await new Promise((resolve, reject) => {
    let stdout = '';
    const deadline = setTimeout(() => reject(new Error('Rust Host readiness timed out: ' + stderr)), 30000);
    server.stdout.on('data', data => { stdout += data; const match = /seekdeep web: (http:\/\/\S+)/.exec(stdout); if (match) { clearTimeout(deadline); resolve(match[1]); } });
    server.once('exit', code => { clearTimeout(deadline); reject(new Error(`Host exited ${code}: ${stderr}`)); });
  });
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  page.setDefaultTimeout(30000);
  const evaluate = async (callback, args) => {
    let deadline;
    try { return await Promise.race([page.evaluate(callback, args), new Promise((resolve, reject) => { deadline = setTimeout(() => reject(new Error('browser Remote scenario timed out')), 30000); })]); }
    finally { clearTimeout(deadline); }
  };
  page.on('pageerror', error => console.error('browser error:', error.message));
  const requests = [];
  const commandIdentities = [];
  const responseReads = [];
  const hostErrors = [];
  page.on('request', request => { if (request.method() === 'POST' && request.url().startsWith(origin + '/api/')) { requests.push(request.url().slice(origin.length)); if (request.url().startsWith(origin + '/api/commands/list')) commandIdentities.push(request.postDataJSON().payload.args.agentId); } });
  page.on('response', response => {
    if (response.url().startsWith(origin + '/api/')) responseReads.push(response.json().then(body => { if (body.result && !body.result.ok) hostErrors.push(body.result.error); }).catch(() => {}));
  });
  await page.goto(origin + '/api/__remote_path_probe__');
  await page.setContent('<!doctype html><html><head></head><body></body></html>');
  const assets = {};
  for (const path of ['vendor/cordis/lib/client.js', 'vendor/cordis/lib/index.js', 'packages/typert/registry/lib/client.js', 'packages/client/connection/lib/client.js', 'packages/api/gateway/lib/client.js', 'packages/api/remotes/lib/client.js']) assets[path] = await readFile(join(root, path), 'utf8');
  const bytes = (await readFile(join(root, 'vendor/cordis/lib/client_bg.wasm'))).toString('base64');
  let loaderAssets;
  if (loaderMode) {
    const html = await (await fetch(origin)).text();
    const match = /window\.__SEEKDEEP_BOOT__ = ([\s\S]*?)<\/script>/.exec(html);
    if (!match) throw new Error('Rust Host did not publish its boot manifest');
    const readRuntime = async (directory, stem, entry = 'index.js') => ({
      wrapper: await readFile(join(root, directory, entry), 'utf8'),
      bindings: await readFile(join(root, directory, stem + '.js'), 'utf8'),
      bytes: (await readFile(join(root, directory, stem + '_bg.wasm'))).toString('base64'), stem,
    });
    loaderAssets = { boot: JSON.parse(match[1]), loader: await readRuntime('vendor/loader/lib', 'client'), modules: await readRuntime('packages/client/modules/lib', 'wasm', 'client.js'), immer: await readFile(join(root, 'support/browser-dependencies/node_modules/immer/dist/immer.production.mjs'), 'utf8') };
    for (const id of ['@seekdeep-ai/seekdeep-api-remotes', '@seekdeep-ai/seekdeep-api-gateway', '@seekdeep-ai/seekdeep-typert-registry', '@seekdeep-ai/seekdeep-client-connection']) {
      if (!loaderAssets.boot.entries.some(entry => entry.id === id)) throw new Error('initial Host boot graph omitted ' + id);
    }
  }
  const publicContracts = [];
  let typedSource;
  let zodSource;
  if (typedConsumer) {
    typedSource = await readFile(typedConsumer, 'utf8');
    zodSource = await readFile(join(output, 'zod.mjs'), 'utf8');
    const model = JSON.parse(await readFile(join(root, 'crates/api-remotes-client/contracts/host-model.json'), 'utf8'));
    for (const pkg of model.face.packages) publicContracts.push({ name: pkg.name.replace('@deepseek-ai/dsh-', '@seekdeep-ai/seekdeep-'), code: await readFile(join(root, pkg.root, 'lib/typert.remote-client.js'), 'utf8') });
  }
  await evaluate(async ({ assets, bytes, publicContracts, typedSource, zodSource, loaderAssets }) => {
    const blob = text => URL.createObjectURL(new Blob([text], { type: 'text/javascript' }));
    const binding = blob(assets['vendor/cordis/lib/client.js']);
    const module = blob(assets['vendor/cordis/lib/index.js'].replace("'./client.js'", JSON.stringify(binding)).replace("new URL('./client_bg.wasm', import.meta.url)", `Uint8Array.from(atob(${JSON.stringify(bytes)}), c => c.charCodeAt(0))`));
    const cordis = await import(module);
    const handoffs = new Map();
    class BrowserContext extends cordis.Context {}
    const client = new BrowserContext();
    const rootFields = Object.keys(client);
    if (!(client instanceof BrowserContext) || rootFields.join(',') !== 'root,baseUrl,fiber,reflect,registry,events,logger') throw new Error('Context subclass or own fields changed');
    const structurePeer = new cordis.Context();
    if (client.reflect.constructor.name !== 'ReflectService' || Object.getPrototypeOf(client.reflect[cordis.symbols.original]) !== Object.getPrototypeOf(structurePeer.reflect[cordis.symbols.original])) throw new Error('reflection roots do not share their public prototype');
    await structurePeer.fiber.dispose();
    const shadowOwner = client.extend({ tag: 'caller' });
    const shadowChild = client.extend({ [cordis.symbols.shadow]: shadowOwner }).extend({ tag: 'child' });
    if (!Object.hasOwn(shadowChild, cordis.symbols.shadow) || Object.keys(shadowChild).length || cordis.getTraceable(shadowChild, shadowChild) !== Object.getPrototypeOf(shadowChild)) throw new Error('Context extension lost its source shadow layers');
    window.remotePathContextResults = { rootFields, subclass: true, reflectionPrototype: true, shadow: true };
    const extensionOwner = client.extend(), originalExtend = extensionOwner.extend;
    let extensionCalls = 0, lazyConfigReads = 0;
    Object.defineProperty(extensionOwner, 'extend', { value(metadata) { extensionCalls++; return originalExtend.call(this, { ...metadata, extensionMarker: true }); } });
    const extendedScope = extensionOwner.isolate('browser-extended-service'), extendedValue = {};
    const removeExtended = extendedScope.provide('browser-extended-service', extendedValue);
    if (extensionCalls !== 1 || !extendedScope.extensionMarker || extendedScope.get('browser-extended-service') !== extendedValue || client.get('browser-extended-service') !== undefined) throw new Error('Context isolation bypassed its extension method or native scope');
    await removeExtended();
    const lazyLogger = extensionOwner.intercept('logger', { get name() { lazyConfigReads++; return 'lazy-browser'; } });
    if (lazyConfigReads !== 0 || lazyLogger.logger().name !== 'lazy-browser' || lazyConfigReads !== 1 || extensionCalls !== 2) throw new Error('Context intercept config was evaluated before use');
    window.remotePathContextResults.extensionOverride = true;
    window.remotePathContextResults.lazyConfig = true;
    const independentReflection = new client.reflect.constructor(client);
    const removeIndependent = independentReflection.provide('browser-independent-record', 17);
    if (independentReflection.get('browser-independent-record') !== 17 || client.get('browser-independent-record') !== undefined) throw new Error('independent reflection published into the root store');
    await removeIndependent();
    const removeRootRecord = client.provide('browser-independent-record', 18);
    if (client.get('browser-independent-record') !== 18) throw new Error('independent reflection lost the shared scope label');
    await removeRootRecord();
    window.remotePathContextResults.independentReflection = true;
    const fiberFields = ['parent', 'inject', 'runtime', 'uid', 'ctx', 'config', '_config', 'state', 'dispose', 'store', 'inertia', '_hooks', '_disposables', 'context', '_error', '_runner', '_store'];
    const runnerMount = client.plugin(() => {}), runnerFiber = await runnerMount, runner = runnerFiber._runner, lifecycleTrace = [];
    if (Object.keys(client.fiber).join(',') !== fiberFields.join(',') || Object.keys(runnerFiber).join(',') !== fiberFields.join(',')) throw new Error('Fiber facade exposes the wrong fields');
    for (const name of ['_setEpoch', '_refresh', '_updateState', '_getState', '_resolveConfig', '_reload', '_unload', '_execute']) {
      const original = runnerFiber[name];
      runnerFiber[name] = function (...args) { lifecycleTrace.push(name); return original.apply(this, args); };
    }
    await runnerMount.update({ next: true });
    const expectedLifecycle = ['_resolveConfig', '_setEpoch', '_updateState', '_unload', '_refresh', '_setEpoch', '_updateState', '_reload', '_resolveConfig', '_execute', '_updateState', '_getState'];
    if (lifecycleTrace.join(',') !== expectedLifecycle.join(',')) throw new Error('Fiber lifecycle bypassed its source methods');
    let replacementRuns = 0;
    runner.execute = () => { replacementRuns++; };
    await runnerMount.restart();
    if (runnerFiber._runner !== runner || runner.epoch !== '' || replacementRuns !== 1) throw new Error('Fiber lifecycle did not retain its current runner');
    for (const name of ['_reload', '_unload', 'await', 'restart']) {
      if (Object.getPrototypeOf(cordis.Fiber.prototype[name]) !== Object.getPrototypeOf(async () => {})) throw new Error('Fiber async method prototype changed');
    }
    await runnerMount.dispose();
    if (runnerFiber.uid !== null || runner.epoch !== '__INACTIVE__') throw new Error('Fiber disposal left an active runner');
    window.remotePathFiberResults = { sourceFields: true, sharedRunner: true, lifecycleDispatch: lifecycleTrace.slice(0, expectedLifecycle.length), asyncMethods: true, disposed: true };
    const delegatedParent = client.extend(), extendParent = delegatedParent.extend;
    let constructorContext, delegatedValue, delegatedConstructors = 0;
    Object.defineProperty(delegatedParent, 'extend', { value(metadata) { if (metadata.fiber) { delegatedConstructors++; constructorContext = metadata.fiber.ctx; } return extendParent.call(this, { ...metadata, delegated: true }); } });
    const delegatedMount = delegatedParent.plugin(ctx => { delegatedValue = ctx.delegated; ctx.provide('browser-parent-owned-service', 7); });
    await delegatedMount;
    if (delegatedConstructors !== 1 || constructorContext !== undefined || delegatedValue !== true || client.get('browser-parent-owned-service') !== 7) throw new Error('Fiber construction bypassed the parent extension or changed its ordering');
    await delegatedMount.dispose();
    if (client.get('browser-parent-owned-service') !== undefined) throw new Error('delegated Fiber did not own its service');
    window.remotePathFiberResults.parentExtension = true;
    const structuralTrace = [], structuralFiber = Object.assign(Object.create(cordis.Fiber.prototype), {
      uid: 1, state: 0, ctx: client, context: client, inject: {}, runtime: null, _config: {},
      _error: undefined, inertia: undefined, _store: {}, store: undefined, _disposables: new cordis.DisposableList(),
      _runner: { epoch: '__INACTIVE__', getOuterStack: () => [], execute() { structuralTrace.push('execute'); return () => structuralTrace.push('cleanup'); }, collect(dispose) { structuralFiber._disposables.push(dispose); } },
    });
    structuralFiber._setEpoch(''); await structuralFiber.await();
    structuralFiber._setEpoch('__INACTIVE__'); await structuralFiber.await();
    if (structuralFiber.state !== 0 || structuralFiber.store !== undefined || structuralTrace.join(',') !== 'execute,cleanup') throw new Error('structural Fiber lifecycle did not settle and clean up');
    window.remotePathFiberResults.structuralLifecycle = true;
    const boundaryRegistry = new cordis.RegistryService(client), registryCallback = () => {}, registryFailure = new Error('registry disposal failure'), registryTrace = [];
    boundaryRegistry._internal.set(registryCallback, { callback: registryCallback, fibers: { [Symbol.iterator]() { return { next() { return { done: false, value: { dispose() { throw registryFailure; } } }; }, return() { registryTrace.push('closed'); return {}; } }; } } });
    let registryCaught;
    try { boundaryRegistry.delete(registryCallback); } catch (error) { registryCaught = error; }
    if (registryCaught !== registryFailure || registryTrace.join(',') !== 'closed' || boundaryRegistry.has(registryCallback)) throw new Error('registry deletion lost iterator closure or error identity');
    window.remotePathFiberResults.registryIterator = true;
    const dependencyTrace = [], dependencyNames = [];
    dependencyNames[Symbol.iterator] = function* () { try { dependencyTrace.push('first'); yield 'first'; dependencyTrace.push('second'); yield 'second'; } finally { dependencyTrace.push('closed'); } };
    let dependencyFailure;
    try { cordis.Inject.resolve(dependencyNames, Object.freeze({})); } catch (error) { dependencyFailure = error; }
    if (!(dependencyFailure instanceof TypeError) || dependencyTrace.join(',') !== 'first,closed') throw new Error('dependency normalization ignored a refused write or failed to close its iterator');
    window.remotePathFiberResults.injectWriteFailure = true;
    if (typeof client.logger !== 'function' || client.logger.exporters.size !== 1) throw new Error('default browser logger was not installed');
    const loggerPayload = { marker: true };
    client.logger('BrowserProbe').info('browser ready', loggerPayload);
    const logged = client.logger.buffer.at(-1);
    if (logged.args[1] !== loggerPayload || logged.fiber.deref() !== client.fiber || logged.name !== 'BrowserProbe' || logged.type !== 'info' || !Number.isFinite(logged.ts)) throw new Error('browser log message identity or metadata changed');
    const formattedLog = cordis.Logger.format({}, logged);
    if (formattedLog !== 'browser ready {"marker":true}') throw new Error('browser logger formatter changed');
    client.intercept('logger', { name: 'browser-scope', level: 3 }).logger.debug('scoped debug');
    if (client.logger.buffer.at(-1).name !== 'browser-scope' || client.logger.buffer.at(-1).level !== 3) throw new Error('browser logger lost scoped config');
    const loggerFailure = new Error('browser startup log');
    const failedLoggingPlugin = client.plugin({ name: 'BrowserLogFailure', apply() { throw loggerFailure; } });
    let startupFailure;
    try { await failedLoggingPlugin.await(); } catch (error) { startupFailure = error; }
    if (startupFailure !== loggerFailure || client.logger.buffer.at(-1).args[0] !== loggerFailure || client.logger.buffer.at(-1).name !== 'browser-log-failure') throw new Error('Fiber failure did not reach the browser logger intact');
    await failedLoggingPlugin.dispose();
    window.remotePathLoggerResults = { installed: true, scoped: true, originalFailure: true, formatted: formattedLog };
    const originalEffectKey = cordis.symbols.effect, changedEffectKey = Symbol('browser effect key'), symbolTrace = [];
    try {
      cordis.symbols.effect = changedEffectKey;
      const dispose = client.effect(() => () => symbolTrace.push('cleanup'), 'symbol-key');
      if (cordis.Context.effect !== originalEffectKey || dispose[changedEffectKey]?.label !== 'symbol-key' || dispose[originalEffectKey] !== undefined) throw new Error('shared symbol mutation changed class metadata or lost effect tagging');
      await dispose();
    } finally { cordis.symbols.effect = originalEffectKey; }
    const composedReason = new Error('browser composed failure');
    composedReason.stack = 'Error: browser composed failure\n    at retained()\n    at marker()\n    at removed()';
    let composedFailure;
    try {
      cordis.composeError(info => { info.error = { stack: 'Error\nunused\n    at marker()' }; info.offset = 0; throw composedReason; }, () => ['    at browserOuter()']);
    } catch (error) { composedFailure = error; }
    function stackCapture() { return cordis.buildOuterStack(); }
    function stackOwner() { return stackCapture()(); }
    if (composedFailure !== composedReason || composedReason.stack !== 'Error: browser composed failure\n    at retained()\n    at browserOuter()' || !stackOwner()[0].includes('stackOwner') || symbolTrace.join(',') !== 'cleanup') throw new Error('browser stack composition or symbol-owned cleanup changed');
    window.remotePathSymbolStackResults = { mutableSymbol: true, errorIdentity: true, callerFrames: true };
    const fiberStackFrames = ['    at browserFiberOwner()'];
    const stackRuntime = { name: 'BrowserStackOwner', fibers: new cordis.DisposableList(), callback() { throw 'browser Fiber startup'; } };
    const stackFiber = new cordis.Fiber(client, undefined, {}, stackRuntime, () => fiberStackFrames);
    let fiberStackFailure;
    try { await stackFiber.await(); } catch (error) { fiberStackFailure = error; }
    if (!(fiberStackFailure instanceof Error) || fiberStackFailure.stack !== 'Error: browser Fiber startup\n    at browserFiberOwner()' || client.logger.buffer.at(-1).args[0] !== fiberStackFailure) throw new Error('Fiber startup lost composed caller frames');
    await stackFiber.dispose();
    const teardownRuntime = { name: 'BrowserTeardownStack', fibers: new cordis.DisposableList(), callback() { return () => { throw 'browser Fiber cleanup'; }; } };
    const stackTeardownFiber = new cordis.Fiber(client, undefined, {}, teardownRuntime, () => fiberStackFrames);
    await stackTeardownFiber.await(); await stackTeardownFiber.dispose();
    const teardownFailure = client.logger.buffer.at(-1).args[0];
    if (!(teardownFailure instanceof Error) || teardownFailure.stack !== 'Error: browser Fiber cleanup\n    at browserFiberOwner()') throw new Error('Fiber teardown lost composed caller frames');
    window.remotePathSymbolStackResults.fiberStartup = true;
    window.remotePathSymbolStackResults.fiberCleanup = true;
    if (!cordis.Context.is(client) || !cordis.Context.is(cordis.Context.prototype) || !(Symbol.for('cordis.is') in client)) throw new Error('Context brand is absent from its public prototype');
    const secondBinding = blob(assets['vendor/cordis/lib/client.js']);
    const secondUrl = blob(assets['vendor/cordis/lib/index.js'].replace("'./client.js'", JSON.stringify(secondBinding)).replace("new URL('./client_bg.wasm', import.meta.url)", `Uint8Array.from(atob(${JSON.stringify(bytes)}), c => c.charCodeAt(0))`));
    const secondCordis = await import(secondUrl);
    const secondContext = new secondCordis.Context();
    if (secondContext instanceof cordis.Context || !cordis.Context.is(secondContext) || !secondCordis.Context.is(client)) throw new Error('Context branding depends on one constructor copy');
    const foreignRegistry = new cordis.RegistryService(secondContext), foreignScope = secondContext.extend({ registry: foreignRegistry });
    foreignRegistry.ctx = foreignScope;
    const removeForeignDependency = foreignScope.provide('copy-dependency', { value: 7 });
    const foreignMount = foreignRegistry.plugin({ inject: ['copy-dependency'], apply(ctx) { ctx.provide('copy-service', ctx['copy-dependency']); } });
    const foreignFiber = await foreignMount;
    if (!(foreignFiber instanceof cordis.Fiber) || foreignFiber instanceof secondCordis.Fiber || foreignScope.get('copy-service').value !== 7) throw new Error('cross-copy registry used the wrong Fiber constructor or service context');
    await foreignMount.restart();
    await removeForeignDependency(); await foreignMount.await();
    if (foreignFiber.state !== 0 || foreignScope.get('copy-service') !== undefined) throw new Error('cross-copy dependency withdrawal failed');
    await foreignMount.dispose();
    window.remotePathFiberResults.crossCopy = true;
    await secondContext.fiber.dispose();
    URL.revokeObjectURL(secondUrl);
    URL.revokeObjectURL(secondBinding);
    const effectBoundaryRoot = new cordis.Context(), deniedEffect = new Error('parent effect refused'), boundaryPlugin = () => {};
    const parentEffect = effectBoundaryRoot.fiber.effect;
    effectBoundaryRoot.fiber.effect = () => { throw deniedEffect; };
    let deniedMount;
    try { effectBoundaryRoot.plugin(boundaryPlugin); } catch (error) { deniedMount = error; }
    if (deniedMount !== deniedEffect || effectBoundaryRoot.registry.get(boundaryPlugin).fibers.length !== 0) throw new Error('Fiber constructor bypassed a parent effect refusal');
    effectBoundaryRoot.fiber.effect = parentEffect;
    const acceptedMount = effectBoundaryRoot.plugin(boundaryPlugin);
    await acceptedMount; await acceptedMount.dispose(); await effectBoundaryRoot.fiber.dispose();
    window.remotePathFiberResults.parentEffect = true;
    let defaultRunnerFailure;
    try { cordis.Fiber.prototype._execute.call({}, { epoch: true, execute() { throw 'default stack failure'; }, collect() {} }); } catch (error) { defaultRunnerFailure = error; }
    if (defaultRunnerFailure?.constructor !== Error || defaultRunnerFailure.message !== 'default stack failure') throw new Error('runner omitted its default stack capture');
    window.remotePathFiberResults.defaultStack = true;
    const constructorTrace = [];
    class ClassPlugin {
      constructor(ctx, config) {
        this.ctx = ctx;
        constructorTrace.push(['construct', config]);
        this[Symbol.for('cordis.initHooks')] = [() => constructorTrace.push(['hook'])];
      }
      *[cordis.Service.init]() {
        constructorTrace.push(['init']);
        yield () => constructorTrace.push(['cleanup']);
      }
    }
    const constructedPlugin = client.plugin(ClassPlugin, 17);
    const constructedCore = await constructedPlugin;
    if (!(constructedCore instanceof cordis.Fiber) || constructedCore.runtime !== client.registry.get(ClassPlugin) || !(constructedCore.runtime.fibers instanceof cordis.DisposableList)) throw new Error('public Fiber or runtime construction changed');
    const fiberField = Object.getOwnPropertyDescriptor(constructedCore.ctx, 'fiber');
    if (Object.keys(constructedCore.ctx).join(',') !== 'fiber' || fiberField?.value !== constructedCore || !fiberField.writable || !fiberField.enumerable || !fiberField.configurable) throw new Error('plugin Context omitted its source Fiber field');
    window.remotePathContextResults.fiberField = true;
    await constructedPlugin.dispose();
    if (client.registry.has(ClassPlugin) || JSON.stringify(constructorTrace) !== JSON.stringify([['construct',17],['hook'],['init'],['cleanup']])) throw new Error('class initialization or owned cleanup changed');
    const directFiber = new cordis.Fiber(client, { direct: true }, {}, null, () => []);
    if (!(directFiber instanceof cordis.Fiber) || directFiber.ctx !== client || directFiber.then !== undefined) throw new Error('direct root Fiber construction changed');
    directFiber.effect(() => () => constructorTrace.push(['direct cleanup']));
    await directFiber.dispose();
    if (directFiber.config.direct !== true || constructorTrace.at(-1)[0] !== 'direct cleanup') throw new Error('direct Fiber restart or cleanup changed');
    window.remotePathConstructorResults = { classTrace: constructorTrace, directRoot: directFiber.state === 2, runtimeRemoved: !client.registry.has(ClassPlugin) };
    const serviceTrace = [], serviceInitializers = [], serviceLabel = Symbol('service-scope');
    const serviceScope = client.isolate('browser-decorated-service', serviceLabel);
    class DecoratedService extends cordis.Service {
      static provide = 'browser-decorated-service';
      constructor(ctx) { super(ctx); this.base = 3; for (const initialize of serviceInitializers) initialize.call(this); }
      [cordis.Service.invoke](value) { return this.base + value; }
      [cordis.Service.init]() { serviceTrace.push('init'); }
      connected() {
        const version = this.ctx['browser-decorated-dependency'].version;
        serviceTrace.push('run:' + version);
        return () => serviceTrace.push('cleanup:' + version);
      }
    }
    cordis.Inject('browser-decorated-dependency')(DecoratedService.prototype.connected, { kind: 'method', addInitializer(value) { serviceInitializers.push(value); } });
    const serviceOwner = serviceScope.plugin(DecoratedService);
    await serviceOwner;
    const callableService = client.isolate('browser-decorated-service', serviceLabel).get('browser-decorated-service');
    if (typeof callableService !== 'function' || !(callableService instanceof DecoratedService) || callableService(4) !== 7 || client.get('browser-decorated-service') !== undefined) throw new Error('callable Service construction or symbol isolation changed');
    const methodFiber = [...client.registry.values()].flatMap(runtime => [...runtime.fibers]).find(fiber => fiber.parent === serviceOwner.ctx);
    if (!methodFiber || methodFiber.state !== 0 || serviceTrace.join(',') !== 'init') throw new Error('method decorator ran without its dependency');
    const removeServiceDependency = client.provide('browser-decorated-dependency', { version: 1 });
    await methodFiber.await();
    await removeServiceDependency();
    await methodFiber.await();
    await serviceOwner.dispose();
    if (serviceTrace.join(',') !== 'init,run:1,cleanup:1' || client.registry.has(DecoratedService) || serviceScope.get('browser-decorated-service') !== undefined) throw new Error('decorated method or Service survived teardown');
    window.remotePathServiceResults = { callable: true, symbolScope: true, dependencyTrace: serviceTrace, removed: true };
    const reflectionService = { value: 3, add(value) { return this.value + value; } };
    const removeReflectionService = client.provide('browser-reflection-service', reflectionService);
    let reflectionContext, accessorReceiver, reflectionCarrier;
    const reflectionOwner = client.plugin(ctx => {
      reflectionContext = ctx.extend({ marker: 'reflection-owner' });
      ctx.accessor('browserComputed', {
        get(receiver, error) { accessorReceiver = this; return this.marker; },
        set(value, receiver, error) { error.stack = 'Error\n    at trap()\n    at browserSetter()'; reflectionCarrier = error; throw error; },
      });
      ctx.mixin('browser-reflection-service', { value: 'browserValue', add: 'browserAdd' });
    });
    await reflectionOwner;
    if (reflectionContext.browserComputed !== 'reflection-owner' || accessorReceiver !== reflectionContext || reflectionContext.browserAdd(2) !== 5) throw new Error('browser reflection accessor or mixin lost its receiver');
    reflectionContext.browserValue = 8;
    if (reflectionService.value !== 8 || client.browserAdd(2) !== 10) throw new Error('browser reflection mixin setter lost its service');
    let reflectedFailure;
    try { reflectionContext.browserComputed = 1; } catch (error) { reflectedFailure = error; }
    if (reflectedFailure !== reflectionCarrier || reflectedFailure.stack !== 'Error: cannot set property "browserComputed" without provide\n    at browserSetter()') throw new Error('browser reflection lost its caller error or stack enhancement');
    const removeReflectionHook = client.on('internal/get', (ctx, name, error, next) => name === 'browserVirtual' ? ctx.marker : next());
    if (reflectionContext.browserVirtual !== 'reflection-owner') throw new Error('browser reflection bypassed internal/get');
    await removeReflectionHook(); await reflectionOwner.dispose(); await removeReflectionService();
    if ('browserComputed' in client || 'browserValue' in client || 'browserAdd' in client) throw new Error('browser reflection definitions survived their owner');
    const filteredReflection = client.isolate('browser-filtered-reflection');
    let reflectionEnabled = false, reflectionRuns = 0;
    const removeFilteredReflection = filteredReflection.reflect.provide('browser-filtered-reflection', {}, () => reflectionEnabled);
    const filteredReflectionFiber = filteredReflection.inject(['browser-filtered-reflection'], () => { reflectionRuns++; });
    await filteredReflectionFiber.await();
    reflectionEnabled = true;
    const reflectionScope = filteredReflection[cordis.symbols.isolate]['browser-filtered-reflection'];
    const notifiedReflection = client.reflect.notify(['browser-filtered-reflection'], ctx => ctx[cordis.symbols.isolate]['browser-filtered-reflection'] === reflectionScope);
    await Promise.all(notifiedReflection.map(fiber => fiber.await()));
    if (notifiedReflection.length !== 1 || reflectionRuns !== 1 || filteredReflectionFiber.state !== 2) throw new Error('browser reflection ignored custom notification scope');
    await filteredReflectionFiber.dispose(); await removeFilteredReflection();
    const providerHookCalls = [], originalProviderEffect = client.fiber.effect;
    const rawReflection = client.reflect[cordis.symbols.original], originalProviderNotify = rawReflection.notify;
    client.fiber.effect = function (...args) { if (args[1] === 'ctx.provide("browser-provider-hook")') providerHookCalls.push('effect'); return originalProviderEffect.apply(this, args); };
    rawReflection.notify = function (...args) { if (args[0][0] === 'browser-provider-hook') providerHookCalls.push('notify'); return originalProviderNotify.apply(this, args); };
    const removeHookedProvider = client.provide('browser-provider-hook', {});
    await removeHookedProvider();
    client.fiber.effect = originalProviderEffect;
    rawReflection.notify = originalProviderNotify;
    if (providerHookCalls.join(',') !== 'effect,notify,notify') throw new Error('browser provider bypassed public effect or notification methods');
    const removeFailingProvider = client.provide('browser-withdrawal-failure', {}), withdrawalError = new Error('browser withdrawal failure');
    rawReflection.notify = function (...args) { if (args[0][0] === 'browser-withdrawal-failure') throw withdrawalError; return originalProviderNotify.apply(this, args); };
    const withdrawal = removeFailingProvider();
    if (!(withdrawal instanceof Promise)) throw new Error('browser provider cleanup lost Promise timing');
    let withdrawalFailure;
    try { await withdrawal; } catch (error) { withdrawalFailure = error; }
    rawReflection.notify = originalProviderNotify;
    if (withdrawalFailure !== withdrawalError || client.get('browser-withdrawal-failure') !== undefined) throw new Error('browser provider cleanup lost rejection identity or removal');
    window.remotePathReflectionResults = { accessor: true, mixin: true, interceptor: true, errorIdentity: true, notification: true, providerHooks: true, cleanupRejection: true, removed: true };
    const effectRoot = new cordis.Context(), effectTrace = [];
    let finishEffectSetup, finishEffectCleanup;
    const effectSetup = new Promise(resolve => { finishEffectSetup = resolve; });
    const effectCleanup = new Promise(resolve => { finishEffectCleanup = resolve; });
    if (effectRoot.fiber.getEffects().length) throw new Error('constructor effects leaked into root diagnostics');
    const outerEffect = effectRoot.effect(() => ({ [Symbol.asyncIterator]() {
      let step = 0;
      return { next() {
        if (step++ === 0) return { value: effectRoot.effect(() => () => effectTrace.push('nested'), 'nested'), done: false };
        return effectSetup.then(() => ({ value: () => { effectTrace.push('late'); return effectCleanup; }, done: true }));
      } };
    } }), 'outer');
    for (let i = 0; i < 8; ++i) await Promise.resolve();
    if (JSON.stringify(effectRoot.fiber.getEffects()) !== JSON.stringify([{ label: 'outer', children: [{ label: 'nested', children: [] }] }])) throw new Error('effect diagnostics do not reflect nested ownership');
    const effectTask = outerEffect();
    if (!(effectTask instanceof Promise) || outerEffect() !== undefined) throw new Error('pending effect disposal lost its single-shot result');
    finishEffectSetup();
    const readyEffectDispose = await outerEffect;
    if (typeof readyEffectDispose !== 'function' || readyEffectDispose() !== undefined || effectTrace.join(',') !== 'late') throw new Error('awaitable effect setup or serial cleanup changed');
    let effectOwnerSettled = false;
    const effectOwnerTask = effectRoot.fiber.dispose().then(() => { effectOwnerSettled = true; });
    for (let i = 0; i < 8; ++i) await Promise.resolve();
    if (effectOwnerSettled) throw new Error('structural owner did not join pending effect cleanup');
    finishEffectCleanup();
    await effectTask; await effectOwnerTask;
    if (effectTrace.join(',') !== 'late,nested' || effectRoot.fiber.getEffects().length) throw new Error('effect cleanup or metadata escaped its owner');
    const effectChild = client.plugin({ *apply() { yield () => {}; return () => {}; } });
    await effectChild; await effectChild.dispose();
    let inactiveEffect;
    try { effectChild.effect(() => {}); } catch (error) { inactiveEffect = error; }
    if (!(inactiveEffect instanceof cordis.CordisError) || inactiveEffect.code !== 'INACTIVE_EFFECT' || inactiveEffect.message !== cordis.CordisError.Code.INACTIVE_EFFECT) throw new Error('inactive Fiber lost its public CordisError');
    window.remotePathEffectResults = { iterable: true, awaitable: true, nestedCleanup: effectTrace, ownerJoined: effectOwnerSettled, inactiveCode: inactiveEffect.code };
    const realm = document.createElement('iframe'); document.body.append(realm);
    const foreign = new realm.contentWindow.Object(); foreign[realm.contentWindow.Symbol.for('cordis.is')] = true;
    if (foreign instanceof Object || !cordis.Context.is(foreign)) throw new Error('Context brand does not cross browser realms');
    realm.remove();
    class TracedService extends cordis.Service {
      constructor(ctx) { super(ctx, 'trace-probe'); }
      current() { return this.ctx; }
      own(log) { this.ctx.effect(() => () => log.push('disposed'), 'traced callback effect'); }
    }
    const tracedService = new TracedService(client), traceLog = [];
    const traceFiber = client.plugin({ name: 'callback-service-tracing', apply(ctx) {
      ctx.on('probe/traced-callback', function (argument) {
        if (this.ctx !== ctx || argument.ctx !== ctx) throw new Error('callback lost its registering context');
        argument.own(traceLog);
        return argument.current();
      });
    } });
    await traceFiber;
    if (client.bail(tracedService, 'probe/traced-callback', tracedService) !== traceFiber.ctx) throw new Error('tracked method return lost its caller');
    await traceFiber.dispose();
    if (traceLog.join(',') !== 'disposed') throw new Error('traced callback effect escaped its owner');
    const updateLog = [], updateFibers = new Map();
    const updateProbe = label => client.plugin({ name: 'update-probe-' + label, apply(ctx, config) {
      updateLog.push(label + ':apply:' + config.version);
      ctx.on('internal/update', function (config, noSave, next) {
        if (this !== updateFibers.get(label)) throw new Error('update hook lost its Fiber receiver');
        updateLog.push(label + ':update:' + noSave);
        return config.veto ? 'vetoed' : next();
      });
      ctx.effect(() => () => updateLog.push(label + ':dispose'));
    } }, { version: 1 });
    const firstUpdate = updateProbe('first'), secondUpdate = updateProbe('second');
    updateFibers.set('first', firstUpdate); updateFibers.set('second', secondUpdate);
    await firstUpdate; await secondUpdate; updateLog.length = 0;
    if (firstUpdate.update({ version: 2, veto: true }, true) !== 'vetoed') throw new Error('update veto was not synchronous');
    if (firstUpdate.config.version !== 1 || firstUpdate._config.version !== 2) throw new Error('update veto changed committed config');
    const updatedConfig = { version: 3 };
    await firstUpdate.update(updatedConfig);
    if (firstUpdate.config !== updatedConfig || updateLog.join(',') !== 'first:update:true,first:update:false,first:dispose,first:apply:3') throw new Error('update crossed Fiber ownership or lost config identity');
    if (Object.getPrototypeOf(firstUpdate) !== firstUpdate.ctx.fiber || await firstUpdate !== firstUpdate.ctx.fiber) throw new Error('awaitable Fiber handle lost its prototype identity');
    await firstUpdate.dispose(); await secondUpdate.dispose();
    const updateFailure = new Error('browser restart failure');
    const recoveringUpdate = await client.plugin({ name: 'update-recovery', apply(ctx, config) { if (config.fail) throw updateFailure; } }, {});
    const failedUpdate = recoveringUpdate.update({ fail: true });
    if (recoveringUpdate.state !== 5) throw new Error('restart admission did not enter UNLOADING synchronously');
    try { await failedUpdate; throw new Error('failed restart resolved'); } catch (error) { if (error !== updateFailure) throw error; }
    if (recoveringUpdate.state !== 3 || !recoveringUpdate._config.fail) throw new Error('failed restart rolled back its config or lost FAILED state');
    recoveringUpdate.update({}); await recoveringUpdate.await();
    if (recoveringUpdate.state !== 2) throw new Error('failed Fiber did not recover');
    await recoveringUpdate.dispose();
    const teardownStarted = [], teardownCompleted = [];
    let finishTeardown, releaseDependency;
    const teardownGate = new Promise(resolve => { finishTeardown = resolve; });
    const teardownDependency = new Promise(resolve => { releaseDependency = resolve; });
    const teardownFiber = client.plugin({ name: 'dependent-disposers', apply(ctx) {
      ctx.effect(() => async () => { teardownStarted.push('first'); releaseDependency(); await teardownGate; teardownCompleted.push('first'); });
      ctx.effect(() => async () => { teardownStarted.push('second'); await teardownDependency; teardownCompleted.push('second'); });
    } });
    await teardownFiber;
    let teardownSettled = false;
    const teardown = teardownFiber.dispose().then(() => { teardownSettled = true; });
    try {
      await new Promise(resolve => setTimeout(resolve, 0));
      if (teardownStarted.join(',') !== 'second,first' || teardownCompleted.join(',') !== 'second' || teardownSettled) throw new Error('dependent async disposers did not start together or were not joined');
    } finally { releaseDependency(); finishTeardown(); await teardown; }
    if (teardownCompleted.join(',') !== 'second,first' || !teardownSettled) throw new Error('async teardown did not settle completely');
    const tableValues = [], table = client.events._hooks;
    if (!(client.events instanceof cordis.EventsService) || cordis.isBailed(false) || !cordis.isBailed(0)) throw new Error('public event exports changed');
    const independent = new cordis.EventsService(client), independentValues = [];
    const independentRemove = independent.on('probe/independent-service', value => independentValues.push(value));
    client.emit('probe/independent-service', 'root');
    independent.emit('probe/independent-service', 'independent');
    independentRemove();
    if (independentValues.join(',') !== 'independent' || independent._hooks === table) throw new Error('independent event service leaked into its Context');
    if (client.events === client || client.events.ctx !== client || Object.getPrototypeOf(table) !== Object.prototype) throw new Error('event service face changed');
    let tableOwner, removeTableHook;
    const tableFiber = client.plugin({ name: 'hook-table-owner', apply(ctx) {
      tableOwner = ctx;
      removeTableHook = ctx.on('probe/hook-table', value => tableValues.push(value), { marker: 17 });
    } });
    await tableFiber;
    const tableList = table['probe/hook-table'], tableHook = tableList[0], tableCallback = tableHook.callback;
    if (tableHook.ctx !== tableOwner || tableHook.marker !== 17) throw new Error('event record lost owner or options');
    tableHook.callback = value => tableValues.push('changed:' + value);
    client.emit('probe/hook-table', 'first');
    tableHook.callback = tableCallback;
    const replacementList = [];
    table['probe/hook-table'] = replacementList;
    removeTableHook();
    if (tableList.length || table['probe/hook-table'] !== replacementList) throw new Error('event disposal changed a replacement list');
    const extra = value => tableValues.push(value);
    tableOwner.events.register('explicit browser hook', replacementList, extra, {});
    client.emit('probe/hook-table', 'second');
    if (client.events.unregister(replacementList, extra) !== true || client.events.unregister(replacementList, extra) !== undefined) throw new Error('callback removal changed');
    tableOwner.events.register('owned browser hook', replacementList, extra, {});
    await tableFiber.dispose();
    client.emit('probe/hook-table', 'withdrawn');
    if (replacementList.length || tableValues.join(',') !== 'changed:first,second') throw new Error('mutable event records escaped Fiber ownership');
    const symbolEvent = Symbol('owned-event'), symbolValues = [];
    const symbolFiber = client.plugin({ name: 'symbol-event-owner', apply(ctx) { ctx.on(symbolEvent, value => symbolValues.push(value)); } });
    await symbolFiber;
    try { client.emit(symbolEvent, 'rejected'); throw new Error('symbol dispatch unexpectedly succeeded'); }
    catch (error) { if (!(error instanceof TypeError) || error.message !== 'name.startsWith is not a function') throw error; }
    const symbolPrefix = Object.getOwnPropertyDescriptor(Symbol.prototype, 'startsWith');
    try {
      Object.defineProperty(Symbol.prototype, 'startsWith', { configurable: true, value() { return false; } });
      client.emit(symbolEvent, 'delivered');
      await symbolFiber.dispose();
      client.emit(symbolEvent, 'withdrawn');
      if (symbolValues.join(',') !== 'delivered') throw new Error('symbol listener was lost or escaped disposal');
    } finally {
      if (symbolPrefix) Object.defineProperty(Symbol.prototype, 'startsWith', symbolPrefix);
      else delete Symbol.prototype.startsWith;
    }
    const eventTrace = [];
    let eventOwner;
    const interceptedDisposer = () => {};
    const eventFiber = client.plugin({ name: 'browser-event-lifecycle', apply(ctx) {
      eventOwner = ctx;
      ctx.once('probe/once', () => { eventTrace.push('once'); client.emit('probe/once'); });
      ctx.on('probe/waterfall', (value, next) => next() + value);
      ctx.on('internal/listener', function (name) {
        if (name === 'probe/intercepted') {
          if (this !== ctx) throw new Error('listener interception lost its registering Context');
          return interceptedDisposer;
        }
      });
      if (ctx.on('probe/intercepted', () => { throw new Error('interception did not replace registration'); }) !== interceptedDisposer) throw new Error('interceptor return identity changed');
    } });
    await eventFiber;
    client.emit('probe/once');
    client.emit('probe/once');
    if (eventTrace.join(',') !== 'once' || client.waterfall('probe/waterfall', 3, () => 4) !== 7) throw new Error('browser once or waterfall semantics changed');
    await eventFiber.dispose();
    client.emit('probe/once');
    client.emit('probe/intercepted');
    if (client.waterfall('probe/waterfall', 3, () => 4) !== 4 || eventTrace.length !== 1) throw new Error('browser event handlers survived plugin teardown');
    let inactiveRejected = false;
    try { eventOwner.on('probe/intercepted', () => {}); } catch { inactiveRejected = true; }
    if (!inactiveRejected) throw new Error('inactive event owner accepted registration');
    let metadataReads = 0, getterReceiver, setterReceiver;
    const getter = function () { metadataReads++; getterReceiver = this; return this.contextMarker; };
    const setter = function (value) { setterReceiver = this; this.contextMarker = value; };
    const token = Symbol('metadata'), tokenValue = Object.freeze({ value: 1 });
    const extension = { contextMarker: 'initial' };
    Object.defineProperty(extension, 'contextValue', { get: getter, set: setter, enumerable: false, configurable: false });
    Object.defineProperty(extension, token, { value: tokenValue, writable: false, enumerable: false, configurable: false });
    const metadataContext = client.extend(extension);
    if (metadataReads !== 0) throw new Error('Context.extend invoked a metadata getter');
    const descriptor = Object.getOwnPropertyDescriptor(metadataContext, 'contextValue');
    if (!descriptor || descriptor.get !== getter || descriptor.set !== setter || descriptor.enumerable || descriptor.configurable) throw new Error('Context.extend changed property descriptors');
    if (!('contextValue' in metadataContext) || metadataReads !== 0) throw new Error('metadata membership invoked a getter');
    if (Object.getPrototypeOf(metadataContext) !== client || metadataContext.contextValue !== 'initial' || getterReceiver !== metadataContext) throw new Error('metadata getter lost its context receiver or prototype');
    metadataContext.contextValue = 'updated';
    if (setterReceiver !== metadataContext || metadataContext.contextMarker !== 'updated') throw new Error('metadata setter lost its receiver');
    const nestedMetadata = metadataContext.extend({ contextMarker: 'nested' });
    if (nestedMetadata.contextValue !== 'nested' || getterReceiver !== nestedMetadata) throw new Error('inherited getter used the parent receiver');
    if (metadataContext[token] !== tokenValue || Reflect.set(metadataContext, token, {})) throw new Error('readonly symbol metadata changed');
    const lookupScope = client.isolate('lookup-lifecycle', 'browser-loading');
    let releaseLookup, announceLookup;
    const lookupGate = new Promise(resolve => { releaseLookup = resolve; });
    const lookupReady = new Promise(resolve => { announceLookup = resolve; });
    const lookupFiber = lookupScope.plugin({ name: 'lookup-lifecycle', async apply(ctx) {
      ctx.provide('lookup-lifecycle', { value: 'loading', field: 42 });
      announceLookup();
      await lookupGate;
    } });
    try {
      await lookupReady;
      if (lookupScope.get('lookup-lifecycle') !== undefined || lookupScope.get('lookup-lifecycle', false)?.value !== 'loading') throw new Error('strict and relaxed loading visibility diverged');
      if (client.get('lookup-lifecycle', false) !== undefined) throw new Error('relaxed lookup escaped isolation');
      releaseLookup();
      await lookupFiber.await();
      lookupScope.mixin('lookup-lifecycle', ['field']);
      if (lookupScope.get('lookup-lifecycle')?.value !== 'loading' || lookupScope.field !== 42 || lookupScope.get('field') !== undefined) throw new Error('explicit lookup and reflected property access diverged');
    } finally {
      releaseLookup();
      await lookupFiber.dispose();
    }
    if (lookupScope.get('lookup-lifecycle', false) !== undefined) throw new Error('relaxed lookup retained a withdrawn provider');
    if (loaderAssets) {
      const loadRuntime = async value => {
        const bindings = blob(value.bindings);
        const url = blob(value.wrapper.replace(`'./${value.stem}.js'`, JSON.stringify(bindings)).replace(`new URL('./${value.stem}_bg.wasm', import.meta.url)`, `Uint8Array.from(atob(${JSON.stringify(value.bytes)}), c => c.charCodeAt(0))`));
        const module = await import(url); URL.revokeObjectURL(url); URL.revokeObjectURL(bindings); return module;
      };
      const loaderModule = await loadRuntime(loaderAssets.loader);
      const modulesModule = await loadRuntime(loaderAssets.modules);
      const boot = modulesModule.parseBootManifest(loaderAssets.boot);
      const modules = new modulesModule.ClientModuleSystem({ modules: boot.modules, staticModules: { '@seekdeep-ai/cordis': cordis } });
      await client.plugin(loaderModule.default);
      const loader = client.get('loader'); loader.internal = modules;
      const disabled = await loader.create({ id: 'disabled', name: '@seekdeep-ai/seekdeep-api-remotes', disabled: true });
      if (loader.resolve(disabled).fiber !== undefined) throw new Error('disabled Loader entry acquired a fiber');
      if (modules.loadCache.has('@seekdeep-ai/seekdeep-api-remotes')) throw new Error('disabled Loader entry imported its module');
      await loader.remove(disabled);
      const entries = [
        { id: 'remotes', name: '@seekdeep-ai/seekdeep-api-remotes' },
        { id: 'gateway', name: '@seekdeep-ai/seekdeep-api-gateway' },
        { id: 'registry', name: '@seekdeep-ai/seekdeep-typert-registry' },
        { id: 'connection', name: '@seekdeep-ai/seekdeep-client-connection' },
      ];
      await Promise.all(entries.map(entry => loader.create(entry))); await loader.await();
      const immerUrl = blob(loaderAssets.immer);
      const immer = await import(immerUrl);
      modules.registerStatic('immer', immer);
      URL.revokeObjectURL(immerUrl);
      const runtime = await modules.import('@seekdeep-ai/seekdeep-client-runtime', '', undefined);
      const sessions = new runtime.SessionRuntime(client, client.connection.api, client.remote);
      const workspaces = new runtime.WorkspaceRuntime(client, client.connection.api, sessions);
      let pickerFailure;
      try { await workspaces.pickDirectory(); } catch (error) { pickerFailure = error; }
      if (!(pickerFailure instanceof Error)) throw new Error('native picker accepted the browse-only Host');
      window.remotePathNativePicker = { message: pickerFailure.message };
      window.remotePathLoader = { loader, modules, connection: entries[3] };
    } else {
      window.__ModuleLoader__ = { load(row) { handoffs.set(row.id, row); } };
      for (const path of Object.keys(assets).filter(path => path.startsWith('packages/'))) {
        const script = document.createElement('script'); script.textContent = assets[path]; document.head.append(script);
      }
      for (const id of ['@seekdeep-ai/seekdeep-typert-registry', '@seekdeep-ai/seekdeep-client-connection', '@seekdeep-ai/seekdeep-api-gateway', '@seekdeep-ai/seekdeep-api-remotes']) {
        const row = handoffs.get(id); if (!row) throw new Error('missing built module ' + id);
        const plugin = row.factory(); await client.plugin(plugin);
      }
    }
    window.remotePathClient = client;
    if (typedSource) {
      const zodUrl = blob(zodSource);
      const importMap = document.createElement('script');
      importMap.type = 'importmap'; importMap.textContent = JSON.stringify({ imports: { zod: zodUrl } }); document.head.append(importMap);
      const contributions = [];
      for (const contract of publicContracts) {
        const url = blob(contract.code);
        const value = await import(url);
        if (Object.keys(value).sort().join(',') !== 'TYPERT_REMOTE,default' || value.default !== value.TYPERT_REMOTE || value.default.package !== contract.name) throw new Error('invalid public Remote module ' + contract.name);
        contributions.push(value.default);
        URL.revokeObjectURL(url);
      }
      const url = blob(typedSource);
      const typed = await import(url);
      window.remotePathTyped = { Context: cordis.Context, handoffs, contributions, run: typed.typedRemote };
      URL.revokeObjectURL(url);
      URL.revokeObjectURL(zodUrl);
    }
    URL.revokeObjectURL(binding); URL.revokeObjectURL(module);
  }, { assets, bytes, publicContracts, typedSource, zodSource, loaderAssets });
  const result = await evaluate(async ({ root }) => {
    const client = window.remotePathClient;
    const assert = (value, message) => { if (!value) throw new Error(message); };
    let sequence = 0;
    const callHost = async (method, payload) => {
      const rpcId = `setup-${++sequence}`;
      const response = await fetch('/api/' + method, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ type: 'client-request', rpcId, method, payload }) });
      const result = await response.json(); assert(result.rpcId === rpcId && result.result.ok, 'Host setup failed: ' + JSON.stringify(result)); return result.result.value;
    };
    const first = await callHost('session.create', { cwd: root });
    const second = await callHost('session.create', { cwd: root });
    client.typert.contexts.registerClient('agent', { identity: ctx => ctx.agentId });
    const descriptors = client.typert.remotes.list();
    assert(descriptors.length === 24, 'incomplete Remote descriptor inventory');
    assert(descriptors.every(d => d.result.typeSymbol && d.sourceLocation && d.parameters.every(p => p.codec.typeSymbol)), 'incomplete generated descriptor metadata');
    let invalidRejected = false;
    try { await client.remote.goals.create(first.sessionId, { objective: 1 }); } catch { invalidRejected = true; }
    assert(invalidRejected, 'invalid input accepted');
    const created = await client.remote.goals.create(first.sessionId, { objective: 'integrated root goal' });
    assert(created.ok, 'Goal creation failed: ' + JSON.stringify(created));
    const edited = await client.remote.goals.edit(first.sessionId, created.value.ref, { objective: 'edited integrated goal' });
    assert(edited.ok && edited.value.revision === 2, 'Goal edit failed: ' + JSON.stringify(edited));
    const scoped = client.extend({ agentId: second.sessionId });
    const scopedGoal = await scoped.remote.goals.create({ objective: 'integrated scoped goal', maxGoalRounds: 3 });
    assert(scopedGoal.ok, 'scoped Goal creation failed: ' + JSON.stringify(scopedGoal));
    let currentIdentity = first.sessionId;
    const liveIdentity = client.extend({ get agentId() { return currentIdentity; } });
    assert((await liveIdentity.remote.commands.list()).ok, 'first live getter call failed');
    currentIdentity = second.sessionId;
    assert((await liveIdentity.remote.commands.list()).ok, 'changed live getter call failed');
    const commands = await client.remote.commands.list(first.sessionId);
    assert(commands.ok && Array.isArray(commands.value), 'command listing failed: ' + JSON.stringify(commands));
    const unknownCommand = await client.remote.commands.execute(first.sessionId, '/__remote_path_unknown_command__');
    assert(unknownCommand.ok && unknownCommand.value === undefined, 'undefined command result was not preserved: ' + JSON.stringify(unknownCommand));
    const cancellation = new AbortController(); cancellation.abort(new Error('remote path cancelled'));
    const cancelled = await client.remote.commands.execute(first.sessionId, '/__remote_path_unknown_command__', cancellation.signal);
    assert(!cancelled.ok && cancelled.error.code === 'internal', 'cancelled call escaped the error branch');
    const failed = await client.remote.goals.edit(first.sessionId, { id: 'missing-goal', revision: 1 }, { objective: 'must fail' });
    assert(!failed.ok, 'missing Goal unexpectedly succeeded');
    const inventory = await client.remote.dynamicCordisRunner.inventory();
    assert(inventory.ok, 'dynamic inventory failed: ' + JSON.stringify(inventory));
    const plugins = await client.remote.pluginInventory.list();
    assert(plugins.ok, 'plugin inventory failed: ' + JSON.stringify(plugins));
    const feedback = await client.remote.messageFeedback.list({ sessionId: first.sessionId });
    assert(feedback.ok, 'feedback listing failed: ' + JSON.stringify(feedback));
    const firstHistory = await callHost('session.history', { sessionId: first.sessionId });
    const secondHistory = await callHost('session.history', { sessionId: second.sessionId });
    const count = history => history.events.filter(entry => entry.event.type === 'goal/change').length;
    assert(count(firstHistory) === 2 && count(secondHistory) === 1, 'durable Goal events differ');
    let typed;
    if (window.remotePathTyped) {
      const runtime = window.remotePathTyped;
      const consumer = new runtime.Context();
      for (const id of ['@seekdeep-ai/seekdeep-typert-registry', '@seekdeep-ai/seekdeep-client-connection', '@seekdeep-ai/seekdeep-api-gateway']) await consumer.plugin(runtime.handoffs.get(id).factory());
      for (const contribution of runtime.contributions) await consumer.remote.$mount(contribution);
      const registry = consumer.typert;
      assert(registry.remotes.list().length === 24, 'public package contributions are incomplete');
      const session = await callHost('session.create', { cwd: root });
      const result = await runtime.run(consumer, session.sessionId);
      assert(result.revision === 2 && result.commands > 0, 'checked public consumer failed');
      const history = await callHost('session.history', { sessionId: session.sessionId });
      assert(count(history) === 2, 'checked public consumer did not persist both Goal operations');
      await consumer.fiber.dispose();
      assert(registry.remotes.list().length === 0, 'public contributions survived teardown');
      typed = { ...result, packages: runtime.contributions.length, goalEvents: count(history), remainingDescriptors: registry.remotes.list().length };
    }
    const registry = client.typert;
    let lifecycle;
    if (window.remotePathLoader) {
      const { loader, modules, connection } = window.remotePathLoader;
      const retained = client.remote.goals.edit;
      await loader.remove(connection.id); await loader.await();
      assert(registry.remotes.list().length === 0, 'provider loss retained Remote descriptors');
      assert(loader.resolve('gateway').fiber.state === 0 && loader.resolve('remotes').fiber.state === 0, 'dependents did not become pending');
      const stale = await retained(first.sessionId, { id: edited.value.id, revision: 2 }, { objective: 'stale handle must not run' });
      assert(!stale.ok, 'retained handle survived provider loss');
      await loader.create(connection); await loader.await();
      assert(registry.remotes.list().length === 24, 'provider remount did not restore descriptors');
      const resumed = await client.remote.goals.edit(first.sessionId, { id: edited.value.id, revision: 2 }, { objective: 'remounted goal' });
      assert(resumed.ok && resumed.value.revision === 3, 'remounted call failed');
      const history = await callHost('session.history', { sessionId: first.sessionId });
      assert(count(history) === 3, 'remounted call did not persist');
      await loader.remove('remotes'); await loader.await();
      assert(registry.remotes.list().length === 0, 'assembly unload retained descriptors');
      const assembly = await modules.import('@seekdeep-ai/seekdeep-api-remotes', '', {});
      const cleanupTrace = [];
      const cleanupFailure = new Error('injected Remote cleanup failure');
      let failOnce = true, cleanup;
      modules.registerStatic('fixture:cleanup-retry', { inject: ['remote'], apply(ctx) {
        const remote = ctx.get('remote');
        const intercepted = new Proxy(remote, { get(target, key, receiver) {
          if (key !== '$mount') return Reflect.get(target, key, receiver);
          return async contribution => {
            const owned = await target.$mount(contribution);
            return async () => {
              cleanupTrace.push(contribution.package);
              if (failOnce && contribution.package.endsWith('-message-feedback')) { failOnce = false; throw cleanupFailure; }
              await owned();
            };
          };
        } });
        const context = new Proxy(ctx, { get(target, key, receiver) {
          if (key === 'get') return name => name === 'remote' ? intercepted : target.get(name);
          return Reflect.get(target, key, receiver);
        } });
        const mounted = assembly.apply(context);
        mounted.then(value => { cleanup = value; });
        return mounted;
      } });
      await loader.create({ id: 'retry', name: 'fixture:cleanup-retry' }); await loader.await();
      assert(registry.remotes.list().length === 24, 'retry assembly did not mount the real registry');
      const failedCleanup = cleanup();
      assert(cleanupTrace.length === 1 && cleanupTrace[0].endsWith('-message-feedback'), 'cleanup did not start synchronously');
      let failure;
      try { await failedCleanup; } catch (error) { failure = error; }
      assert(failure === cleanupFailure && registry.remotes.list().length === 24, 'cleanup failure lost identity or advanced past the failed disposer');
      const retriedCleanup = cleanup();
      assert(cleanupTrace[1].endsWith('-commands'), 'cleanup retry did not reverse the retained array');
      await retriedCleanup;
      assert(registry.remotes.list().length === 0, 'cleanup retry retained descriptors');
      await cleanup();
      assert(cleanupTrace.length === 11, 'repeated cleanup skipped the retained handles');
      await loader.remove('retry'); await loader.await();
      await loader.create({ id: 'remotes', name: '@seekdeep-ai/seekdeep-api-remotes' }); await loader.await();
      assert(registry.remotes.list().length === 24, 'remount after cleanup failure did not recover');
      lifecycle = { entries: loader.entries().length, modules: modules.loadCache.size, goalEvents: count(history), staleRejected: !stale.ok, cleanupFailurePreserved: failure === cleanupFailure, cleanupCalls: cleanupTrace.length };
    }
    await client.fiber.dispose();
    assert(registry.remotes.list().length === 0, 'Remote descriptors survived teardown');
    return { descriptors: descriptors.length, invalidRejected, undefinedPreserved: true, cancellation: cancelled.error, hostFailure: failed.error, rootGoalEvents: count(firstHistory), scopedGoalEvents: count(secondHistory), commands: commands.value.length, namespaceCalls: 5, remainingDescriptors: registry.remotes.list().length, ...(typed ? { typed } : {}), ...(lifecycle ? { lifecycle } : {}), ...(window.remotePathNativePicker ? { nativePicker: window.remotePathNativePicker } : {}) };
  }, { root });
  const goalCreates = requests.filter(path => path === '/api/goals/create').length;
  if (commandIdentities.length < 3 || commandIdentities[0] === commandIdentities[1] || commandIdentities[0] !== commandIdentities[2]) throw new Error('live context getter did not route the current Agent identity');
  if (goalCreates !== (typedConsumer ? 3 : 2)) throw new Error('invalid request reached Host or valid request was lost: ' + JSON.stringify(requests));
  if (typedConsumer && !result.typed) throw new Error('checked consumer was not exercised');
  if (loaderMode && requests.filter(path => path === '/api/goals/edit').length !== 3) throw new Error('stale Loader handle reached the Host or remounted call was lost');
  await Promise.all(responseReads);
  if (loaderMode) {
    const rejection = hostErrors.find(error => error.code === 'directory-picker-unavailable');
    if (!rejection || result.nativePicker?.message !== 'directory picker failed: ' + rejection.message || requests.filter(path => path === '/api/host.pickDirectory').length !== 1) throw new Error('public picker binding did not preserve the real Host rejection');
  }
  if (!hostErrors.some(error => JSON.stringify(error) === JSON.stringify(result.hostFailure))) throw new Error('gateway did not preserve the Host error verbatim');
  if (requests.filter(path => path === '/api/commands/execute').length !== 1) throw new Error('pre-aborted command reached the Host');
  await page.evaluate(result => { const pre = document.createElement('pre'); pre.textContent = JSON.stringify(result, null, 2); document.body.replaceChildren(pre); }, result);
  await page.screenshot({ path: join(output, 'remote-path.png'), fullPage: true });
  console.log(JSON.stringify({ ...result, browserContext: await page.evaluate(() => window.remotePathContextResults), browserFiber: await page.evaluate(() => window.remotePathFiberResults), browserLogger: await page.evaluate(() => window.remotePathLoggerResults), browserSymbolStacks: await page.evaluate(() => window.remotePathSymbolStackResults), browserReflection: await page.evaluate(() => window.remotePathReflectionResults), browserServices: await page.evaluate(() => window.remotePathServiceResults), browserConstructors: await page.evaluate(() => window.remotePathConstructorResults), browserEffects: await page.evaluate(() => window.remotePathEffectResults), browserEventLifecycle: true, browser: await browser.version(), requests }));
} finally {
  if (browser) await browser.close();
  if (server && server.exitCode === null && server.signalCode === null) {
    const exited = once(server, 'exit');
    let forced = false;
    server.kill('SIGINT');
    const shutdownDeadline = setTimeout(() => { forced = true; server.kill('SIGKILL'); }, 30000);
    const [code, signal] = await exited;
    clearTimeout(shutdownDeadline);
    if (forced || code !== 130) throw new Error(`Rust Host shutdown failed: code=${code}, signal=${signal}, forced=${forced}`);
    console.log(JSON.stringify({ hostShutdown: { code, signal, forced } }));
  }
  await rm(home, { recursive: true, force: true });
}
"#;
