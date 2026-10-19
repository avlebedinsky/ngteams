onconnect = (event) => {
  const port = event.ports[0];
  fetch('/sink/shared-worker', { method: 'POST', body: 'lab' })
    .then((response) => 'ok ' + response.status, (error) => 'error ' + error.name)
    .then((outcome) => port.postMessage(outcome));
};
