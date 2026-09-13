//! JavaScript compatibility bindings backed by the Rust worker engine.

use seekdeep_code_runtime::CodeBindingNamespace;
use std::fmt::Write as _;

pub(crate) const WORKER_GLOBALS: &str = r"
(() => {
  const capturedObject = Object;
  const capturedPromise = Promise;
  const capturedString = String;
  const log = globalThis.__seekdeep_log__;
  const exit = globalThis.__seekdeep_exit__;
  const sleep = globalThis.__seekdeep_sleep__;
  const portControl = globalThis.__seekdeep_port_control__;
  const portCall = globalThis.__seekdeep_port_call__;
  const defineData = (target, key, value, enumerable = true, configurable = true) => {
    const descriptor = capturedObject.create(null);
    descriptor.configurable = configurable;
    descriptor.enumerable = enumerable;
    descriptor.writable = false;
    descriptor.value = value;
    capturedObject.defineProperty(target, key, descriptor);
  };
  const listeners = [];
  const deliverReply = message => {
    for (let index = 0; index < listeners.length; index++) {
      const listener = listeners[index];
      if (typeof listener === 'function') listener(message);
    }
  };
  defineData(globalThis, '__seekdeep_deliver_reply__', deliverReply, false, false);
  const parentPort = capturedObject.create(null);
  defineData(parentPort, 'postMessage', message => {
    if (!portControl(message)) void portCall(message);
  });
  defineData(parentPort, 'on', (event, listener) => {
    if (event === 'message' && typeof listener === 'function') defineData(listeners, listeners.length, listener);
    return parentPort;
  });
  defineData(parentPort, 'off', (event, listener) => {
    if (event === 'message') {
      for (let index = 0; index < listeners.length; index++) {
        if (listeners[index] === listener) defineData(listeners, index, undefined);
      }
    }
    return parentPort;
  });
  const workerThreadsModule = capturedObject.freeze({ parentPort });
  defineData(globalThis, '__seekdeep_worker_threads_module__', workerThreadsModule, false, false);
  const consoleShim = capturedObject.create(null);
  for (const level of ['log', 'info', 'warn', 'error', 'debug']) {
    capturedObject.defineProperty(consoleShim, level, {
      enumerable: true,
      value: (...args) => log(...args),
    });
  }
  const streamPrototype = capturedObject.create(null);
  const write = function(chunk, ...rest) {
    log(typeof chunk === 'string' ? chunk : String(chunk));
    const callback = rest.find(value => typeof value === 'function');
    if (callback) capturedPromise.resolve().then(() => callback(null));
    return true;
  };
  capturedObject.defineProperty(streamPrototype, 'write', { value: write });
  const makeStream = () => {
    const stream = capturedObject.create(streamPrototype);
    capturedObject.defineProperty(stream, 'write', { value: write, writable: true });
    return stream;
  };
  const processShim = capturedObject.create(null);
  capturedObject.defineProperties(processShim, {
    env: { enumerable: true, value: capturedObject.create(null) },
    stdout: { enumerable: true, value: makeStream() },
    stderr: { enumerable: true, value: makeStream() },
    exit: { enumerable: true, value: code => exit(code) },
  });
  const bufferShim = capturedObject.create(null);
  capturedObject.defineProperties(bufferShim, {
    byteLength: { writable: true, value: value => capturedString(value).length },
    alloc: { writable: true, value: size => new Uint8Array(size) },
    from: { writable: true, value: value => value },
  });
  let nextTimer = 1;
  const cancelledTimers = new Set();
  const setTimeoutShim = (callback, delay = 0, ...args) => {
    const id = nextTimer++;
    sleep(delay).then(() => {
      if (!cancelledTimers.delete(id)) callback(...args);
    });
    return id;
  };
  const clearTimeoutShim = id => { cancelledTimers.add(id); };
  capturedObject.defineProperties(globalThis, {
    console: { configurable: true, writable: true, value: consoleShim },
    process: { configurable: true, writable: true, value: processShim },
    Buffer: { configurable: true, writable: true, value: bufferShim },
    setTimeout: { configurable: true, writable: true, value: setTimeoutShim },
    clearTimeout: { configurable: true, writable: true, value: clearTimeoutShim },
    queueMicrotask: { configurable: true, writable: true, value: callback => capturedPromise.resolve().then(callback) },
  });
  delete globalThis.__seekdeep_log__;
  delete globalThis.__seekdeep_exit__;
  delete globalThis.__seekdeep_sleep__;
  delete globalThis.__seekdeep_port_control__;
  delete globalThis.__seekdeep_port_call__;
})();
";

