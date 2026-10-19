// The browser fence's WebSocket guard. Arming (crates/proto-lab/src/arming.rs) installs it in every JavaScript
// context of every lab target before the target's own code runs; crates/proto-lab/src/fence_ws.rs fills in the three
// names below and decides every frame.
//
// `WebSocket.prototype.send` is replaced. Nothing is sent synchronously: each frame is copied (so the page can't change
// it after the decision) and queued on its socket, and the fence is asked for a verdict through the CDP binding
// (`Runtime.addBinding`). The fence answers by calling the resolver with the run's token. A frame is replayed through
// the original `send` only on an explicit allow; a denied frame is dropped (the fence logs it and fails the run). A
// socket's frames leave in the order they were sent: a frame waits until every earlier frame of its socket is decided.
//
// Hardening, against code that would use the guard to send what the fence didn't allow:
// - the binding is taken off the global before any page code runs, so only the guard can ask for a verdict;
// - the resolver is a non-writable, non-configurable global that does nothing without the token, which only this
//   closure holds (a function's source shows the name TOKEN, never its value);
// - every built-in the guard calls later is captured here, before page code can replace it; the guard's own records
//   have no prototype, and its code past this point uses no array iteration, destructuring or spread, so nothing the
//   page defines on Object.prototype or Array.prototype is consulted;
// - `WebSocketStream` (which writes without `send`) is removed.
// Anything that still gets a frame out (a context the guard isn't in) is caught afterwards by the audit of
// `Network.webSocketFrameSent`, which fails the run.
//
// Differences from the plain `send`: a frame doesn't count towards `bufferedAmount` while it waits for its verdict, and
// a `close()` while frames wait closes the socket at once (the waiting frames are then dropped, not sent).
(() => {
  'use strict';
  const BINDING = '{{BINDING}}';
  const RESOLVER = '{{RESOLVER}}';
  const TOKEN = '{{TOKEN}}';

  const scope = globalThis;
  const { apply, defineProperty, deleteProperty, getOwnPropertyDescriptor, getPrototypeOf } = Reflect;
  const binding = scope[BINDING];
  // taken off whether or not the guard is in already: a binding added again to a running context (by another session
  // attached to it) must not stay within reach of its code either
  deleteProperty(scope, BINDING);
  if (getOwnPropertyDescriptor(scope, RESOLVER) !== undefined) {
    return 'armed';
  }
  deleteProperty(scope, 'WebSocketStream');

  const Socket = scope.WebSocket;
  if (typeof Socket !== 'function') {
    // no sockets in this scope (a worklet)
    return 'armed';
  }
  const proto = Socket.prototype;
  if (typeof binding !== 'function') {
    // fail closed: no way to ask the fence, so nothing is sent
    defineProperty(proto, 'send', {
      value: function send() {
        throw new Error('the lab fence has no verdict channel in this scope; nothing is sent');
      },
      writable: true,
      enumerable: true,
      configurable: true,
    });
    return 'no binding';
  }

  const getter = (owner, name) => getOwnPropertyDescriptor(owner, name).get;
  const originalSend = proto.send;
  const readyStateOf = getter(proto, 'readyState');
  const urlOf = getter(proto, 'url');
  const stringify = JSON.stringify;
  const wellFormed = String.prototype.toWellFormed;
  const Bytes = Uint8Array;
  const TypedArray = getPrototypeOf(Bytes.prototype);
  const partsOf = (owner) => ({
    __proto__: null,
    buffer: getter(owner, 'buffer'),
    offset: getter(owner, 'byteOffset'),
    length: getter(owner, 'byteLength'),
  });
  const typedParts = partsOf(TypedArray);
  const dataViewParts = partsOf(DataView.prototype);
  const isView = ArrayBuffer.isView;
  const bufferLength = getter(ArrayBuffer.prototype, 'byteLength');
  const sliceBuffer = ArrayBuffer.prototype.slice;
  const toBase64 = Bytes.prototype.toBase64;
  const Blob_ = scope.Blob;
  const blobSize = Blob_ === undefined ? undefined : getter(Blob_.prototype, 'size');
  const blobBytes = Blob_ === undefined ? undefined : Blob_.prototype.arrayBuffer;
  const then = Promise.prototype.then;
  const Exception = scope.DOMException;
  const states = new WeakMap();
  const getState = WeakMap.prototype.get;
  const setState = WeakMap.prototype.set;
  // socket id -> its state, while it has frames waiting
  const waiting = { __proto__: null };
  let lastSocket = 0;

  // Whether `value` passes a built-in getter's brand check.
  const branded = (get, value) => {
    try {
      apply(get, value, []);
      return true;
    } catch {
      return false;
    }
  };

  // A copy of an ArrayBuffer or a view's bytes; throws a TypeError for a shared buffer, as `send` does.
  const copyBytes = (data) => {
    if (!isView(data)) {
      return apply(sliceBuffer, data, [0, apply(bufferLength, data, [])]);
    }
    const parts = branded(typedParts.buffer, data) ? typedParts : dataViewParts;
    const start = apply(parts.offset, data, []);
    return apply(sliceBuffer, apply(parts.buffer, data, []), [start, start + apply(parts.length, data, [])]);
  };

  const base64 = (buffer) => apply(toBase64, new Bytes(buffer), []);

  // What `send(data)` would send: text, bytes, or a Blob to read first.
  const frameOf = (data) => {
    if (isView(data) || branded(bufferLength, data)) {
      const bytes = copyBytes(data);
      return { __proto__: null, kind: 'binary', data: bytes, payload: base64(bytes) };
    }
    if (blobSize !== undefined && branded(blobSize, data)) {
      return { __proto__: null, kind: 'blob', data };
    }
    // a template literal converts as `send` does (a symbol throws)
    const text = apply(wellFormed, `${data}`, []);
    return { __proto__: null, kind: 'text', data: text, payload: text };
  };

  const stateOf = (socket) => {
    let state = apply(getState, states, [socket]);
    if (state === undefined) {
      lastSocket += 1;
      state = { __proto__: null, id: lastSocket, socket, seq: 0, head: null, tail: null };
      apply(setState, states, [socket, state]);
    }
    return state;
  };

  // Sends every decided frame at the head of the queue, in order; stops at the first one still waiting.
  const flush = (state) => {
    while (state.head !== null && state.head.verdict !== undefined) {
      const frame = state.head;
      state.head = frame.next;
      if (state.head === null) {
        state.tail = null;
      }
      if (frame.verdict === true) {
        try {
          apply(originalSend, state.socket, [frame.data]);
        } catch {
          // the socket went away meanwhile; the plain send drops a frame on a closing socket too
        }
      }
    }
    if (state.head === null) {
      deleteProperty(waiting, state.id);
    }
  };

  const ask = (state, frame, kind, payload) => {
    const url = apply(urlOf, state.socket, []);
    binding(stringify({ __proto__: null, socket: state.id, seq: frame.seq, url, kind, data: payload }));
  };

  const resolve = (token, socket, seq, allow) => {
    if (token !== TOKEN) {
      return false;
    }
    const state = waiting[socket];
    if (state === undefined) {
      return false;
    }
    for (let frame = state.head; frame !== null; frame = frame.next) {
      if (frame.seq === seq) {
        if (frame.verdict === undefined) {
          frame.verdict = allow === true;
        }
        break;
      }
    }
    flush(state);
    return true;
  };

  function send(data) {
    if (apply(readyStateOf, this, []) === 0) {
      throw new Exception("Failed to execute 'send' on 'WebSocket': Still in CONNECTING state.", 'InvalidStateError');
    }
    const sent = frameOf(data);
    const state = stateOf(this);
    state.seq += 1;
    const frame = { __proto__: null, seq: state.seq, data: sent.data, verdict: undefined, next: null };
    if (state.tail === null) {
      state.head = frame;
      waiting[state.id] = state;
    } else {
      state.tail.next = frame;
    }
    state.tail = frame;
    if (sent.kind !== 'blob') {
      ask(state, frame, sent.kind, sent.payload);
      return;
    }
    const read = (bytes) => {
      frame.data = bytes;
      ask(state, frame, 'binary', base64(bytes));
    };
    const unreadable = () => {
      // asked anyway, so the fence logs it; the frame is dropped whatever it answers
      frame.verdict = false;
      ask(state, frame, 'unreadable', '');
      flush(state);
    };
    apply(then, apply(blobBytes, sent.data, []), [read, unreadable]);
  }

  defineProperty(scope, RESOLVER, { value: resolve, writable: false, enumerable: false, configurable: false });
  defineProperty(proto, 'send', { value: send, writable: true, enumerable: true, configurable: true });
  return 'armed';
})();
