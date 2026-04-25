// GeniePod System Dashboard
// Polls /api/status, /api/tegrastats, /api/services every 5 seconds.
// Renders Chart.js time-series for RAM, GPU, and power.

const POLL_MS = 5000;
const MAX_POINTS = 120; // 10 minutes at 5s interval

// --- Chart setup ---
const chartOpts = (label, color, yMax) => ({
  responsive: true,
  maintainAspectRatio: false,
  animation: false,
  plugins: { legend: { display: false } },
  scales: {
    x: { display: false },
    y: {
      min: 0,
      max: yMax,
      ticks: { color: '#556', font: { size: 10 } },
      grid: { color: '#1e2730' },
    },
  },
  elements: { point: { radius: 0 }, line: { borderWidth: 1.5 } },
});

function makeChart(canvasId, label, color, yMax) {
  const ctx = document.getElementById(canvasId);
  if (!ctx) return null;
  return new Chart(ctx, {
    type: 'line',
    data: {
      labels: [],
      datasets: [{
        label,
        data: [],
        borderColor: color,
        backgroundColor: color + '18',
        fill: true,
        tension: 0.3,
      }],
    },
    options: chartOpts(label, color, yMax),
  });
}

let ramChart, gpuChart, powerChart;

function initCharts() {
  ramChart = makeChart('chart-ram', 'RAM Used (MB)', '#00d4ff', 8192);
  gpuChart = makeChart('chart-gpu', 'GPU %', '#7c4dff', 100);
  powerChart = makeChart('chart-power', 'Power (W)', '#ffd740', 20);
}

function pushPoint(chart, label, value) {
  if (!chart) return;
  chart.data.labels.push(label);
  chart.data.datasets[0].data.push(value);
  if (chart.data.labels.length > MAX_POINTS) {
    chart.data.labels.shift();
    chart.data.datasets[0].data.shift();
  }
  chart.update();
}

// --- Mode badge ---
function updateMode(mode) {
  const badge = document.getElementById('mode-badge');
  if (!badge) return;
  const m = (mode || 'unknown').replace(/_/g, '-');
  badge.textContent = m.toUpperCase();
  badge.className = 'mode mode-' + m;
}

// --- Stat values ---
function setText(id, val) {
  const el = document.getElementById(id);
  if (el) el.textContent = val;
}

// --- Fetch helpers ---
async function fetchJson(url) {
  try {
    const r = await fetch(url);
    return await r.json();
  } catch {
    return null;
  }
}

// --- Poll loops ---
async function pollStatus() {
  const data = await fetchJson('/api/status');
  if (!data) return;

  updateMode(data.mode);

  const memAvail = data.mem_available_mb_live ?? data.mem_available_mb ?? 0;
  const memTotal = 7620; // Orin Nano 8GB reports ~7620 MB
  const memUsed = memTotal - memAvail;

  setText('ram-used', memUsed);
  setText('ram-avail', memAvail);
  setText('ram-total', memTotal);

  const now = new Date().toLocaleTimeString([], { hour12: false, hour: '2-digit', minute: '2-digit', second: '2-digit' });
  pushPoint(ramChart, now, memUsed);
}

async function pollTegrastats() {
  const data = await fetchJson('/api/tegrastats');
  if (!data || !data.length) return;

  // Use the most recent entry.
  const latest = data[0];

  setText('gpu-freq', latest.gpu_pct ?? '--');
  setText('gpu-temp', latest.gpu_c != null ? latest.gpu_c.toFixed(1) : '--');
  setText('cpu-temp', latest.cpu_c != null ? latest.cpu_c.toFixed(1) : '--');

  const powerW = latest.power_mw != null ? (latest.power_mw / 1000).toFixed(1) : '--';
  setText('power', powerW);

  const now = new Date().toLocaleTimeString([], { hour12: false, hour: '2-digit', minute: '2-digit', second: '2-digit' });
  pushPoint(gpuChart, now, latest.gpu_pct ?? 0);
  pushPoint(powerChart, now, latest.power_mw != null ? latest.power_mw / 1000 : 0);

  // Backfill charts from historical data (only on first load).
  if (ramChart && ramChart.data.labels.length <= 1 && data.length > 1) {
    const history = data.slice().reverse().slice(-MAX_POINTS);
    for (const row of history) {
      const t = new Date(row.ts).toLocaleTimeString([], { hour12: false, hour: '2-digit', minute: '2-digit', second: '2-digit' });
      pushPoint(ramChart, t, row.ram_used ?? 0);
      pushPoint(gpuChart, t, row.gpu_pct ?? 0);
      pushPoint(powerChart, t, row.power_mw != null ? row.power_mw / 1000 : 0);
    }
  }
}

