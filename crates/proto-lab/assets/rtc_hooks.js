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
//
// The audio tap: every received audio track (a connection's `track` event) is read in the page through WebAudio, never
// from a speaker: a context of its own, the track as a source, an analyser that is connected to nothing else (an
// analyser is pulled by itself, so the context's output stays silent). Chrome decodes a received track only while
// something plays it out, so the tap also gives it a muted audio element of its own. Every TAP_INTERVAL_MS the
// analyser's newest TAP_WINDOW samples are measured and reported as `audio`: their RMS level and, by a Goertzel filter
// at TONE_HZ, the share of their power that is that tone (about 1 for the pure tone, near 0 for speech or silence),
// until the track ends or its connection closes. The lab browser runs with its output muted and no audio device
// (crates/proto-lab/src/browser.rs).
(() => {
  'use strict';
  const BINDING = '{{BINDING}}';
  const STATS_INTERVAL_MS = {{STATS_INTERVAL_MS}};
  const TAP_INTERVAL_MS = {{TAP_INTERVAL_MS}};
  const TAP_WINDOW = {{TAP_WINDOW}};
  const TONE_HZ = {{TONE_HZ}};

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

  // the audio tap's built-ins, where the scope has WebAudio (a page or an iframe; not a worklet)
  const AudioContextOf = scope.AudioContext;
  const StreamOf = scope.MediaStream;
  const Samples = scope.Float32Array;
  const { cos, sqrt, PI } = Math;
  const methodOf = (owner, name) => (owner && typeof owner.prototype[name] === 'function' ? owner.prototype[name] : undefined);
  const getterIn = (owner, name) => {
    const descriptor = owner ? getOwnPropertyDescriptor(owner.prototype, name) : undefined;
    return descriptor ? descriptor.get : undefined;
  };
  const setterIn = (owner, name) => {
    const descriptor = owner ? getOwnPropertyDescriptor(owner.prototype, name) : undefined;
    return descriptor ? descriptor.set : undefined;
  };
  const audio = {
    __proto__: null,
    createSource: methodOf(AudioContextOf, 'createMediaStreamSource'),
    createAnalyser: methodOf(scope.BaseAudioContext, 'createAnalyser'),
    resume: methodOf(AudioContextOf, 'resume'),
    close: methodOf(AudioContextOf, 'close'),
    contextState: getterIn(scope.BaseAudioContext, 'state'),
    sampleRate: getterIn(scope.BaseAudioContext, 'sampleRate'),
    connect: methodOf(scope.AudioNode, 'connect'),
    setWindow: setterIn(scope.AnalyserNode, 'fftSize'),
    read: methodOf(scope.AnalyserNode, 'getFloatTimeDomainData'),
    kind: getterIn(scope.MediaStreamTrack, 'kind'),
    trackId: getterIn(scope.MediaStreamTrack, 'id'),
    readyState: getterIn(scope.MediaStreamTrack, 'readyState'),
    trackOf: getterIn(scope.RTCTrackEvent, 'track'),
    setMuted: setterIn(scope.HTMLMediaElement, 'muted'),
    setSource: setterIn(scope.HTMLMediaElement, 'srcObject'),
    play: methodOf(scope.HTMLMediaElement, 'play'),
  };
  const ElementOf = scope.Audio;
  const tapsAudio = typeof AudioContextOf === 'function' && typeof StreamOf === 'function' &&
    typeof ElementOf === 'function' &&
    Object.values(audio).every((value) => typeof value === 'function');
  const tapped = new WeakSet();
  const isTapped = WeakSet.prototype.has;
  const markTapped = WeakSet.prototype.add;

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

  // The RMS level of `samples` and the share of their power at TONE_HZ (a Goertzel filter at that frequency): for a
  // tone of amplitude A over N samples the filter's power is about (A N / 2)^2 and the samples' energy A^2 N / 2, so
  // 2 * power / (N * energy) is about 1, and it falls towards 0 as less of the signal is the tone.
  const measure = (samples, rate) => {
    const coefficient = 2 * cos((2 * PI * TONE_HZ) / rate);
    let previous = 0;
    let beforePrevious = 0;
    let energy = 0;
    for (let index = 0; index < samples.length; index += 1) {
      const sample = samples[index];
      const current = sample + coefficient * previous - beforePrevious;
      beforePrevious = previous;
      previous = current;
      energy += sample * sample;
    }
    const power = previous * previous + beforePrevious * beforePrevious - coefficient * previous * beforePrevious;
    const count = samples.length;
    return {
      __proto__: null,
      rms: count > 0 ? sqrt(energy / count) : 0,
      tone: energy > 0 ? (2 * power) / (count * energy) : 0,
    };
  };

  // reads a received audio track through WebAudio (see the top), once per track, until it ends
  const tap = (connection, id, track) => {
    if (!tapsAudio || track === null || typeof track !== 'object' || apply(audio.kind, track, []) !== 'audio') {
      return;
    }
    if (apply(isTapped, tapped, [track])) {
      return;
    }
    apply(markTapped, tapped, [track]);
    const name = apply(audio.trackId, track, []);
    let context;
    let analyser;
    let element;
    try {
      const stream = construct(StreamOf, [[track]]);
      element = construct(ElementOf, []);
      apply(audio.setMuted, element, [true]);
      apply(audio.setSource, element, [stream]);
      apply(then, apply(audio.play, element, []), [() => {}, () => {}]);
      context = construct(AudioContextOf, []);
      const source = apply(audio.createSource, context, [stream]);
      analyser = apply(audio.createAnalyser, context, []);
      apply(audio.setWindow, analyser, [TAP_WINDOW]);
      apply(audio.connect, source, [analyser]);
    } catch (error) {
      send(id, 'audio:error', toText(error && error.message));
      return;
    }
    const samples = new Samples(TAP_WINDOW);
    const rate = apply(audio.sampleRate, context, []);
    const timer = apply(setTimer, scope, [
      () => {
        const state = apply(audio.contextState, context, []);
        const ended =
          apply(audio.readyState, track, []) === 'ended' || apply(signalingOf, connection, []) === 'closed';
        if (ended || state === 'closed') {
          apply(clearTimer, scope, [timer]);
          apply(audio.setSource, element, [null]);
          if (state !== 'closed') {
            apply(then, apply(audio.close, context, []), [() => {}, () => {}]);
          }
          send(id, 'audio:ended', { __proto__: null, track: name });
          return;
        }
        if (state === 'suspended') {
          apply(then, apply(audio.resume, context, []), [() => {}, () => {}]);
        }
        apply(audio.read, analyser, [samples]);
        const measured = measure(samples, rate);
        send(id, 'audio', { __proto__: null, track: name, state, rate, rms: measured.rms, tone: measured.tone });
      },
      TAP_INTERVAL_MS,
    ]);
  };

  // The data channels: every channel a connection makes or is given (`datachannel`), the messages it sends and
  // receives, each as `dc:send` / `dc:message` with the channel's label and id and its data: text as it is, binary as
  // hex (at most DC_MAX_BYTES of it, with its whole length). A message received as a Blob is reported by its size.
  const Channel = scope.RTCDataChannel;
  const channelProto = Channel ? Channel.prototype : undefined;
  const channelGetter = (name) => {
    const descriptor = channelProto ? getOwnPropertyDescriptor(channelProto, name) : undefined;
    return descriptor ? descriptor.get : undefined;
  };
  const channelLabel = channelGetter('label');
  const channelId = channelGetter('id');
  const eventData = getterIn(scope.MessageEvent, 'data');
  const channelOf = getterIn(scope.RTCDataChannelEvent, 'channel');
  const Bytes = scope.Uint8Array;
  const ArrayBufferOf = scope.ArrayBuffer;
  const isView = ArrayBufferOf ? ArrayBufferOf.isView : undefined;
  const DC_MAX_BYTES = 4096;
  const hexDigits = '0123456789abcdef';
  const channelPc = new WeakMap();
  const watched = new WeakSet();
  const isWatched = WeakSet.prototype.has;
  const markWatched = WeakSet.prototype.add;
  const payload = (data) => {
    if (typeof data === 'string') {
      return { __proto__: null, text: data };
    }
    let bytes;
    if (ArrayBufferOf && data instanceof ArrayBufferOf) {
      bytes = new Bytes(data);
    } else if (isView && isView(data)) {
      bytes = new Bytes(data.buffer, data.byteOffset, data.byteLength);
    } else {
      return { __proto__: null, other: toText(data && data.size !== undefined ? 'blob ' + data.size : typeof data) };
    }
    const length = bytes.length;
    const shown = length < DC_MAX_BYTES ? length : DC_MAX_BYTES;
    let hex = '';
    for (let index = 0; index < shown; index += 1) {
      const byte = bytes[index];
      hex += hexDigits[byte >> 4] + hexDigits[byte & 15];
    }
    return { __proto__: null, length, hex };
  };
  const channelInfo = (channel) => ({
    __proto__: null,
    label: channelLabel ? apply(channelLabel, channel, []) : null,
    id: channelId ? apply(channelId, channel, []) : null,
  });
  const watchChannel = (id, channel) => {
    if (!channel || typeof channel !== 'object' || apply(isWatched, watched, [channel])) {
      return;
    }
    apply(markWatched, watched, [channel]);
    apply(setId, channelPc, [channel, id]);
    apply(addListener, channel, [
      'open',
      () => send(id, 'dc:open', channelInfo(channel)),
    ]);
    apply(addListener, channel, [
      'message',
      (event) => {
        try {
          const info = channelInfo(channel);
          info.data = payload(eventData ? apply(eventData, event, []) : undefined);
          send(id, 'dc:message', info);
        } catch (_) {
          // the page gets its message whatever the hooks run into
        }
      },
    ]);
  };
  if (channelProto && typeof channelProto.send === 'function') {
    const originalSend = channelProto.send;
    const hookedSend = {
      send(...args) {
        try {
          const id = apply(idOf, channelPc, [this]);
          if (id !== undefined) {
            const info = channelInfo(this);
            info.data = payload(args[0]);
            send(id, 'dc:send', info);
          }
        } catch (_) {
          // the page's send goes through whatever the hooks run into
        }
        return apply(originalSend, this, args);
      },
    }.send;
    defineProperty(channelProto, 'send', { value: hookedSend, writable: true, enumerable: true, configurable: true });
  }

  // the connection's number, noting it (and starting its listeners and stats) the first time it is seen
  const track = (connection, configuration) => {
    const known = apply(idOf, ids, [connection]);
    if (known !== undefined) {
      return known;
    }
    // only a real connection takes the listeners; anything else (the prototype) throws here, before it is noted
    apply(addListener, connection, ['icecandidate', (event) => send(id, 'icecandidate', candidate(event.candidate))]);
    apply(addListener, connection, [
      'datachannel',
      (event) => {
        try {
          const channel = channelOf ? apply(channelOf, event, []) : null;
          send(id, 'datachannel', channel ? channelInfo(channel) : null);
          watchChannel(id, channel);
        } catch (_) {
          // the page gets its channel whatever the hooks run into
        }
      },
    ]);
    apply(addListener, connection, [
      'track',
      (event) => {
        try {
          tap(connection, id, tapsAudio ? apply(audio.trackOf, event, []) : null);
        } catch (_) {
          // the page gets its track whatever the tap runs into
        }
      },
    ]);
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
  {
    const originalCreate = proto.createDataChannel;
    if (typeof originalCreate === 'function') {
      const hookedCreate = {
        createDataChannel(...args) {
          const channel = apply(originalCreate, this, args);
          try {
            const id = this !== null && typeof this === 'object' ? track(this, undefined) : undefined;
            if (id !== undefined) {
              send(id, 'createDataChannel', channelInfo(channel));
              watchChannel(id, channel);
            }
          } catch (_) {
            // the page gets its channel whatever the hooks run into
          }
          return channel;
        },
      }.createDataChannel;
      defineProperty(proto, 'createDataChannel', {
        value: hookedCreate,
        writable: true,
        enumerable: true,
        configurable: true,
      });
    }
  }

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