const BINDING_SETUP_PREFIX: &str = r"
(() => {
  const call = globalThis.__seekdeep_call__;
  const capturedArray = Array;
  const capturedError = Error;
  const capturedObject = Object;
  const capturedString = String;
  const capturedCreate = capturedObject.create;
  const capturedDefine = capturedObject.defineProperty;
  const capturedObjectPrototype = capturedObject.prototype;
  const defineData = (target, key, value, enumerable = true) => {
    const descriptor = capturedCreate(null);
    descriptor.configurable = true;
    descriptor.enumerable = enumerable;
    descriptor.writable = true;
    descriptor.value = value;
    capturedDefine(target, key, descriptor);
  };
  const setLength = (target, value) => {
    const descriptor = capturedCreate(null);
    descriptor.value = value;
    capturedDefine(target, 'length', descriptor);
  };
  const append = (target, value) => { defineData(target, target.length, value); };
  const decode = wire => {
    const frames = [];
    let root;
    let rootSet = false;
    const attach = value => {
      const parent = frames.length === 0 ? undefined : frames[frames.length - 1];
      if (parent === undefined) {
        if (rootSet) throw new capturedError('invalid binding wire');
        root = value;
        rootSet = true;
        return;
      }
      if (parent.kind === 'array') defineData(parent.target, parent.index, value);
      else defineData(parent.target, parent.keys[parent.index], value);
      parent.index += 1;
    };
    for (let tokenIndex = 0; tokenIndex < wire.length; tokenIndex++) {
      const token = wire[tokenIndex];
      let value;
      let frame;
      if (token === null || typeof token === 'boolean' || typeof token === 'number' || typeof token === 'string') {
        value = token;
      } else if (token.kind === 'array') {
        value = [];
        if (token.length > 0) frame = { kind: 'array', target: value, length: token.length, index: 0 };
      } else {
        value = capturedCreate(capturedObjectPrototype);
        if (token.keys.length > 0) frame = { kind: 'object', target: value, keys: token.keys, index: 0 };
      }
      attach(value);
      if (frame !== undefined) append(frames, frame);
      while (frames.length > 0) {
        const current = frames[frames.length - 1];
        const length = current.kind === 'array' ? current.length : current.keys.length;
        if (current.index < length) break;
        setLength(frames, frames.length - 1);
      }
    }
    if (!rootSet || frames.length !== 0) throw new capturedError('invalid binding wire');
    return root;
  };
";

pub(crate) fn binding_setup(bindings: &[CodeBindingNamespace]) -> anyhow::Result<String> {
    let mut source = String::from(BINDING_SETUP_PREFIX);

    for (namespace_index, namespace) in bindings.iter().enumerate() {
        let global = serde_json::to_string(&namespace.global)?;
        let error_variable = if let Some(descriptor) = &namespace.error_class {
            let variable = format!("BindingError{namespace_index}");
            let class_name = &descriptor.name;
            let class_name_json = serde_json::to_string(class_name)?;
            let member_json = serde_json::to_string(&descriptor.member_name_property)?;
            write!(
                &mut source,
                "  const {variable} = class {class_name} extends capturedError {{\n    constructor(memberName, message) {{\n      super(message);\n      defineData(this, 'name', {class_name_json});\n      defineData(this, {member_json}, memberName);\n    }}\n  }};\n  defineData(globalThis, {class_name_json}, {variable}, false);\n"
            )?;
            Some(variable)
        } else {
            None
        };
        let namespace_variable = format!("namespace{namespace_index}");
        writeln!(
            &mut source,
            "  const {namespace_variable} = capturedCreate(null);"
        )?;
        for name in namespace.functions.keys() {
            let name_json = serde_json::to_string(name)?;
            let call_expression = format!("decode(await call({global}, {name_json}, args))");
            let body = error_variable.as_ref().map_or_else(
                || format!("return {call_expression};"),
                |error_variable| {
                    format!(
                        "try {{ return {call_expression}; }} catch (error) {{ const message = error !== null && typeof error === 'object' && typeof error.message === 'string' ? error.message : capturedString(error); throw new {error_variable}({name_json}, message); }}"
                    )
                },
            );
            writeln!(
                &mut source,
                "  defineData({namespace_variable}, {name_json}, async args => {{ {body} }});\n"
            )?;
        }
        writeln!(
            &mut source,
            "  defineData(globalThis, {global}, {namespace_variable}, false);"
        )?;
    }
    source.push_str("  delete globalThis.__seekdeep_call__;\n})();\n");
    Ok(source)
}