async function pollServices() {
  const data = await fetchJson('/api/services');
  const tbody = document.getElementById('services-body');
  if (!tbody) return;

  if (!data || !data.length) {
    tbody.innerHTML = '<tr><td colspan="3" style="color:var(--text2)">No data yet</td></tr>';
    return;
  }

  tbody.innerHTML = data.map(s => {
    const dotClass = s.healthy ? 'dot-up' : 'dot-down';
    const status = s.healthy ? 'Healthy' : (s.error || 'Down');
    const statusColor = s.healthy ? 'var(--green)' : 'var(--red)';
    return `<tr>
      <td><span class="dot ${dotClass}"></span>${s.service}</td>
      <td style="color:${statusColor}">${status}</td>
      <td>${s.response_ms}ms</td>
    </tr>`;
  }).join('');
}

async function pollRuntimeContract() {
  const data = await fetchJson('/api/runtime/contract');
  if (!data || data.error) {
    setText('contract-hash', '--');
    setText('contract-model', '--');
    setText('contract-tools', '--');
    setText('contract-prompt', '--');
    setText('contract-detail', data?.error ? `Runtime contract unavailable: ${data.error}` : 'Runtime contract unavailable.');
    return;
  }

  setText('contract-hash', data.contract_hash || '--');
  setText('contract-model', data.model_family || '--');
  setText('contract-tools', data.tool_count ?? '--');
  setText('contract-prompt', data.prompt_hash || '--');
  setText(
    'contract-detail',
    `policy ${data.policy_hash || '--'} · hydration ${data.hydration_hash || '--'} · history ${data.max_history_turns ?? '--'} turns`
  );
}

async function postJson(url, payload) {
  const r = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload),
  });
  return await r.json();
}

function formatTime(ts) {
  if (!ts) return '--';
  return new Date(ts).toLocaleString();
}

function escapeHtml(value) {
  return String(value ?? '')
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;');
}

async function pollActuation() {
  const pendingData = await fetchJson('/api/actuation/pending');
  const actionsData = await fetchJson('/api/actuation/actions');
  const auditData = await fetchJson('/api/actuation/audit');
  const pendingEl = document.getElementById('pending-list');
  const actionsEl = document.getElementById('action-list');
  const auditEl = document.getElementById('audit-list');
  const auditPathEl = document.getElementById('actuation-audit-path');
  if (auditPathEl) {
    auditPathEl.textContent = 'Audit log: ' + (pendingData?.audit_log_path || '--');
  }

  if (pendingEl) {
    const pending = pendingData?.pending || [];
    pendingEl.innerHTML = pending.length ? pending.map(item => `
      <div class="pending-item">
        <div class="pending-meta">
          <span class="token token-warn">${escapeHtml(item.requested_by || 'unknown')}</span>
          <span>${escapeHtml(item.action)} → ${escapeHtml(item.entity)}</span>
          <span>expires ${escapeHtml(formatTime(item.expires_ms))}</span>
        </div>
        <div class="small" style="margin-bottom:.55rem">${escapeHtml(item.reason)}</div>
        <div class="memory-actions">
          <button class="btn" type="button" onclick="confirmPendingAction('${escapeHtml(item.token)}')">Confirm</button>
          <span class="small">Token: ${escapeHtml(item.token)}</span>
        </div>
      </div>
    `).join('') : '<div class="empty">No pending confirmations.</div>';
  }

  if (actionsEl) {
    const actions = actionsData?.actions || [];
    actionsEl.innerHTML = actions.length ? actions.map(item => {
      const undo = item.inverse_action ? `undo: ${item.inverse_action}` : 'not undoable';
      return `
        <div class="action-item">
          <div class="action-meta">
            <span class="token">${escapeHtml(item.origin || 'unknown')}</span>
            <span>${escapeHtml(formatTime(item.executed_ms))}</span>
            <span>${escapeHtml(item.action || '')} → ${escapeHtml(item.entity || '')}</span>
            <span>${escapeHtml(undo)}</span>
          </div>
          <div class="small">${escapeHtml(item.summary || '')}</div>
        </div>
      `;
    }).join('') : '<div class="empty">No executed home actions yet.</div>';
  }

  if (auditEl) {
    const audit = Array.isArray(auditData) ? auditData : [];
    auditEl.innerHTML = audit.length ? audit.map(item => {
      const status = item.status || 'unknown';
      const tokenClass = status.includes('blocked') ? 'token-danger' : status.includes('confirmation') ? 'token-warn' : '';
      return `
        <div class="audit-item">
          <div class="audit-meta">
            <span class="token ${tokenClass}">${escapeHtml(status)}</span>
            <span>${escapeHtml(item.origin || 'unknown')}</span>
            <span>${escapeHtml(formatTime(item.ts_ms))}</span>
            <span>${escapeHtml(item.action || '')} → ${escapeHtml(item.entity || '')}</span>
          </div>
          <div class="small">${escapeHtml(item.reason || '')}</div>
        </div>
      `;
    }).join('') : '<div class="empty">No actuation audit events yet.</div>';
  }
}

