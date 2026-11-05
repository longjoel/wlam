const $ = id => document.getElementById(id);
let current = null, config = {}, projects = [], polling = false;
const busy = p => ['queued', 'analyzing', 'compiling'].includes(p.state);
const el = (tag, text, cls) => { const n = document.createElement(tag); n.textContent = text; if (cls) n.className = cls; return n; };
async function api(path, options) {
  const response = await fetch(path, options);
  const data = await response.json();
  if (!response.ok) throw Error(data.error || 'Request failed');
  return data;
}
function notice(text, error = false) { $('notice').textContent = text; $('notice').className = error ? 'error' : ''; }
function playerURL(p) { const url = new URL(location.href); url.port = config.player_port; url.pathname = `/${p.id}/index.html`; url.search = ''; url.hash = ''; return url.href; }
function stopPlayer() { $('player').src = 'about:blank'; $('player-panel').hidden = true; }
async function refresh() {
  projects = await api('/api/projects');
  $('projects').replaceChildren(...projects.map(p => {
    const button = el('button', p.name, current?.id === p.id ? 'active' : '');
    button.append(el('small', p.state));
    button.onclick = () => { stopPlayer(); render(p); refresh().catch(report); };
    return button;
  }));
}
function render(p) {
  current = p;
  $('breadcrumb').textContent = p.name;
  $('import-panel').hidden = true; $('project-panel').hidden = false;
  $('project-title').textContent = p.name; $('state').textContent = p.state.toUpperCase();
  $('executable').replaceChildren(...p.executables.map(name => { const option = el('option', name); option.value = name; return option; }));
  $('executable').value = p.selected;
  $('files').textContent = p.files.join('\n');
  $('project-summary').textContent = `${p.files.length} files · ${p.executables.length} executable${p.executables.length === 1 ? '' : 's'} · Assets preserved in the source directory`;
  for (const id of ['analyze', 'build', 'executable']) $(id).disabled = busy(p);
  $('build-label').textContent = p.state.toUpperCase();
  $('ready-actions').hidden = p.state !== 'ready';
  $('log').hidden = !p.log; $('log').textContent = p.log;
  $('output').replaceChildren(el('span', busy(p) ? '◌' : p.state === 'ready' ? '↗' : p.error ? '!' : '⌘'),
    el('h3', busy(p) ? `Job ${p.state}…` : p.state === 'ready' ? 'Your browser bundle is ready.' : p.error ? 'Build needs attention.' : 'Ready when you are.'),
    el('p', p.error || (p.state === 'ready' ? 'Launch the hosted build or download it for static hosting.' : busy(p) ? 'The worker is processing your application.' : 'Inspect compatibility, then recompile your application.')));
  $('download').href = `/api/projects/${p.id}/download`;
  const reportNode = $('analysis'); reportNode.replaceChildren(); reportNode.className = 'empty';
  if (p.analysis) {
    const a = p.analysis; reportNode.className = 'report';
    $('architecture').textContent = `${a.kind} / ${a.machine}`.toUpperCase();
    const stats = el('div', '', 'stats');
    for (const [label, value] of [['Imports', a.imports], ['Libraries', a.libraries.length], ['Blockers', a.blocking.length]]) {
      const stat = el('div', '', 'stat'); stat.append(el('strong', String(value)), el('span', label)); stats.append(stat);
    }
    reportNode.append(stats);
    for (const [state, count] of Object.entries(a.compatibility)) reportNode.append(el('p', `${state}: ${count}`));
    if (!a.blocking.length) reportNode.append(el('p', 'No blocking imports reported. This does not verify instruction or runtime compatibility.'));
    for (const blocker of a.blocking) { const row = el('div', '', 'blocker'); row.append(el('strong', `${blocker.library} / ${blocker.symbol}`), el('small', `${blocker.state} · ${blocker.reason}`)); reportNode.append(row); }
  } else reportNode.append(el('span', '◎'), el('h3', 'Know before you compile.'), el('p', 'Inspect the selected executable to see its API compatibility.'));
}
function report(error) { notice(error.message, true); }
async function upload(file) {
  if (!file) return;
  if (file.size > 64 * 1024 * 1024) return notice('Upload limit is 64 MiB.', true);
  notice(`Importing ${file.name}…`);
  try {
    const p = await api('/api/projects', { method: 'POST', headers: {'Content-Type':'application/zip', 'X-Filename':encodeURIComponent(file.name)}, body:file });
    stopPlayer(); render(p); await refresh(); notice('Project imported. Select its entry executable, then inspect compatibility.');
  } catch (error) { report(error); }
  finally { $('upload').value = ''; }
}
async function job(action) {
  if (!current) return;
  try {
    stopPlayer();
    const p = await api(`/api/projects/${current.id}/${action}`, {method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify({selected:$('executable').value})});
    render(p); await refresh(); notice('');
  } catch (error) { report(error); }
}
$('upload').onchange = e => upload(e.target.files[0]);
for (const name of ['dragenter', 'dragover']) $('drop').addEventListener(name, e => { e.preventDefault(); $('drop').classList.add('drag'); });
for (const name of ['dragleave', 'drop']) $('drop').addEventListener(name, e => { e.preventDefault(); $('drop').classList.remove('drag'); });
$('drop').addEventListener('drop', e => upload(e.dataTransfer.files[0]));
$('new-project').onclick = () => {
  current = null; stopPlayer(); $('project-panel').hidden = true; $('import-panel').hidden = false; $('breadcrumb').textContent = 'Overview';
  $('analysis').className = 'empty'; $('analysis').replaceChildren(el('span', '◎'), el('h3', 'Know before you compile.'), el('p', 'Inspect Windows API imports and find what your application needs to run.'));
  $('output').replaceChildren(el('span', '⌘'), el('h3', 'A browser is the destination.'), el('p', 'Your build logs and runnable output will appear here.'));
  $('log').hidden = true; $('ready-actions').hidden = true; $('build-label').textContent = 'AWAITING PROJECT'; $('architecture').textContent = 'PE → WASM'; notice(''); refresh().catch(report);
};
$('executable').onchange = () => {
  stopPlayer();
  const changed = $('executable').value !== current?.selected;
  $('ready-actions').hidden = changed || current?.state !== 'ready';
  if (changed) notice('The displayed report belongs to the previous executable. Inspect compatibility to update it.');
  else notice('');
};
$('analyze').onclick = () => job('analyze'); $('build').onclick = () => job('build');
$('launch').onclick = () => { if (current?.state !== 'ready') return; $('player').src = playerURL(current); $('player-panel').hidden = false; $('player-panel').scrollIntoView({behavior:'smooth'}); };
$('stop').onclick = stopPlayer;
setInterval(async () => {
  if (polling || !current || !busy(current)) return;
  polling = true;
  const id = current.id;
  try { const p = await api(`/api/projects/${id}`); if (current?.id === id) render(p); await refresh(); }
  catch (error) { report(error); } finally { polling = false; }
}, 1000);
(async () => {
  config = await api('/api/config'); await refresh();
  if (projects.length) render(projects[0]);
  if (!config.analyzer) notice('Analyzer binary is missing. Build wlam-cli or set WLAM_BIN.');
  else if (!config.compiler) notice('Compatibility analysis is available. Connect a compiler adapter to enable runnable builds.');
})().catch(report);
