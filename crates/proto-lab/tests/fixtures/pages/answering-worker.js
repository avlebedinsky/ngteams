// Answers /probe itself, and takes over every open window in its scope at once.
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()));
self.addEventListener('fetch', (event) => {
  if (new URL(event.request.url).pathname === '/probe') {
    event.respondWith(new Response('from-service-worker'));
  }
});
