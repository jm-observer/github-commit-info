// 简易 JS：直接 fetch / api/* —— 与 server 同源，无 CSP 问题。
const $ = (id) => document.getElementById(id);
const fmt = (v) => (v == null ? "—" : JSON.stringify(v, null, 2));

async function api(path, init = {}) {
  const res = await fetch(path, {
    headers: { "Content-Type": "application/json" },
    ...init,
  });
  const text = await res.text();
  let body;
  try { body = JSON.parse(text); } catch { body = text; }
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}: ${typeof body === "string" ? body : body.error || text}`);
  return body;
}

// -------- tabs --------
document.querySelectorAll("nav#tabs button").forEach((b) => {
  b.onclick = () => {
    document.querySelectorAll("nav#tabs button").forEach((x) => x.classList.remove("active"));
    document.querySelectorAll("main section").forEach((x) => x.classList.remove("active"));
    b.classList.add("active");
    $("tab-" + b.dataset.tab).classList.add("active");
    if (b.dataset.tab === "tasks") refreshTasks();
    if (b.dataset.tab === "douyin") refreshCreators();
    if (b.dataset.tab === "workbench") wbRefreshCreators();
    if (b.dataset.tab === "asr") {
      // 懒加载 ASR 控制台 iframe：首次切到「语音」tab 时才注入 src，
      // 避免每次开主面板都建立 WS。
      // 注意:必须用无尾斜杠的 "/api/asr" —— axum `nest("/api/asr", ..)` 把内层
      // `/` 路由映射到 `/api/asr`(无斜杠)上,带斜杠的 `/api/asr/` 反而 404,
      // iframe 会白屏。
      const f = $("asr-frame");
      if (f && !f.src) f.src = "/api/asr";
    }
  };
});

// -------- 概览 --------
async function refreshHealth() {
  try {
    const h = await api("/api/web/health");
    $("health-detail").textContent = fmt(h);
    $("pill-health").textContent = "health: ok";
    $("pill-health").className = "pill ok";
  } catch (e) {
    $("health-detail").textContent = e.message;
    $("pill-health").textContent = "health: err";
    $("pill-health").className = "pill err";
  }
}

async function refreshCookie() {
  try {
    const c = await api("/api/web/douyin/cookie_status");
    $("cookie-detail").textContent = fmt(c);
    const ok = c.has_required && c.logged_in;
    $("pill-cookie").textContent = ok ? `cookie: ok (${c.fields} fields)` : "cookie: 待登录";
    $("pill-cookie").className = ok ? "pill ok" : "pill warn";
  } catch (e) {
    $("cookie-detail").textContent = e.message;
    $("pill-cookie").textContent = "cookie: err";
    $("pill-cookie").className = "pill err";
  }
}

async function refreshOverviewTasks() {
  try {
    const r = await api("/api/web/tasks?limit=10");
    const rows = (r.tasks || r || []).slice(0, 10);
    renderTaskRows($("overview-tasks").querySelector("tbody"), rows);
  } catch (e) {
    $("overview-tasks").querySelector("tbody").innerHTML =
      `<tr><td colspan="4">${e.message}</td></tr>`;
  }
}

$("overview-refresh").onclick = () => {
  refreshHealth();
  refreshCookie();
  refreshOverviewTasks();
};

function renderTaskRows(tbody, rows) {
  if (!rows.length) {
    tbody.innerHTML = `<tr><td colspan="5"><span style="color:var(--muted)">无</span></td></tr>`;
    return;
  }
  tbody.innerHTML = rows
    .map((t) => {
      const tid = t.task_id || t.id || "—";
      return `<tr data-id="${tid}">
        <td><code>${tid}</code></td>
        <td>${t.kind || "—"}</td>
        <td class="state-${t.state || ""}">${t.state || "—"}</td>
        <td>${t.created_at || "—"}</td>
        <td>${t.updated_at || "—"}</td>
      </tr>`;
    })
    .join("");
  tbody.querySelectorAll("tr").forEach((tr) => {
    tr.onclick = () => {
      $("task-id").value = tr.dataset.id;
      document.querySelector('nav#tabs button[data-tab="tasks"]').click();
      fetchTaskDetail();
    };
  });
}

// -------- 抖音 --------
document.querySelectorAll('#tab-douyin button[data-act]').forEach((b) => {
  b.onclick = async () => {
    const handle = $("dy-handle").value.trim();
    const out = $("dy-out");
    out.textContent = "loading…";
    try {
      let path;
      if (b.dataset.act === "creator") path = `/api/web/douyin/creator?handle=${encodeURIComponent(handle)}`;
      else if (b.dataset.act === "works") path = `/api/web/douyin/works?handle=${encodeURIComponent(handle)}`;
      else if (b.dataset.act === "cookie_status") path = "/api/web/douyin/cookie_status";
      else if (b.dataset.act === "tags") path = `/api/web/douyin/tags?unique_id=${encodeURIComponent($("dy-uid").value.trim())}`;
      else if (b.dataset.act === "filter")
        path = `/api/web/douyin/filter?unique_id=${encodeURIComponent($("dy-uid").value.trim())}&tags=${encodeURIComponent($("dy-tags").value.trim())}&match=${$("dy-match").value}`;
      const r = await api(path);
      const target = b.dataset.act === "tags" || b.dataset.act === "filter" ? $("dy-filter-out") : out;
      target.textContent = fmt(r);
    } catch (e) { out.textContent = e.message; }
  };
});

document.querySelectorAll('#tab-douyin button[data-task]').forEach((b) => {
  b.onclick = async () => {
    const out = $("dy-task-out");
    let body;
    try { body = JSON.parse($("dy-task-input").value || "{}"); }
    catch (e) { out.textContent = "JSON parse: " + e.message; return; }
    out.textContent = "submitting…";
    try {
      const r = await api(`/api/web/douyin/${b.dataset.task}`, {
        method: "POST", body: JSON.stringify(body),
      });
      out.textContent = fmt(r);
    } catch (e) { out.textContent = e.message; }
  };
});

// -------- 任务 --------
async function refreshTasks() {
  const tbody = $("tasks-table").querySelector("tbody");
  const params = new URLSearchParams();
  const k = $("tasks-kind").value.trim(); if (k) params.set("kind", k);
  const st = $("tasks-state").value.trim(); if (st) params.set("state", st);
  const lim = $("tasks-limit").value.trim(); if (lim) params.set("limit", lim);
  try {
    const r = await api("/api/web/tasks?" + params);
    renderTaskRows(tbody, r.tasks || r || []);
  } catch (e) {
    tbody.innerHTML = `<tr><td colspan="5">${e.message}</td></tr>`;
  }
}
$("tasks-refresh").onclick = refreshTasks;

async function fetchTaskDetail() {
  const id = $("task-id").value.trim();
  if (!id) return;
  $("task-detail").textContent = "loading…";
  try {
    const r = await api("/api/web/tasks/" + encodeURIComponent(id));
    $("task-detail").textContent = fmt(r);
  } catch (e) { $("task-detail").textContent = e.message; }
}
$("task-detail-btn").onclick = fetchTaskDetail;

$("submit-btn").onclick = async () => {
  const out = $("submit-out");
  let input;
  try { input = JSON.parse($("submit-input").value || "{}"); }
  catch (e) { out.textContent = "JSON parse: " + e.message; return; }
  const body = { kind: $("submit-kind").value.trim(), input };
  const cb = $("submit-cb").value.trim();
  if (cb) body.callback_url = cb;
  out.textContent = "submitting…";
  try {
    const r = await api("/api/web/tasks", { method: "POST", body: JSON.stringify(body) });
    out.textContent = fmt(r);
    refreshTasks();
  } catch (e) { out.textContent = e.message; }
};

// -------- 知识库 --------
$("kb-btn").onclick = async () => {
  const out = $("kb-out");
  const uid = $("kb-uid").value.trim();
  const ids = $("kb-ids").value.split(/[,\s]+/).filter(Boolean);
  out.textContent = "publishing…";
  try {
    const r = await api("/api/web/douyin/kb_publish", {
      method: "POST",
      body: JSON.stringify({ unique_id: uid, only_ids: ids }),
    });
    out.textContent = fmt(r);
  } catch (e) { out.textContent = e.message; }
};

// -------- desktop 桥（127.0.0.1:28788） --------
const DESKTOP_BRIDGE = "http://127.0.0.1:28788";
let lastDesktopCtx = null;

async function refreshDesktop() {
  const pill = $("pill-desktop");
  try {
    const res = await fetch(DESKTOP_BRIDGE + "/context", { cache: "no-store" });
    if (!res.ok) throw new Error("HTTP " + res.status);
    const ctx = await res.json();
    lastDesktopCtx = ctx;
    const loginOk = ctx.login && ctx.login.has_window;
    const msOk = ctx.ms_token && ctx.ms_token.present;
    const thsOk = ctx.ths && ctx.ths.has_required && !ctx.ths.ticket_is_session;
    const parts = [];
    parts.push(loginOk ? "login✓" : "login✗");
    parts.push(msOk ? "msToken✓" : "msToken✗");
    parts.push(thsOk ? "ths✓" : "ths-");
    pill.textContent = "desktop: " + parts.join(" ");
    pill.className = "pill " + (loginOk && msOk ? "ok" : "warn");
    const det = $("ctx-detail");
    if (det) det.textContent = JSON.stringify(ctx, null, 2);
  } catch (e) {
    lastDesktopCtx = null;
    pill.textContent = "desktop: 离线";
    pill.className = "pill err";
    pill.title = String(e);
    const det = $("ctx-detail");
    if (det && !det.textContent.startsWith("{")) {
      det.textContent = "未连接到 desktop 桥 (127.0.0.1:28788): " + e.message;
    }
  }
}

document.addEventListener("DOMContentLoaded", () => {
  const r = document.getElementById("ctx-refresh");
  if (r) r.onclick = refreshDesktop;
  const cr = document.getElementById("creators-refresh");
  if (cr) cr.onclick = refreshCreators;
});

async function refreshCreators() {
  const tbody = $("creators-table").querySelector("tbody");
  try {
    const r = await api("/api/web/douyin/creators?limit=200");
    const list = r.creators || [];
    if (!list.length) {
      tbody.innerHTML = `<tr><td colspan="6" style="color:var(--muted)">空。到桌面端点「解析当前博主」加入。</td></tr>`;
      return;
    }
    tbody.innerHTML = list.map((c) => {
      const last = c.last_synced_at ? c.last_synced_at.slice(0, 19).replace("T", " ") : "—";
      return `<tr data-uid="${c.unique_id}" data-secuid="${c.sec_uid || ""}" data-url="https://www.douyin.com/user/${c.sec_uid || ""}">
        <td>${c.nickname || "—"}</td>
        <td><code>${c.unique_id}</code></td>
        <td>${c.aweme_count ?? "—"}</td>
        <td>${c.follower_count ?? "—"}</td>
        <td style="color:var(--muted)">${last}</td>
        <td>
          <button class="secondary act-tags" style="margin:0;padding:3px 8px;font-size:10px;">tags</button>
          <button class="secondary act-sync" style="margin:0;padding:3px 8px;font-size:10px;">sync_works</button>
        </td>
      </tr>`;
    }).join("");
    tbody.querySelectorAll("tr").forEach((tr) => {
      const uid = tr.dataset.uid;
      const url = tr.dataset.url;
      tr.querySelector(".act-tags").onclick = async (ev) => {
        ev.stopPropagation();
        try {
          const r = await api(`/api/web/douyin/tags?unique_id=${encodeURIComponent(uid)}`);
          $("dy-uid").value = uid;
          $("dy-filter-out").textContent = JSON.stringify(r, null, 2);
        } catch (e) { alert(e.message); }
      };
      tr.querySelector(".act-sync").onclick = async (ev) => {
        ev.stopPropagation();
        try {
          const r = await api(`/api/web/douyin/sync_works`, {
            method: "POST",
            body: JSON.stringify({ input: url, max_pages: 60 }),
          });
          $("dy-task-out").textContent = JSON.stringify(r, null, 2);
          refreshTasks();
        } catch (e) { alert(e.message); }
      };
      tr.onclick = () => { $("dy-handle").value = url; };
    });
  } catch (e) {
    tbody.innerHTML = `<tr><td colspan="6">${e.message}</td></tr>`;
  }
}
$("pill-desktop").onclick = refreshDesktop;

// ======== 博主工作台 ========
let wbCreator = null;          // {unique_id, sec_uid, nickname, url}
let wbWorks = [];              // /works_saved 的 works
const wbActiveTags = new Set();

const wbEsc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) =>
  ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

async function wbRefreshCreators() {
  const tbody = $("wb-creators-table").querySelector("tbody");
  try {
    const r = await api("/api/web/douyin/creators?limit=200");
    const list = r.creators || [];
    if (!list.length) {
      tbody.innerHTML = `<tr><td style="color:var(--muted)">空。到桌面端「解析当前博主」收录。</td></tr>`;
      return;
    }
    tbody.innerHTML = list.map((c) =>
      `<tr data-uid="${wbEsc(c.unique_id)}" data-secuid="${wbEsc(c.sec_uid || "")}" data-nick="${wbEsc(c.nickname || "")}">
        <td><div>${wbEsc(c.nickname || c.unique_id)}</div>
        <div style="font-size:10px;color:var(--muted)">${wbEsc(c.unique_id)} · ${c.aweme_count ?? "?"}作品</div></td>
      </tr>`).join("");
    tbody.querySelectorAll("tr").forEach((tr) => {
      tr.onclick = () => {
        tbody.querySelectorAll("tr").forEach((x) => x.classList.remove("sel"));
        tr.classList.add("sel");
        wbSelect({
          unique_id: tr.dataset.uid, sec_uid: tr.dataset.secuid, nickname: tr.dataset.nick,
          url: "https://www.douyin.com/user/" + tr.dataset.secuid,
        });
      };
    });
  } catch (e) {
    tbody.innerHTML = `<tr><td>${wbEsc(e.message)}</td></tr>`;
  }
}

function wbSelect(c) {
  wbCreator = c;
  wbActiveTags.clear();
  $("wb-empty").style.display = "none";
  $("wb-panel").style.display = "";
  $("wb-title").textContent = c.nickname || c.unique_id;
  $("wb-meta").textContent = c.unique_id;
  $("wb-out").textContent = "—";
  wbLoadTags();
  wbLoadWorks();
}

async function wbLoadTags() {
  const box = $("wb-tags");
  box.innerHTML = "";
  try {
    const r = await api(`/api/web/douyin/tags?unique_id=${encodeURIComponent(wbCreator.unique_id)}`);
    const tags = r.tags || [];
    if (!tags.length) { box.innerHTML = `<span style="font-size:11px;color:var(--muted)">无标签（先同步作品）</span>`; return; }
    box.innerHTML = tags.map((t) =>
      `<button class="wb-chip" data-tag="${wbEsc(t.name)}">${wbEsc(t.name)} · ${t.count}</button>`).join("");
    box.querySelectorAll(".wb-chip").forEach((ch) => {
      ch.onclick = () => {
        const tag = ch.dataset.tag;
        if (wbActiveTags.has(tag)) { wbActiveTags.delete(tag); ch.classList.remove("on"); }
        else { wbActiveTags.add(tag); ch.classList.add("on"); }
        wbRenderWorks();
      };
    });
  } catch (e) { box.innerHTML = `<span style="font-size:11px;color:var(--muted)">${wbEsc(e.message)}</span>`; }
}

async function wbLoadWorks() {
  const tbody = $("wb-works").querySelector("tbody");
  tbody.innerHTML = `<tr><td colspan="4" style="color:var(--muted)">加载中…</td></tr>`;
  try {
    const r = await api(`/api/web/douyin/works_saved?unique_id=${encodeURIComponent(wbCreator.unique_id)}`);
    wbWorks = r.works || [];
    const meta = wbWorks.length
      ? `${wbCreator.unique_id} · 已存盘 ${r.count} / 共 ${r.aweme_count ?? "?"}${r.throttled ? " ⚠️抽稀" : ""} · ${(r.cached_at || "").slice(0, 19).replace("T", " ")}`
      : wbCreator.unique_id;
    $("wb-meta").textContent = meta;
    if (!wbWorks.length) {
      tbody.innerHTML = `<tr><td colspan="4" style="color:var(--muted)">尚未同步作品。点右上「同步/更新作品」。</td></tr>`;
      return;
    }
    wbRenderWorks();
  } catch (e) { tbody.innerHTML = `<tr><td colspan="4">${wbEsc(e.message)}</td></tr>`; }
}

function wbRenderWorks() {
  const tbody = $("wb-works").querySelector("tbody");
  const sel = [...wbActiveTags];
  const rows = wbWorks.filter((w) => sel.every((t) => (w.tags || []).includes(t)));
  if (!rows.length) { tbody.innerHTML = `<tr><td colspan="4" style="color:var(--muted)">无匹配作品。</td></tr>`; wbUpdateCount(); return; }
  tbody.innerHTML = rows.map((w) => {
    const t = w.create_ym || (w.create_time ? new Date(w.create_time * 1000).toISOString().slice(0, 10) : "—");
    const badge = (on, cls, label) => `<span class="badge ${cls} ${on ? "on" : ""}">${label}</span>`;
    const tagline = (w.tags || []).length ? `<div style="font-size:10px;color:var(--muted)">#${(w.tags || []).join(" #")}</div>` : "";
    return `<tr>
      <td><input type="checkbox" class="wb-cb" data-id="${wbEsc(w.aweme_id)}" /></td>
      <td><div class="desc">${wbEsc(w.title || w.desc || w.aweme_id)}</div>${tagline}</td>
      <td style="color:var(--muted)">${wbEsc(t)}</td>
      <td>${badge(w.downloaded, "dl", "下载")}${badge(w.transcribed, "asr", "识别")}${badge(w.refined, "ref", "整理")}</td>
    </tr>`;
  }).join("");
  tbody.querySelectorAll(".wb-cb").forEach((cb) => (cb.onchange = wbUpdateCount));
  $("wb-selall").checked = false;
  wbUpdateCount();
}

