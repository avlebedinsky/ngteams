fetch('/sink/dedicated-worker', { method: 'POST', body: 'lab' })
  .then((response) => 'ok ' + response.status, (error) => 'error ' + error.name)
  .then((outcome) => postMessage(outcome));
