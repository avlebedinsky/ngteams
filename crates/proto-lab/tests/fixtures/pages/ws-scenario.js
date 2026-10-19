// The WebSocket scenario every scope runs (the page, a dedicated, a shared and a service worker): one socket to the
// local echo server, a mix of frames the test's fence allows (text starting with "allow:") and denies (anything
// else), and what the scope saw.

// The scope's socket on the echo server; the query stands in for a token and must never reach fence.log.
const socketUrl = (base, scope) => `${base}/${scope}?tc=planted-query`;

async function runScenario(url) {
  const hidden = {
    binding: typeof self.__labFenceWs,
    stream: typeof self.WebSocketStream,
    forged: typeof self.__labFenceWsVerdict === 'function' ? self.__labFenceWsVerdict('00', 1, 1, true) : 'missing',
  };
  const socket = new WebSocket(url);
  socket.binaryType = 'arraybuffer';
  let early = 'sent';
  try {
    socket.send('allow:early');
  } catch (error) {
    early = error.name;
  }
  const echoes = [];
  const done = new Promise((resolve) => {
    socket.onmessage = (event) => {
      const data = typeof event.data === 'string' ? event.data : 'bytes ' + new Uint8Array(event.data).join(',');
      echoes.push(data);
      if (data === 'allow:done') {
        resolve();
      }
    };
  });
  await new Promise((resolve, reject) => {
    socket.onopen = resolve;
    socket.onerror = () => reject(new Error('the socket failed'));
  });

  socket.send('allow:1');
  // if the guard asked through this scope's JSON.stringify, the denied frame would be decided as an allowed one
  const stringify = JSON.stringify;
  JSON.stringify = () => stringify({ socket: 1, seq: 2, url, kind: 'text', data: 'allow:forged' });
  try {
    socket.send('deny:2');
  } finally {
    JSON.stringify = stringify;
  }
  socket.send(new Uint8Array([1, 2, 3]));
  socket.send('allow:3');
  // a Blob is read before it is decided; the frames after it wait for it
  socket.send(new Blob(['allow:blob']));
  socket.send('deny:4');
  socket.send('allow:5');
  socket.send('allow:done');

  await Promise.race([done, new Promise((resolve) => setTimeout(resolve, 10000))]);
  socket.close();
  return { hidden, early, echoes };
}