function wbSelectedIds() {
  return [...document.querySelectorAll("#wb-works .wb-cb:checked")].map((c) => c.dataset.id);
}
function wbUpdateCount() {
  $("wb-selcount").textContent = "已选 " + wbSelectedIds().length;
}

async function wbPost(path, body, label) {
  const out = $("wb-out");
  out.textContent = label + " 提交中…";
  try {
    const r = await api(path, { method: "POST", body: JSON.stringify(body) });
    out.textContent = `✓ ${label} 已提交：task_id=${r.task_id || "?"}\n` + fmt(r);
    if (typeof refreshTasks === "function") refreshTasks();
    return r;
  } catch (e) { out.textContent = `✗ ${label}：` + e.message; }
}

function wbOpOnSelection(path, label, withUid, idKey = "aweme_ids") {
  const ids = wbSelectedIds();
  if (!ids.length) { $("wb-out").textContent = "请先勾选作品。"; return; }
  const body = { [idKey]: ids };
  if (withUid) body.unique_id = wbCreator.unique_id;
  wbPost(`/api/web/douyin/${path}`, body, label);
}

document.addEventListener("DOMContentLoaded", () => {
  $("wb-creators-refresh").onclick = wbRefreshCreators;
  $("wb-reload").onclick = wbLoadWorks;
  $("wb-selall").onchange = (e) => {
    document.querySelectorAll("#wb-works .wb-cb").forEach((cb) => (cb.checked = e.target.checked));
    wbUpdateCount();
  };
  $("wb-sync").onclick = () =>
    wbPost("/api/web/douyin/sync_works", { handle: wbCreator.url, max_pages: 60 }, "同步作品")
      .then(() => { $("wb-out").textContent += "\n（任务进行中，完成后点「刷新作品」）"; });
  $("wb-pipeline").onclick = () =>
    wbPost("/api/web/douyin/pipeline", { handle: wbCreator.url }, "整链 pipeline");
  $("wb-op-download").onclick = () => wbOpOnSelection("download", "下载", false);
  $("wb-op-transcribe").onclick = () => wbOpOnSelection("transcribe", "识别", true);
  $("wb-op-refine").onclick = () => wbOpOnSelection("refine", "整理", true);
  $("wb-op-kb").onclick = () => wbOpOnSelection("kb_publish", "入库", true, "only_ids");
});

// init
refreshHealth();
refreshCookie();
refreshOverviewTasks();
refreshDesktop();
// 30s 自动刷新概览状态；desktop 桥 10s 一次
setInterval(() => { refreshHealth(); refreshCookie(); }, 30000);
setInterval(refreshDesktop, 10000);
