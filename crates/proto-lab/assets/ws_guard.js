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
// A guard queues the frames of its own realm's sockets only. Blink's `send` takes a socket of any realm, so a page can
// call one window's `send` on another window's socket (a same-origin iframe's, a popup's or its opener's); two guards
// would then keep two queues for one socket, and the frame would be asked for in a context (and, for a popup, a CDP
// session) that isn't the socket's. So the `WebSocket` constructor is wrapped to note every socket made in this realm,
// and a `send` on any other socket is refused: the frame is dropped and reported to the fence, which denies it (and
// fails the run) without classifying it. Teams sends on each socket from the realm that made it.
//
// Hardening, against code that would use the guard to send what the fence didn't allow:
// - the binding is taken off the global before any page code runs, so only the guard can ask for a verdict;
// - the resolver is a non-writable, non-configurable global that does nothing without the token, which only this
//   closure holds (a function's source shows the name TOKEN, never its value);
// - every request carries the token too, and the fence answers none without it: the binding is also in isolated worlds
//   the guard isn't in, and an answer there would hand the token to whatever that world called the resolver;
// - every built-in the guard calls later is captured here, before page code can replace it; the guard's own records
//   have no prototype, and its code past this point uses no array iteration, destructuring or spread, so nothing the
//   page defines on Object.prototype or Array.prototype is consulted;
// - `WebSocketStream` (which writes without `send`) is removed;
// - `WebSocket` is a proxy whose `construct` notes the socket and does nothing else; the original constructor isn't
//   reachable from it (`WebSocket.prototype.constructor` is the proxy too), and a socket made without it is foreign.
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

  const { construct } = Reflect;
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
  const setBytes = TypedArray.set;
  const toBase64 = Bytes.prototype.toBase64;
  const Blob_ = scope.Blob;
  const blobSize = Blob_ === undefined ? undefined : getter(Blob_.prototype, 'size');
  const blobBytes = Blob_ === undefined ? undefined : Blob_.prototype.arrayBuffer;
  const then = Promise.prototype.then;
  const Exception = scope.DOMException;
  const own = new WeakSet();
  const isOwn = WeakSet.prototype.has;
  const noteOwn = WeakSet.prototype.add;
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

  // `length` bytes of `buffer` from `offset`, copied into a buffer the guard allocates itself and never hands out.
  // `ArrayBuffer.prototype.slice` would build its result through the buffer's `constructor[Symbol.species]`, which the
  // page controls, and so could keep a reference to the copy and change it after the decision. The byte-length getter
  // throws a TypeError for a shared buffer, as `send` does.
  const ownCopy = (buffer, offset, length) => {
    apply(bufferLength, buffer, []);
    const copy = new Bytes(length);
    apply(setBytes, copy, [new Bytes(buffer, offset, length)]);
    return apply(typedParts.buffer, copy, []);
  };

  // A copy of an ArrayBuffer's or a view's bytes.
  const copyBytes = (data) => {
    if (!isView(data)) {
      return ownCopy(data, 0, apply(bufferLength, data, []));
    }
    const parts = branded(typedParts.buffer, data) ? typedParts : dataViewParts;
    return ownCopy(apply(parts.buffer, data, []), apply(parts.offset, data, []), apply(parts.length, data, []));
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
    binding(stringify({ __proto__: null, token: TOKEN, socket: state.id, seq: frame.seq, url, kind, data: payload }));
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
    if (!apply(isOwn, own, [this])) {
      // another realm's socket: dropped like a denied frame, and reported (nothing waits for an answer)
      const url = apply(urlOf, this, []);
      binding(stringify({ __proto__: null, token: TOKEN, socket: 0, seq: 0, url, kind: 'foreign', data: '' }));
      return;
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
    const unreadable = () => {
      // asked anyway, so the fence logs it; the frame is dropped whatever it answers
      frame.verdict = false;
      ask(state, frame, 'unreadable', '');
      flush(state);
    };
    // The promise settles with whatever a `then` the page defines on ArrayBuffer.prototype or Object.prototype hands
    // it, so the value is checked to be a buffer and copied before it is decided: the page may keep what it handed in.
    // Anything that fails here makes the frame unreadable; a throw would leave it, and its socket, waiting forever.
    const read = (value) => {
      let bytes;
      try {
        bytes = ownCopy(value, 0, apply(bufferLength, value, []));
      } catch {
        unreadable();
        return;
      }
      frame.data = bytes;
      ask(state, frame, 'binary', base64(bytes));
    };
    apply(then, apply(blobBytes, sent.data, []), [read, unreadable]);
  }

  // Only what `construct` does differs from the original: the proxy forwards its name, length, statics and prototype.
  // The handler has no prototype, so no trap is looked up on Object.prototype.
  const Guarded = new Proxy(Socket, {
    __proto__: null,
    construct(target, args, newTarget) {
      const socket = construct(target, args, newTarget);
      apply(noteOwn, own, [socket]);
      return socket;
    },
  });
  // the same attributes as the originals, with the proxy as the value
  const replace = (owner, name, value) => {
    const slot = getOwnPropertyDescriptor(owner, name);
    defineProperty(owner, name, {
      __proto__: null,
      value,
      writable: slot.writable,
      enumerable: slot.enumerable,
      configurable: slot.configurable,
    });
  };

  defineProperty(scope, RESOLVER, { value: resolve, writable: false, enumerable: false, configurable: false });
  defineProperty(proto, 'send', { value: send, writable: true, enumerable: true, configurable: true });
  replace(proto, 'constructor', Guarded);
  replace(scope, 'WebSocket', Guarded);
  return 'armed';
})();
