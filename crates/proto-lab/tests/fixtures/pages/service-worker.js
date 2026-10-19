// Posts once it is active, then tells every window in its scope what happened.
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => {
  event.waitUntil((async () => {
    const outcome = await fetch('/sink/service-worker', { method: 'POST', body: 'lab' })
      .then((response) => 'ok ' + response.status, (error) => 'error ' + error.name);
    for (const client of await self.clients.matchAll({ includeUncontrolled: true, type: 'window' })) {
      client.postMessage(outcome);
    }
  })());
});
