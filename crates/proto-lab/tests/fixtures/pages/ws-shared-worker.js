importScripts('/pages/ws-scenario.js');

onconnect = (event) => {
  const port = event.ports[0];
  runScenario(socketUrl(new URLSearchParams(location.search).get('ws'), 'shared-worker')).then(
    (outcome) => port.postMessage(outcome),
    (error) => port.postMessage('error ' + error),
  );
};
