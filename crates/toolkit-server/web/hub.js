// Hub shell — 每个模块 × 2 环境（local / g10），各自独立 URL 与健康状态。
// 改 g10 IP 直接改 ENVS 即可；不再依赖 window.location.hostname。

const ENVS = [
  { id: 'local', name: 'local', host: 'localhost' },
  { id: 'g10',   name: 'g10',   host: '192.168.0.68' },
];

// 每个模块默认端口 = `port`；某环境端口不同时用 `ports: {envId: N}` 覆盖
const MODULES = [
  { id: 'toolkit',      name: 'Toolkit',      port: 8788, health: '/api/web/health' },
  { id: 'trace-hub',    name: 'Trace Hub',    port: 9120, health: '/v1/traces' },
  { id: 'orchestrator', name: 'Orchestrator', port: 8090, health: '/api/stats' },
  { id: 'prompt-show',  name: 'Prompt Show',  port: 8267, ports: { g10: 9201 }, health: '/api/sessions' },
  { id: 'alarm',        name: 'Alarm',        port: 3030, ports: { g10: 9080 }, health: '/api/health' },
];

const portFor   = (m, e) => (m.ports && m.ports[e.id]) || m.port;
const url       = (m, e) => `http://${e.host}:${portFor(m, e)}/`;
const healthUrl = (m, e) => `http://${e.host}:${portFor(m, e)}${m.health}`;
const cellKey   = (mid, eid) => `${mid}.${eid}`;

// ── DOM refs ────────────────────────────────────────────────────────────────
const nav         = document.getElementById('nav');
const frame       = document.getElementById('frame');
const topbarTitle = document.getElementById('topbar-title');

// ── 状态 ─────────────────────────────────────────────────────────────────────
function loadActive() {
  try {
    const raw = JSON.parse(localStorage.getItem('hub.active') || 'null');
    if (raw && MODULES.some(m => m.id === raw.moduleId) && ENVS.some(e => e.id === raw.envId)) {
      return raw;
    }
  } catch {}
  return { moduleId: MODULES[0].id, envId: ENVS[0].id };
}
function saveActive(a) { localStorage.setItem('hub.active', JSON.stringify(a)); }

let active = loadActive();
const expanded = new Set([active.moduleId]); // 默认展开当前激活模块

// ── 渲染 ─────────────────────────────────────────────────────────────────────
const envButtons = {}; // key → <button>
const dots       = {}; // key → <span>
const groupEls   = {}; // moduleId → { header, children, chevron }

MODULES.forEach((m) => {
  const group = document.createElement('div');
  group.className = 'group';

  const header = document.createElement('button');
  header.className = 'group-header';
  header.innerHTML =
    `<span class="chevron">+</span><span class="nav-label">${m.name}</span>`;
  header.addEventListener('click', () => toggleGroup(m.id));

  const children = document.createElement('div');
  children.className = 'group-children';

  ENVS.forEach((e) => {
    const btn = document.createElement('button');
    btn.className = 'env-btn';
    const dot = document.createElement('span');
    dot.className = 'health-dot unknown';
    dot.title = 'checking…';
    btn.innerHTML = `<span class="env-label">${e.name}</span>`;
    btn.appendChild(dot);
    btn.addEventListener('click', () => selectCell(m.id, e.id));
    children.appendChild(btn);
    envButtons[cellKey(m.id, e.id)] = btn;
    dots[cellKey(m.id, e.id)] = dot;
  });

  group.appendChild(header);
  group.appendChild(children);
  nav.appendChild(group);
  groupEls[m.id] = { header, children, chevron: header.querySelector('.chevron') };
});

function applyExpansion() {
  MODULES.forEach((m) => {
    const g = groupEls[m.id];
    const open = expanded.has(m.id);
    g.children.style.display = open ? 'block' : 'none';
    g.chevron.textContent = open ? '−' : '+';
  });
}

function toggleGroup(id) {
  if (expanded.has(id)) expanded.delete(id); else expanded.add(id);
  applyExpansion();
}

function selectCell(moduleId, envId) {
  const m = MODULES.find(x => x.id === moduleId);
  const e = ENVS.find(x => x.id === envId);
  if (!m || !e) return;

  Object.values(envButtons).forEach(b => b.classList.remove('active'));
  envButtons[cellKey(moduleId, envId)].classList.add('active');

  frame.src = url(m, e);
  topbarTitle.textContent = `${m.name} · ${e.name}`;
  active = { moduleId, envId };
  saveActive(active);
}

applyExpansion();
selectCell(active.moduleId, active.envId);

// ── 健康轮询（每个 module×env 单元独立）──────────────────────────────────────
async function checkCell(m, e) {
  const dot = dots[cellKey(m.id, e.id)];
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), 3000);
  try {
    await fetch(healthUrl(m, e), { mode: 'no-cors', signal: ctrl.signal });
    dot.className = 'health-dot green';
    dot.title = `${e.host}:${m.port} reachable`;
  } catch {
    dot.className = 'health-dot red';
    dot.title = `${e.host}:${m.port} unreachable`;
  } finally {
    clearTimeout(timer);
  }
}

function pollAll() {
  MODULES.forEach(m => ENVS.forEach(e => checkCell(m, e)));
}
pollAll();
setInterval(pollAll, 10_000);
