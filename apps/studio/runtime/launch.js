const status = document.getElementById('status');
const run = document.getElementById('run'), stop = document.getElementById('stop');
let worker, watchdog;
function finish() { clearTimeout(watchdog); worker?.terminate(); worker = null; run.disabled = false; stop.disabled = true; }
run.onclick = () => {
  finish(); run.disabled = true; stop.disabled = false; status.textContent = 'Loading guest…';
  worker = new Worker('./guest-worker.js', {type:'module'});
  worker.onmessage = ({data}) => {
    status.textContent += `\n${data.message}`;
    if (data.type === 'done' || data.type === 'error') finish();
  };
  worker.onerror = event => { status.textContent += `\nWorker failed: ${event.message}`; finish(); };
  worker.postMessage({manifest:'./manifest.json'});
  watchdog = setTimeout(() => { status.textContent += '\nStopped after 30 seconds. The diagnostic runner does not implement a guest event scheduler.'; finish(); }, 30000);
};
stop.onclick = () => { finish(); status.textContent += '\nStopped by user.'; };
addEventListener('pagehide', finish);
