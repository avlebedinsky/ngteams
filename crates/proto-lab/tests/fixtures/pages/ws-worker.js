importScripts('/pages/ws-scenario.js');

runScenario(socketUrl(new URLSearchParams(location.search).get('ws'), 'dedicated-worker')).then(
  (outcome) => postMessage(outcome),
  (error) => postMessage('error ' + error),
);