async function confirmPendingAction(token) {
  const result = await postJson('/api/actuation/confirm', { token });
  if (!result?.ok) {
    alert(result?.error || 'Confirmation failed');
    return;
  }
  await pollActuation();
}

let memoryEntries = [];

function renderMemories() {
  const list = document.getElementById('memory-list');
  if (!list) return;
  if (!memoryEntries.length) {
    list.innerHTML = '<div class="empty">No saved memories yet.</div>';
    return;
  }

  list.innerHTML = memoryEntries.map((entry, index) => `
    <div class="memory-item" data-memory-id="${entry.id}">
      <div class="memory-meta">
        <span class="token">${escapeHtml(entry.kind)}</span>
        <span>${escapeHtml(entry.scope)}</span>
        <span>${escapeHtml(entry.sensitivity)}</span>
        <span>recalls ${escapeHtml(entry.recall_count)}</span>
        <span>${escapeHtml(formatTime(entry.accessed_ms))}</span>
      </div>
      <textarea class="memory-content" id="memory-content-${entry.id}"></textarea>
      <div class="memory-actions">
        <button class="btn" type="button" onclick="saveMemory(${entry.id})">Save</button>
        <button class="btn" type="button" onclick="moveMemory(${index}, -1)" ${index === 0 ? 'disabled' : ''}>Up</button>
        <button class="btn" type="button" onclick="moveMemory(${index}, 1)" ${index === memoryEntries.length - 1 ? 'disabled' : ''}>Down</button>
        <button class="btn btn-danger" type="button" onclick="deleteMemory(${entry.id})">Delete</button>
      </div>
    </div>
  `).join('');

  for (const entry of memoryEntries) {
    const textarea = document.getElementById(`memory-content-${entry.id}`);
    if (textarea) textarea.value = entry.content || '';
  }
}

async function loadMemories() {
  const data = await fetchJson('/api/memories');
  memoryEntries = Array.isArray(data) ? data : [];
  renderMemories();
}

async function saveMemory(id) {
  const input = document.getElementById(`memory-content-${id}`);
  if (!input) return;
  const result = await postJson('/api/memories/update', { id, content: input.value });
  if (!result?.ok) {
    alert(result?.error || 'Failed to save memory');
    return;
  }
  await loadMemories();
}

async function deleteMemory(id) {
  const result = await postJson('/api/memories/delete', { id });
  if (!result?.ok) {
    alert(result?.error || 'Failed to delete memory');
    return;
  }
  await loadMemories();
}

async function moveMemory(index, delta) {
  const next = index + delta;
  if (next < 0 || next >= memoryEntries.length) return;
  const updated = [...memoryEntries];
  const [item] = updated.splice(index, 1);
  updated.splice(next, 0, item);
  memoryEntries = updated;
  renderMemories();
  await postJson('/api/memories/reorder', { ids: memoryEntries.map(item => item.id) });
}

// --- Init ---
document.addEventListener('DOMContentLoaded', () => {
  if (typeof Chart !== 'undefined') {
    initCharts();
  } else {
    // Chart.js CDN failed (offline mode) — skip charts.
    console.warn('Chart.js not loaded, charts disabled');
  }

  // Initial polls.
  pollStatus();
  pollTegrastats();
  pollServices();
  pollRuntimeContract();
  pollActuation();
  loadMemories();

  // Recurring polls.
  setInterval(pollStatus, POLL_MS);
  setInterval(pollTegrastats, POLL_MS);
  setInterval(pollServices, POLL_MS * 2); // Services change slowly.
  setInterval(pollRuntimeContract, POLL_MS * 4); // Runtime contract should rarely change.
  setInterval(pollActuation, POLL_MS * 2);

  document.getElementById('refresh-actuation')?.addEventListener('click', pollActuation);
  document.getElementById('refresh-memories')?.addEventListener('click', loadMemories);
});

window.confirmPendingAction = confirmPendingAction;
window.moveMemory = moveMemory;
window.saveMemory = saveMemory;
window.deleteMemory = deleteMemory;
