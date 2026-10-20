// The lab's WebRTC capture hooks. When a run records (crates/proto-lab/src/capture.rs), arming
// (crates/proto-lab/src/arming.rs) installs them in every document of every page and iframe target before the
// document's own scripts run, as it does the WebSocket guard; capture.rs fills in the two names below.
//
// They report, through the CDP binding (`Runtime.addBinding`), what `chrome://webrtc-internals` would show and CDP
// can't export: for every `RTCPeerConnection` its configuration, every offer and answer it creates, the local and
// remote descriptions it applies (as applied, so an implicit `setLocalDescription()` reports the SDP it made), the ICE
// candidates it gathers and is given, its state changes, and its `getStats()` every interval until it closes.
//
// They only observe: every call goes to the original method with the same arguments and returns what it returned, and
// the page's promises are left as they are (a report hangs off a derived promise that swallows its own errors). A
// connection is tracked from its constructor (`RTCPeerConnection` is a proxy whose `construct` notes it) or from the
// first hooked method called on it (one made by another realm's constructor). The binding is taken off the global
// before any page code runs, and the built-ins used later are captured now. This is capture, not enforcement: page code
// that wanted to could still hide a connection from it.
(() => {
  'use strict';
  const BINDING = '{{BINDING}}';
  const STATS_INTERVAL_MS = {{STATS_INTERVAL_MS}};

  const scope = globalThis;
  const { apply, construct, defineProperty, deleteProperty, getOwnPropertyDescriptor } = Reflect;
  const report = scope[BINDING];
  deleteProperty(scope, BINDING);
  const Connection = scope.RTCPeerConnection;
  if (typeof Connection !== 'function') {
    return 'no RTCPeerConnection';
  }
  const proto = Connection.prototype;
  const MARK = Symbol.for('lab.capture.rtc');
  if (getOwnPropertyDescriptor(proto, MARK) !== undefined) {
    return 'hooked';
  }
  if (typeof report !== 'function') {
    return 'no binding';
  }
  defineProperty(proto, MARK, { value: true });

  const stringify = JSON.stringify;
  const now = Date.now;
  const toText = String;
  const then = Promise.prototype.then;
  const addListener = EventTarget.prototype.addEventListener;
  const setTimer = scope.setInterval;
  const clearTimer = scope.clearInterval;
  const originalStats = proto.getStats;
  const statsForEach = scope.RTCStatsReport.prototype.forEach;
  const ids = new WeakMap();
  const idOf = WeakMap.prototype.get;
  const setId = WeakMap.prototype.set;
  const getterOf = (name) => getOwnPropertyDescriptor(proto, name).get;
  const localOf = getterOf('localDescription');
  const remoteOf = getterOf('remoteDescription');
  const signalingOf = getterOf('signalingState');
  const STATES = {
    __proto__: null,
    connectionstatechange: getterOf('connectionState'),
    iceconnectionstatechange: getterOf('iceConnectionState'),
    icegatheringstatechange: getterOf('iceGatheringState'),
    signalingstatechange: signalingOf,
  };
  const STATE_EVENTS = ['connectionstatechange', 'iceconnectionstatechange', 'icegatheringstatechange', 'signalingstatechange'];
  let connections = 0;

  const send = (id, event, data) => {
    let text;
    try {
      text = stringify({ __proto__: null, pc: id, event, ts: now(), data });
    } catch (error) {
      text = stringify({ __proto__: null, pc: id, event, ts: now(), error: toText(error) });
    }
    try {
      report(text);
    } catch (_) {
      // nothing to report to any more
    }
  };
  const description = (value) => (value ? { __proto__: null, type: value.type, sdp: value.sdp } : null);
  const candidate = (value) =>
    value
      ? {
          __proto__: null,
          candidate: value.candidate,
          sdpMid: value.sdpMid,
          sdpMLineIndex: value.sdpMLineIndex,
          usernameFragment: value.usernameFragment,
        }
      : null;

  const takeStats = (connection, id, timer) => {
    if (apply(signalingOf, connection, []) === 'closed') {
      apply(clearTimer, scope, [timer]);
      return;
    }
    let pending;
    try {
      pending = apply(originalStats, connection, []);
    } catch (_) {
      return;
    }
    const list = [];
    const collect = (entry) => {
      list[list.length] = entry;
    };
    apply(then, pending, [
      (statsReport) => {
        apply(statsForEach, statsReport, [collect]);
        send(id, 'stats', list);
      },
      () => {},
    ]);
  };

  // the connection's number, noting it (and starting its listeners and stats) the first time it is seen
  const track = (connection, configuration) => {
    const known = apply(idOf, ids, [connection]);
    if (known !== undefined) {
      return known;
    }
    // only a real connection takes the listeners; anything else (the prototype) throws here, before it is noted
    apply(addListener, connection, ['icecandidate', (event) => send(id, 'icecandidate', candidate(event.candidate))]);
    connections += 1;
    const id = connections;
    apply(setId, ids, [connection, id]);
    send(id, 'create', configuration === undefined ? null : configuration);
    for (let index = 0; index < STATE_EVENTS.length; index += 1) {
      const name = STATE_EVENTS[index];
      const state = STATES[name];
      apply(addListener, connection, [name, () => send(id, name, apply(state, connection, []))]);
    }
    const timer = apply(setTimer, scope, [() => takeStats(connection, id, timer), STATS_INTERVAL_MS]);
    return id;
  };

  const hook = (name, before, after) => {
    const original = proto[name];
    if (typeof original !== 'function') {
      return;
    }
    const hooked = {
      [name](...args) {
        // a call on something that isn't a live connection (the prototype, a primitive) goes through untouched, and
        // so does the page's call whatever the hooks run into
        let id;
        try {
          id = this !== null && typeof this === 'object' ? track(this, undefined) : undefined;
          if (id !== undefined && before) {
            before(id, args);
          }
        } catch (_) {
          id = undefined;
        }
        const result = apply(original, this, args);
        if (id !== undefined && after && result !== null && typeof result === 'object') {
          try {
            apply(then, result, [
              (value) => after(this, id, value),
              (error) => send(id, name + ':error', toText(error && error.message)),
            ]);
          } catch (_) {
            // not a promise
          }
        }
        return result;
      },
    }[name];
    defineProperty(proto, name, { value: hooked, writable: true, enumerable: true, configurable: true });
  };

  hook('createOffer', null, (_, id, offer) => send(id, 'createOffer', description(offer)));
  hook('createAnswer', null, (_, id, answer) => send(id, 'createAnswer', description(answer)));
  hook('setLocalDescription', null, (connection, id) =>
    send(id, 'setLocalDescription', description(apply(localOf, connection, []))),
  );
  hook('setRemoteDescription', null, (connection, id) =>
    send(id, 'setRemoteDescription', description(apply(remoteOf, connection, []))),
  );
  hook('addIceCandidate', (id, args) => send(id, 'addIceCandidate', candidate(args[0])), null);
  hook('close', (id) => send(id, 'close', null), null);

  const handler = { __proto__: null };
  handler.construct = (target, args, newTarget) => {
    const connection = construct(target, args, newTarget);
    try {
      track(connection, args[0]);
    } catch (_) {
      // the page gets its connection whatever the hooks run into
    }
    return connection;
  };
  const Hooked = new Proxy(Connection, handler);
  const replace = (owner, name) => {
    const descriptor = getOwnPropertyDescriptor(owner, name);
    if (descriptor !== undefined && descriptor.value === Connection) {
      descriptor.value = Hooked;
      defineProperty(owner, name, descriptor);
    }
  };
  replace(scope, 'RTCPeerConnection');
  replace(scope, 'webkitRTCPeerConnection');
  replace(proto, 'constructor');
  return 'hooked';
})();
