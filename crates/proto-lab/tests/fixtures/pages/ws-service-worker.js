importScripts('/pages/ws-scenario.js');

self.onmessage = (event) => {
  const scenario = runScenario(socketUrl(new URLSearchParams(location.search).get('ws'), 'service-worker')).then(
    (outcome) => event.source.postMessage(outcome),
    (error) => event.source.postMessage('error ' + error),
  );
  event.waitUntil(scenario);
};
