// gitview 前端。
// 沒有打包工具與框架：這個介面是儀表板與一張圖，用不到那些東西，
// 少一層建置流程就少一整類版本問題。

// 這些是 Tauri 注入的全域物件。外掛的 JS 綁定不保證存在，
// 因此不在頂層解構 —— 少一個屬性就會讓整個腳本停在載入階段，
// 使用者只會看到一片空白，完全無從得知原因。
const tauri = window.__TAURI__ || {};
const invoke = tauri.core && tauri.core.invoke;
const listen = tauri.event && tauri.event.listen;

/** 開啟資料夾選擇器；外掛不可用時回傳 null 由呼叫端處理。 */
async function pickDirectory(title) {
  if (tauri.dialog && tauri.dialog.open) {
    return tauri.dialog.open({ directory: true, multiple: false, title });
  }
  return null;
}

/** 把啟動階段的錯誤顯示在畫面上，而不是留下空白視窗。 */
function fatal(message) {
  document.body.innerHTML = "";
  const box = document.createElement("pre");
  box.style.cssText = "margin:2rem;padding:1rem;border:1px solid #a32f2f;color:#a32f2f;white-space:pre-wrap;font-family:monospace";
  box.textContent = `gitview 無法啟動\n\n${message}`;
  document.body.appendChild(box);
}

const ROW_HEIGHT = 30;
const LANE_WIDTH = 16;
const LANE_COLOURS = 8;

const state = {
  repos: [],
  selected: null,
  tab: "divergence",
  workspace: null,
  selectedConflict: null,
  diffFiles: [],
  selectedFile: null,
  /** "inline" 或 "split"。 */
  diffMode: "inline",
  /** "unstaged"、"staged" 或 "commit"。 */
  diffSource: "unstaged",
  selectedLines: new Set(),
  commitDetail: null,
};

const el = (id) => document.getElementById(id);

/* ---------- 共用 ---------- */

function laneColour(index) {
  const styles = getComputedStyle(document.documentElement);
  return styles.getPropertyValue(`--lane-${index % LANE_COLOURS}`).trim() || "#888";
}

function setStatus(text) {
  el("global-status").textContent = text || "";
}

function relativeTime(millis) {
  if (!millis) return "從未";
  const seconds = Math.max(0, Math.round((Date.now() - millis) / 1000));
  if (seconds < 60) return "剛剛";
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分鐘前`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)} 小時前`;
  return `${Math.floor(seconds / 86400)} 天前`;
}

/** 狀態標籤。文字本身就說明狀態，不單靠顏色傳達資訊。 */
function chipsFor(repo) {
  const chips = [];
  if (repo.error) {
    chips.push(["attention", "無法讀取"]);
    return chips;
  }
  if (repo.operation) chips.push(["attention", repo.operation]);
  if (repo.working_tree.conflicted > 0) {
    chips.push(["attention", `${repo.working_tree.conflicted} 個衝突`]);
  }
  if (repo.ahead > 0 && repo.behind > 0) {
    chips.push(["diverged", `分岔 ↑${repo.ahead} ↓${repo.behind}`]);
  } else {
    if (repo.behind > 0) chips.push(["incoming", `↓ ${repo.behind} 個待拉取`]);
    if (repo.ahead > 0) chips.push(["unpushed", `↑ ${repo.ahead} 個未推送`]);
  }
  const dirty = repo.working_tree.total - repo.working_tree.conflicted;
  if (dirty > 0) chips.push(["uncommitted", `${dirty} 個未提交`]);
  if (chips.length === 0) chips.push(["clean", "乾淨"]);
  return chips;
}

function renderChips(container, repo) {
  container.replaceChildren();
  for (const [kind, text] of chipsFor(repo)) {
    const span = document.createElement("span");
    span.className = `chip ${kind}`;
    span.textContent = text;
    container.appendChild(span);
  }
}

/* ---------- Repository 清單 ---------- */

function renderRepoList() {
  const container = el("repos");
  container.replaceChildren();
  el("empty-hint").hidden = state.repos.length > 0;

  for (const repo of state.repos) {
    const card = document.createElement("button");
    card.type = "button";
    card.className = "repo-card" + (repo.path === state.selected ? " selected" : "");
    card.addEventListener("click", () => selectRepo(repo.path));

    const top = document.createElement("div");
    top.className = "repo-card-top";
    const name = document.createElement("span");
    name.className = "repo-name";
    name.textContent = repo.name;
    const branch = document.createElement("span");
    branch.className = "repo-branch";
    branch.textContent = repo.branch || "(detached)";
    top.append(name, branch);

    const bottom = document.createElement("div");
    bottom.className = "repo-card-bottom";
    renderChips(bottom, repo);

    card.append(top, bottom);
    container.appendChild(card);
  }
}

/* ---------- 同步狀態 ---------- */

/** 小圖示意 rebase 與 merge 之後歷史的形狀。 */
function outcomeDiagram(kind, aheadCount, behindCount) {
  const svgNS = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(svgNS, "svg");
  const rows = Math.min(aheadCount + behindCount, 6) + 1;
  const height = rows * 18 + 10;
  svg.setAttribute("viewBox", `0 0 120 ${height}`);
  svg.setAttribute("width", "120");
  svg.setAttribute("height", String(height));
  svg.setAttribute("role", "img");

  const line = (x1, y1, x2, y2, colour) => {
    const path = document.createElementNS(svgNS, "line");
    path.setAttribute("x1", x1); path.setAttribute("y1", y1);
    path.setAttribute("x2", x2); path.setAttribute("y2", y2);
    path.setAttribute("stroke", colour);
    path.setAttribute("stroke-width", "2");
    svg.appendChild(path);
  };
  const dot = (x, y, colour, filled = true) => {
    const circle = document.createElementNS(svgNS, "circle");
    circle.setAttribute("cx", x); circle.setAttribute("cy", y);
    circle.setAttribute("r", "4");
    circle.setAttribute("fill", filled ? colour : "var(--surface)");
    circle.setAttribute("stroke", colour);
    circle.setAttribute("stroke-width", "2");
    svg.appendChild(circle);
  };

  const local = laneColour(1);
  const remote = laneColour(0);
  const shownAhead = Math.min(aheadCount, 3);
  const shownBehind = Math.min(behindCount, 3);

  if (kind === "rebase") {
    svg.setAttribute("aria-label", "rebase 之後：本機的 commit 接在遠端內容之後，維持單線");
    let y = 14;
    line(20, 8, 20, height - 8, remote);
    for (let i = 0; i < shownAhead; i += 1) { dot(20, y, local); y += 18; }
    for (let i = 0; i < shownBehind; i += 1) { dot(20, y, remote); y += 18; }
  } else {
    svg.setAttribute("aria-label", "merge 之後：多出一個合併節點，歷史分成兩條再匯合");
    line(20, 14, 20, height - 8, remote);
    line(20, 22, 52, 40, local);
    line(52, 40, 52, height - 26, local);
    line(52, height - 26, 20, height - 8, local);
    dot(20, 14, remote, false);
    for (let i = 0; i < shownBehind; i += 1) dot(20, 40 + i * 18, remote);
    for (let i = 0; i < shownAhead; i += 1) dot(52, 40 + i * 18, local);
    dot(20, height - 8, remote);
  }
  return svg;
}

function fileList(paths, overlapping) {
  const list = document.createElement("ul");
  list.className = "file-list";
  const overlapSet = new Set(overlapping || []);
  const shown = paths.slice(0, 20);
  for (const path of shown) {
    const item = document.createElement("li");
    if (overlapSet.has(path)) {
      item.className = "overlap";
      item.textContent = `${path}　← 兩側都改到`;
    } else {
      item.textContent = path;
    }
    list.appendChild(item);
  }
  if (paths.length > shown.length) {
    const item = document.createElement("li");
    item.textContent = `… 另有 ${paths.length - shown.length} 個檔案`;
    list.appendChild(item);
  }
  return list;
}

function commitList(commits) {
  const list = document.createElement("ul");
  list.className = "commit-list";
  for (const commit of commits.slice(0, 15)) {
    const item = document.createElement("li");
    const oid = document.createElement("span");
    oid.className = "oid";
    oid.textContent = commit.short_oid;
    const message = document.createElement("span");
    message.className = "msg";
    message.textContent = commit.summary;
    item.append(oid, message);
    list.appendChild(item);
  }
  if (commits.length > 15) {
    const item = document.createElement("li");
    item.textContent = `… 另有 ${commits.length - 15} 個 commit`;
    list.appendChild(item);
  }
  return list;
}

function heading(text) {
  const node = document.createElement("div");
  node.className = "section-title";
  node.textContent = text;
  return node;
}

function renderDivergence(data) {
  const panel = el("tab-divergence");
  panel.replaceChildren();

  const verdict = document.createElement("div");
  verdict.className = "verdict";
  if (data.recommendation === "resolve-working-tree") verdict.classList.add("blocked");
  else if (data.risk === "possible") verdict.classList.add("risk");

  const title = document.createElement("h2");
  title.textContent = data.recommendation_headline;
  const detail = document.createElement("p");

  if (data.recommendation === "no-upstream") {
    detail.textContent = "這個分支沒有對應的遠端分支，因此無從比較。";
  } else if (data.recommendation === "up-to-date") {
    detail.textContent = `與 ${data.upstream} 完全一致，沒有待處理的事項。`;
  } else if (data.recommendation === "push") {
    detail.textContent = `本機有 ${data.ahead.length} 個 commit 還沒推上去，遠端沒有新內容，推送不會有衝突。`;
  } else if (data.recommendation === "fast-forward") {
    detail.textContent = `遠端有 ${data.behind.length} 個新 commit，本機沒有分岔，可以直接快轉，不會產生額外的 commit。`;
  } else if (data.recommendation === "resolve-working-tree") {
    detail.textContent =
      `即將進來的變更會碰到 ${data.uncommitted_overlap.length} 個你尚未提交的檔案。` +
      "先提交或暫存這些變更，否則可能遺失工作內容。";
  } else if (data.risk === "none") {
    detail.textContent =
      `本機 ${data.ahead.length} 個、遠端 ${data.behind.length} 個 commit，兩側沒有改到同一個檔案，` +
      "因此可以確定不會產生衝突。";
  } else {
    detail.textContent =
      `本機 ${data.ahead.length} 個、遠端 ${data.behind.length} 個 commit，` +
      `其中 ${data.overlapping_files.length} 個檔案兩側都改過，可能需要解衝突。` +
      "檔案層級的重疊只代表風險，不保證一定衝突。";
  }
  verdict.append(title, detail);
  panel.appendChild(verdict);

  const actions = syncActions(data);
  if (actions) panel.appendChild(actions);

  // 分岔時才需要比較兩種處置的結果。
  if (data.is_diverged) {
    const grid = document.createElement("div");
    grid.className = "outcome-grid";

    const rebase = document.createElement("div");
    rebase.className = "outcome recommended";
    const rebaseTitle = document.createElement("h3");
    rebaseTitle.textContent = "rebase";
    const badge = document.createElement("span");
    badge.className = "badge";
    badge.textContent = "建議";
    rebaseTitle.appendChild(badge);
    const rebaseText = document.createElement("p");
    rebaseText.textContent =
      `你的 ${data.ahead.length} 個 commit 會重新接到遠端內容之後，` +
      `歷史維持單線，共 ${data.commits_after_rebase} 個 commit。` +
      "這些 commit 只在你本機，重寫它們不會影響任何人。";
    rebase.append(rebaseTitle, outcomeDiagram("rebase", data.ahead.length, data.behind.length), rebaseText);

    const merge = document.createElement("div");
    merge.className = "outcome";
    const mergeTitle = document.createElement("h3");
    mergeTitle.textContent = "merge";
    const mergeText = document.createElement("p");
    mergeText.textContent =
      `會多產生一個合併節點，共 ${data.commits_after_merge} 個 commit，` +
      "歷史會出現分岔再匯合的形狀。保留原本的 commit 不被改寫。";
    merge.append(mergeTitle, outcomeDiagram("merge", data.ahead.length, data.behind.length), mergeText);

    grid.append(rebase, merge);
    panel.appendChild(grid);
  }

  if (data.uncommitted_overlap.length > 0) {
    panel.appendChild(heading("未提交、且會被進來的變更影響的檔案"));
    panel.appendChild(fileList(data.uncommitted_overlap, data.uncommitted_overlap));
  }
  if (data.behind.length > 0) {
    panel.appendChild(heading(`即將進來的 ${data.behind.length} 個 commit`));
    panel.appendChild(commitList(data.behind));
    panel.appendChild(heading("這些變更會碰到的檔案"));
    panel.appendChild(fileList(data.incoming_files, data.overlapping_files));
  }
  if (data.ahead.length > 0) {
    panel.appendChild(heading(`本機還沒推上去的 ${data.ahead.length} 個 commit`));
    panel.appendChild(commitList(data.ahead));
  }
}

/* ---------- 歷史圖 ---------- */

function renderGraph(data) {
  el("graph-meta").textContent =
    `${data.total_commits} 個 commit · ${data.lane_count} 條線道` +
    (data.truncated ? `（只載入前 ${data.commits.length} 個）` : "");

  const rows = el("graph-rows");
  rows.replaceChildren();

  const width = Math.max(1, Math.min(data.lane_count, 14)) * LANE_WIDTH + 12;

  for (const commit of data.commits) {
    const row = document.createElement("div");
    row.className = "commit-row";
    row.style.paddingLeft = `${width}px`;
    row.title = "點擊檢視這個 commit 改了什麼";
    row.addEventListener("click", () => showCommitDetail(commit.oid));

    const oid = document.createElement("span");
    oid.className = "oid";
    oid.textContent = commit.short_oid;
    row.appendChild(oid);

    for (const name of commit.refs) {
      const tag = document.createElement("span");
      tag.className = "ref-tag";
      tag.style.color = laneColour(commit.colour);
      tag.textContent = name;
      row.appendChild(tag);
    }

    const summary = document.createElement("span");
    summary.className = "summary";
    summary.textContent = commit.summary;
    const author = document.createElement("span");
    author.className = "author";
    author.textContent = commit.author;
    row.append(summary, author);
    rows.appendChild(row);
  }

  drawGraphCanvas(data, width);
}

function drawGraphCanvas(data, width) {
  const canvas = el("graph-canvas");
  const height = data.commits.length * ROW_HEIGHT;
  const ratio = window.devicePixelRatio || 1;

  canvas.width = width * ratio;
  canvas.height = height * ratio;
  canvas.style.width = `${width}px`;
  canvas.style.height = `${height}px`;

  const context = canvas.getContext("2d");
  context.scale(ratio, ratio);
  context.clearRect(0, 0, width, height);
  context.lineWidth = 2;

  const x = (lane) => 10 + Math.min(lane, 13) * LANE_WIDTH;
  const y = (row) => row * ROW_HEIGHT + ROW_HEIGHT / 2;

  // 先畫線再畫節點，節點才會蓋在線上面。
  for (const edge of data.edges) {
    context.strokeStyle = laneColour(edge.colour);
    context.beginPath();
    const startX = x(edge.child_lane);
    const startY = y(edge.child_row);
    const endX = x(edge.parent_lane);
    const endY = y(edge.parent_row);
    context.moveTo(startX, startY);
    if (startX === endX) {
      context.lineTo(endX, endY);
    } else {
      // 先斜向切到目標線道，再垂直往下，分支主體才會是直線。
      const turn = Math.min(startY + ROW_HEIGHT, endY);
      context.lineTo(endX, turn);
      context.lineTo(endX, endY);
    }
    context.stroke();
  }

  for (const commit of data.commits) {
    const colour = laneColour(commit.colour);
    context.beginPath();
    context.arc(x(commit.lane), y(commit.row), 4.5, 0, Math.PI * 2);
    // 合併節點畫成空心，形狀本身就能區分，不必只靠顏色。
    if (commit.is_merge) {
      context.fillStyle = getComputedStyle(document.documentElement)
        .getPropertyValue("--surface").trim() || "#fff";
      context.fill();
      context.strokeStyle = colour;
      context.stroke();
    } else {
      context.fillStyle = colour;
      context.fill();
    }
  }
}

/* ---------- 明細 ---------- */

function currentRepo() {
  return state.repos.find((repo) => repo.path === state.selected) || null;
}

async function renderDetail() {
  const repo = currentRepo();
  el("detail-empty").hidden = Boolean(repo);
  el("detail-body").hidden = !repo;
  if (!repo) return;

  el("detail-name").textContent = repo.name;
  el("detail-path").textContent = repo.path;
  renderChips(el("detail-chips"), repo);

  const divergencePanel = el("tab-divergence");
  if (repo.error) {
    divergencePanel.replaceChildren();
    const box = document.createElement("div");
    box.className = "error-box";
    box.textContent = repo.error;
    divergencePanel.appendChild(box);
    return;
  }

  updateConflictBanner();

  if (state.tab === "divergence") {
    try {
      renderDivergence(await invoke("repo_divergence", { path: repo.path }));
    } catch (error) {
      divergencePanel.replaceChildren();
      const box = document.createElement("div");
      box.className = "error-box";
      box.textContent = String(error);
      divergencePanel.appendChild(box);
    }
  } else if (state.tab === "workspace") {
    if (state.diffSource !== "commit") await loadDiff();
    renderWorkspace();
  } else if (state.tab === "conflicts") {
    renderConflicts();
  } else {
    try {
      renderGraph(await invoke("repo_graph", { path: repo.path, limit: 500 }));
    } catch (error) {
      el("graph-meta").textContent = String(error);
    }
  }
}

/** 有衝突或有進行中的操作時，在最上方明確提示並提供入口。 */
function updateConflictBanner() {
  const banner = el("conflict-banner");
  const tab = document.querySelector('.tab[data-tab="conflicts"]');
  const data = state.workspace;
  const conflicts = data ? data.conflicts.length : 0;
  const operation = data ? data.operation : null;

  const needsAttention = conflicts > 0 || Boolean(operation);
  banner.hidden = !needsAttention;
  tab.hidden = !needsAttention;
  if (!needsAttention) return;

  banner.replaceChildren();
  const text = document.createElement("span");
  text.innerHTML = "";
  const strong = document.createElement("strong");
  strong.textContent = operation || "有未解決的衝突";
  text.append(strong);
  if (conflicts > 0) {
    text.append(`　還有 ${conflicts} 個檔案的衝突要處理。`);
  } else {
    text.append("　衝突都解決了，可以繼續。");
  }
  banner.append(text, button("前往處理", () => switchTab("conflicts"), "primary"));
}

async function selectRepo(path) {
  state.selected = path;
  state.selectedConflict = null;
  state.selectedFile = null;
  state.diffFiles = [];
  state.selectedLines.clear();
  state.commitDetail = null;
  state.diffSource = "unstaged";
  renderRepoList();
  await loadWorkspace();
  await renderDetail();
}

function switchTab(tab) {
  state.tab = tab;
  for (const button of document.querySelectorAll(".tab")) {
    button.classList.toggle("active", button.dataset.tab === tab);
  }
  el("tab-divergence").hidden = tab !== "divergence";
  el("tab-graph").hidden = tab !== "graph";
  renderDetail();
}

/* ---------- 資料流 ---------- */

function applyRepos(repos) {
  state.repos = repos;
  if (state.selected && !repos.some((repo) => repo.path === state.selected)) {
    state.selected = null;
  }
  renderRepoList();
  renderDetail();
}

async function refresh() {
  applyRepos(await invoke("list_repos"));
}

async function loadSettings() {
  const settings = await invoke("get_settings");
  el("fetch-interval").value = String(settings.fetch_interval_secs);
  el("background-fetch").checked = settings.background_fetch;
  el("notify-incoming").checked = settings.notify_incoming;
}

async function pushSettings() {
  await invoke("update_settings", {
    fetchIntervalSecs: Number(el("fetch-interval").value),
    backgroundFetch: el("background-fetch").checked,
    notifyIncoming: el("notify-incoming").checked,
  });
  setStatus("設定已儲存");
}

/* ---------- 啟動 ---------- */

function wireEvents() {
  el("add-repo").addEventListener("click", async () => {
    let chosen = await pickDirectory("選擇 repository");
    if (chosen === null) {
      // 沒有系統對話框可用時，退回手動輸入路徑，功能不會因此不可用。
      chosen = window.prompt("輸入 repository 的路徑");
    }
    if (!chosen) return;
    try {
      applyRepos(await invoke("add_repo", { path: chosen }));
      setStatus("已加入");
    } catch (error) {
      setStatus(String(error));
    }
  });

  el("fetch-all").addEventListener("click", async () => {
    setStatus("檢查中…");
    applyRepos(await invoke("fetch_all"));
    setStatus(`已於 ${new Date().toLocaleTimeString()} 檢查完畢`);
  });

  el("fetch-one").addEventListener("click", async () => {
    const repo = currentRepo();
    if (!repo) return;
    setStatus("檢查中…");
    const updated = await invoke("fetch_repo", { path: repo.path });
    state.repos = state.repos.map((item) => (item.path === updated.path ? updated : item));
    renderRepoList();
    await renderDetail();
    setStatus(updated.fetch_state ? updated.fetch_state.message : "已檢查");
  });

  el("remove-repo").addEventListener("click", async () => {
    const repo = currentRepo();
    if (!repo) return;
    applyRepos(await invoke("remove_repo", { path: repo.path }));
    setStatus("已從清單移除（不會刪除任何檔案）");
  });

  el("toggle-settings").addEventListener("click", () => {
    const panel = el("settings-panel");
    panel.hidden = !panel.hidden;
  });

  for (const input of ["fetch-interval", "background-fetch", "notify-incoming"]) {
    el(input).addEventListener("change", pushSettings);
  }

  for (const button of document.querySelectorAll(".tab")) {
    button.addEventListener("click", () => switchTab(button.dataset.tab));
  }
}

async function main() {
  if (!invoke) {
    fatal("找不到 Tauri 的 invoke API。\n請確認 tauri.conf.json 的 app.withGlobalTauri 為 true。");
    return;
  }
  wireEvents();
  await loadSettings();
  await refresh();
  await loadWorkspace();
  await renderDetail();

  // 背景排程完成一輪後會送出更新。
  if (listen) {
    await listen("repos-updated", (event) => {
      applyRepos(event.payload);
      setStatus(`背景檢查完成 ${new Date().toLocaleTimeString()}`);
    });
  }
}

main().catch((error) => {
  // 啟動失敗要看得見；靜默失敗只會留下空白視窗。
  fatal(error && error.stack ? error.stack : String(error));
});

/* ---------- 確認與執行 ---------- */

/** 會消滅工作成果的操作一律先問過。回傳使用者是否確定。 */
function confirmAction(title, detail) {
  return new Promise((resolve) => {
    const backdrop = el("confirm-backdrop");
    el("confirm-title").textContent = title;
    el("confirm-detail").textContent = detail;
    backdrop.hidden = false;
    el("confirm-ok").focus();

    const finish = (answer) => {
      backdrop.hidden = true;
      el("confirm-ok").onclick = null;
      el("confirm-cancel").onclick = null;
      document.removeEventListener("keydown", onKey);
      resolve(answer);
    };
    const onKey = (event) => {
      if (event.key === "Escape") finish(false);
    };
    el("confirm-ok").onclick = () => finish(true);
    el("confirm-cancel").onclick = () => finish(false);
    document.addEventListener("keydown", onKey);
  });
}

/**
 * 執行一個會改動 repository 的指令。
 *
 * `confirm` 有值時先取得確認。成功後重新載入狀態，讓畫面反映實際結果
 * 而不是樂觀更新 —— 這類操作出錯的後果太大，不該顯示未經確認的狀態。
 */
async function runOp(command, args, confirm) {
  if (confirm && !(await confirmAction(confirm.title, confirm.detail))) return null;
  setStatus("執行中…");
  try {
    const outcome = await invoke(command, args);
    setStatus(outcome.message);
    await refresh();
    await loadWorkspace();
    await renderDetail();
    return outcome;
  } catch (error) {
    setStatus(String(error));
    return null;
  }
}

async function loadWorkspace() {
  const repo = currentRepo();
  if (!repo || repo.error) {
    state.workspace = null;
    return;
  }
  try {
    state.workspace = await invoke("repo_workspace", { path: repo.path });
  } catch (error) {
    state.workspace = null;
    setStatus(String(error));
  }
}

/* ---------- 同步狀態的操作按鈕 ---------- */

function button(label, onClick, className) {
  const node = document.createElement("button");
  node.type = "button";
  node.textContent = label;
  if (className) node.className = className;
  node.addEventListener("click", onClick);
  return node;
}

function syncActions(data) {
  const repo = currentRepo();
  const bar = document.createElement("div");
  bar.className = "action-bar";
  const path = repo.path;
  const dirty = repo.working_tree.total > 0;
  const blocked = dirty && (data.recommendation === "rebase" || data.recommendation === "fast-forward");

  if (data.recommendation === "fast-forward") {
    bar.appendChild(button("拉取（快轉）", () => runOp("op_fast_forward", { path }), "primary"));
  }
  if (data.is_diverged) {
    bar.appendChild(
      button("用 rebase 整合", () =>
        runOp("op_rebase", { path }, {
          title: "以 rebase 整合",
          detail: "你的 commit 會被重新套用到遠端內容之後，識別碼會改變。操作前會自動建立還原點。",
        }), "primary")
    );
    bar.appendChild(
      button("用 merge 整合", () =>
        runOp("op_merge", { path }, {
          title: "以 merge 整合",
          detail: "會產生一個合併節點，原本的 commit 不被改寫。操作前會自動建立還原點。",
        }))
    );
  }
  if (data.ahead.length > 0 && data.behind.length === 0) {
    bar.appendChild(button("推送", () => runOp("op_push", { path }), "primary"));
  } else if (data.ahead.length > 0) {
    bar.appendChild(
      button("強制推送", () =>
        runOp("op_push", { path, force: true }, {
          title: "強制推送",
          detail: "會覆寫遠端的分支。程式會先確認遠端沒有被別人推過，若有則中止。",
        }), "danger")
    );
  }
  if (blocked) {
    bar.appendChild(
      button("先把變更暫存起來", () => runOp("op_stash_save", { path, message: "整合前自動暫存" }))
    );
  }

  const undoPoints = (state.workspace && state.workspace.undo_points) || [];
  if (undoPoints.length > 0) {
    const latest = undoPoints[0];
    const undo = button(`還原上一步（${latest.operation}）`, () =>
      runOp("op_undo", { path, reference: latest.reference }, {
        title: "還原到操作前",
        detail: `會把分支與工作目錄重設回「${latest.operation}」之前的狀態，目前未提交的變更會遺失。`,
      }), "ghost");
    undo.classList.add("spacer");
    bar.appendChild(undo);
  }

  if (bar.childElementCount === 0) return null;
  if (blocked) {
    const note = document.createElement("div");
    note.className = "action-note";
    note.textContent = "工作目錄有未提交的變更，整合前需要先提交或暫存。";
    bar.appendChild(note);
  }
  return bar;
}

/* ---------- 變更與提交 ---------- */

function renderWorkspace() {
  const panel = el("tab-workspace");
  panel.replaceChildren();
  const repo = currentRepo();
  const data = state.workspace;
  if (!repo || !data) {
    panel.textContent = "沒有可顯示的資料";
    return;
  }
  const path = repo.path;

  // 分支列
  const bar = document.createElement("div");
  bar.className = "workspace-bar";
  const select = document.createElement("select");
  for (const branch of data.branches.filter((item) => !item.is_remote)) {
    const option = document.createElement("option");
    option.value = branch.name;
    option.textContent = branch.name;
    option.selected = branch.is_head;
    select.appendChild(option);
  }
  select.addEventListener("change", () =>
    runOp("op_checkout", { path, name: select.value })
  );
  const newName = document.createElement("input");
  newName.placeholder = "新分支名稱";
  bar.append("分支", select, newName,
    button("建立並切換", () => {
      if (!newName.value.trim()) return setStatus("請先輸入分支名稱");
      return runOp("op_create_branch", { path, name: newName.value.trim() });
    }));
  panel.appendChild(bar);

  // 變更清單
  panel.appendChild(heading(`工作目錄的變更（${data.changes.length}）`));
  if (data.changes.length === 0) {
    const empty = document.createElement("p");
    empty.className = "action-note";
    empty.textContent = "沒有任何變更。";
    panel.appendChild(empty);
  } else {
    const list = document.createElement("div");
    list.className = "change-list";
    for (const change of data.changes) {
      const row = document.createElement("div");
      row.className = "change-row";

      const kind = document.createElement("span");
      const label = change.is_conflicted
        ? "conflict"
        : change.staged !== "none"
          ? change.staged
          : change.unstaged;
      kind.className = `kind ${label}`;
      kind.textContent = { new: "新增", modified: "修改", deleted: "刪除", renamed: "改名", conflict: "衝突" }[label] || label;

      const pathNode = document.createElement("span");
      pathNode.className = "path";
      pathNode.textContent = change.path;
      row.append(kind, pathNode);

      if (!change.is_conflicted) {
        if (change.staged === "none") {
          row.appendChild(button("暫存", () => runOp("op_stage", { path, paths: [change.path] })));
        } else {
          row.appendChild(button("取消暫存", () => runOp("op_unstage", { path, paths: [change.path] })));
        }
        if (!change.is_untracked) {
          row.appendChild(
            button("丟棄", () =>
              runOp("op_discard", { path, paths: [change.path] }, {
                title: `丟棄 ${change.path} 的變更`,
                detail: "尚未提交的編輯會永久消失，git 無法還原。",
              }), "ghost danger")
          );
        }
      }
      list.appendChild(row);
    }
    panel.appendChild(list);

    const bulk = document.createElement("div");
    bulk.className = "action-bar";
    bulk.append(
      button("暫存全部", () => runOp("op_stage", { path, paths: [] })),
      button("取消暫存全部", () => runOp("op_unstage", { path, paths: [] }))
    );
    panel.appendChild(bulk);
  }

  // 差異
  panel.appendChild(heading("差異"));
  const sourceBar = document.createElement("div");
  sourceBar.className = "action-bar";
  for (const [value, label] of [["unstaged", "未暫存"], ["staged", "已暫存"]]) {
    const item = button(label, async () => {
      state.diffSource = value;
      state.selectedLines.clear();
      state.commitDetail = null;
      await loadDiff();
      renderWorkspace();
    }, state.diffSource === value ? "primary" : undefined);
    sourceBar.appendChild(item);
  }
  if (state.commitDetail) {
    const info = document.createElement("span");
    info.className = "action-note";
    info.style.flexBasis = "auto";
    info.textContent = `正在看 commit ${state.commitDetail.short_oid}：${state.commitDetail.summary}`;
    sourceBar.appendChild(info);
  }
  panel.appendChild(sourceBar);

  const diffLayout = document.createElement("div");
  diffLayout.className = "diff-layout";
  const fileColumn = document.createElement("div");
  fileColumn.className = "conflict-files";
  for (const item of state.diffFiles) {
    const entry = button(`${item.path}  +${item.added} −${item.removed}`, () => {
      state.selectedFile = item.path;
      state.selectedLines.clear();
      renderDiffPanel();
      for (const node of fileColumn.children) node.classList.remove("selected");
      entry.classList.add("selected");
    });
    if (item.path === state.selectedFile) entry.classList.add("selected");
    fileColumn.appendChild(entry);
  }
  const diffHolder = document.createElement("div");
  diffHolder.id = "diff-panel";
  diffLayout.append(fileColumn, diffHolder);
  panel.appendChild(diffLayout);
  renderDiffPanel();

  // 提交
  panel.appendChild(heading("提交"));
  const box = document.createElement("div");
  box.className = "commit-box";
  const message = document.createElement("textarea");
  message.placeholder = "這次改了什麼？";
  const actions = document.createElement("div");
  actions.className = "commit-actions";
  const amendLabel = document.createElement("label");
  const amend = document.createElement("input");
  amend.type = "checkbox";
  amendLabel.append(amend, "改寫前一個 commit");
  actions.append(
    button("提交", async () => {
      const outcome = await runOp("op_commit", {
        path,
        message: message.value,
        amend: amend.checked,
      }, amend.checked ? {
        title: "改寫前一個 commit",
        detail: "識別碼會改變。如果它已經推送出去，其他人會看到歷史不一致。",
      } : undefined);
      if (outcome) message.value = "";
    }, "primary"),
    amendLabel
  );
  box.append(message, actions);
  panel.appendChild(box);

  // Stash
  panel.appendChild(heading(`暫存（${data.stashes.length}）`));
  const stashBar = document.createElement("div");
  stashBar.className = "action-bar";
  stashBar.appendChild(button("把目前的變更暫存起來", () => runOp("op_stash_save", { path, message: "" })));
  panel.appendChild(stashBar);
  if (data.stashes.length > 0) {
    const list = document.createElement("div");
    list.className = "change-list";
    for (const stash of data.stashes) {
      const row = document.createElement("div");
      row.className = "change-row";
      const text = document.createElement("span");
      text.className = "path";
      text.textContent = stash.message;
      row.append(text,
        button("取出", () => runOp("op_stash_pop", { path, index: stash.index })),
        button("刪除", () =>
          runOp("op_stash_drop", { path, index: stash.index }, {
            title: "刪除這筆暫存",
            detail: "裡面的內容會永久消失。",
          }), "ghost danger"));
      list.appendChild(row);
    }
    panel.appendChild(list);
  }
}

/* ---------- 解決衝突 ---------- */

function sidePane(title, side, onUse) {
  const pane = document.createElement("div");
  pane.className = "side-pane";
  const header = document.createElement("h4");
  header.textContent = title;
  if (onUse) header.appendChild(button("採用這一方", onUse));
  const body = document.createElement("pre");
  body.textContent = side.exists
    ? (side.text === null ? "（二進位檔案）" : side.text)
    : "（這一方刪除了這個檔案）";
  pane.append(header, body);
  return pane;
}

function renderConflicts() {
  const panel = el("tab-conflicts");
  panel.replaceChildren();
  const repo = currentRepo();
  const data = state.workspace;
  if (!repo || !data) return;
  const path = repo.path;
  const files = data.conflicts;

  const bar = document.createElement("div");
  bar.className = "action-bar";
  if (files.length === 0) {
    bar.appendChild(button("全部解決了，繼續", () => runOp("op_continue", { path }), "primary"));
  }
  bar.appendChild(
    button("中止，回到操作前", () =>
      runOp("op_abort", { path }, {
        title: "中止這次操作",
        detail: "會回到操作開始前的狀態，這段期間解掉的衝突會白費。",
      }), "ghost danger")
  );
  if (data.operation && data.operation.includes("rebase")) {
    bar.appendChild(
      button("略過這個 commit", () =>
        runOp("op_skip_step", { path }, {
          title: "略過這一個 commit",
          detail: "這個 commit 的變更不會被套用，等於捨棄它。",
        }), "ghost")
    );
  }
  panel.appendChild(bar);

  if (files.length === 0) {
    const done = document.createElement("p");
    done.className = "action-note";
    done.textContent = "沒有未解決的衝突了。";
    panel.appendChild(done);
    return;
  }

  const layout = document.createElement("div");
  layout.className = "conflict-layout";

  const fileList = document.createElement("div");
  fileList.className = "conflict-files";
  if (!files.some((file) => file.path === state.selectedConflict)) {
    state.selectedConflict = files[0].path;
  }
  for (const file of files) {
    const item = button(file.path, () => {
      state.selectedConflict = file.path;
      renderConflicts();
    });
    if (file.path === state.selectedConflict) item.classList.add("selected");
    fileList.appendChild(item);
  }

  const detail = document.createElement("div");
  const current = files.find((file) => file.path === state.selectedConflict);

  const sides = document.createElement("div");
  sides.className = "side-by-side";
  sides.append(
    sidePane("我的版本", current.ours, () =>
      runOp("op_resolve_conflict", { path, file: current.path, side: "ours" })),
    sidePane("他們的版本", current.theirs, () =>
      runOp("op_resolve_conflict", { path, file: current.path, side: "theirs" }))
  );
  detail.appendChild(sides);

  if (current.is_binary) {
    const note = document.createElement("p");
    note.className = "action-note";
    note.textContent = "二進位檔案無法逐行編輯，請整檔擇一。";
    detail.appendChild(note);
  } else {
    detail.appendChild(heading("合併後的內容（可直接編輯，含 <<<< 標記的地方要處理掉）"));
    const editor = document.createElement("textarea");
    editor.className = "merged-editor";
    editor.value = current.merged || "";
    const save = document.createElement("div");
    save.className = "action-bar";
    save.appendChild(
      button("以這個內容標記為已解決", () =>
        runOp("op_resolve_conflict", { path, file: current.path, content: editor.value }), "primary")
    );
    detail.append(editor, save);
  }

  layout.append(fileList, detail);
  panel.appendChild(layout);
}

/* ---------- 差異呈現 ---------- */

/** 把一行切成純文字與需要標示的片段。以位元組位移切割，需轉成字元索引。 */
function segmentsOf(line) {
  if (!line.spans || line.spans.length === 0) return [{ text: line.content, marked: false }];
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  const bytes = encoder.encode(line.content);
  const parts = [];
  let cursor = 0;
  for (const span of line.spans) {
    const start = Math.max(0, Math.min(span.start, bytes.length));
    const end = Math.max(start, Math.min(span.end, bytes.length));
    if (start > cursor) parts.push({ text: decoder.decode(bytes.slice(cursor, start)), marked: false });
    parts.push({ text: decoder.decode(bytes.slice(start, end)), marked: true });
    cursor = end;
  }
  if (cursor < bytes.length) parts.push({ text: decoder.decode(bytes.slice(cursor)), marked: false });
  return parts;
}

function lineContent(line) {
  const holder = document.createElement("span");
  holder.className = "code";
  for (const part of segmentsOf(line)) {
    if (part.marked) {
      const mark = document.createElement("mark");
      mark.textContent = part.text;
      holder.appendChild(mark);
    } else {
      holder.appendChild(document.createTextNode(part.text));
    }
  }
  return holder;
}

function lineKey(hunkIndex, lineIndex) {
  return `${hunkIndex}:${lineIndex}`;
}

function toggleLine(hunkIndex, lineIndex) {
  const key = lineKey(hunkIndex, lineIndex);
  if (state.selectedLines.has(key)) state.selectedLines.delete(key);
  else state.selectedLines.add(key);
  renderDiffPanel();
}

/** 行內模式：一欄，以顏色與前綴區分增刪。 */
function inlineRows(file, hunkIndex, hunk) {
  const rows = document.createDocumentFragment();
  hunk.lines.forEach((line, lineIndex) => {
    const row = document.createElement("div");
    row.className = `diff-row ${line.kind}`;
    if (line.whitespace_only) row.classList.add("ws-only");
    const selectable = line.kind !== "context";
    if (selectable) {
      row.classList.add("selectable");
      if (state.selectedLines.has(lineKey(hunkIndex, lineIndex))) row.classList.add("selected");
      row.addEventListener("click", () => toggleLine(hunkIndex, lineIndex));
    }
    const oldNo = document.createElement("span");
    oldNo.className = "lineno";
    oldNo.textContent = line.old_lineno ?? "";
    const newNo = document.createElement("span");
    newNo.className = "lineno";
    newNo.textContent = line.new_lineno ?? "";
    const sign = document.createElement("span");
    sign.className = "sign";
    sign.textContent = line.kind === "added" ? "+" : line.kind === "removed" ? "−" : " ";
    row.append(oldNo, newNo, sign, lineContent(line));
    rows.appendChild(row);
  });
  return rows;
}

/** 並排模式：左舊右新，成對的增刪對齊在同一列。 */
function splitRows(file, hunkIndex, hunk) {
  const rows = document.createDocumentFragment();
  let index = 0;
  const lines = hunk.lines;

  const cell = (line, lineIndex, side) => {
    const box = document.createElement("div");
    box.className = `diff-cell ${line ? line.kind : "empty"}`;
    if (!line) return box;
    if (line.whitespace_only) box.classList.add("ws-only");
    if (line.kind !== "context") {
      box.classList.add("selectable");
      if (state.selectedLines.has(lineKey(hunkIndex, lineIndex))) box.classList.add("selected");
      box.addEventListener("click", () => toggleLine(hunkIndex, lineIndex));
    }
    const no = document.createElement("span");
    no.className = "lineno";
    no.textContent = (side === "old" ? line.old_lineno : line.new_lineno) ?? "";
    box.append(no, lineContent(line));
    return box;
  };

  while (index < lines.length) {
    const line = lines[index];
    if (line.kind === "context") {
      const row = document.createElement("div");
      row.className = "diff-split-row";
      row.append(cell(line, index, "old"), cell(line, index, "new"));
      rows.appendChild(row);
      index += 1;
      continue;
    }
    // 收集一組連續的刪除與新增，兩兩對齊。
    const removedStart = index;
    while (index < lines.length && lines[index].kind === "removed") index += 1;
    const addedStart = index;
    while (index < lines.length && lines[index].kind === "added") index += 1;
    const removed = addedStart - removedStart;
    const added = index - addedStart;

    for (let offset = 0; offset < Math.max(removed, added); offset += 1) {
      const row = document.createElement("div");
      row.className = "diff-split-row";
      const leftIndex = offset < removed ? removedStart + offset : null;
      const rightIndex = offset < added ? addedStart + offset : null;
      row.append(
        leftIndex === null ? cell(null) : cell(lines[leftIndex], leftIndex, "old"),
        rightIndex === null ? cell(null) : cell(lines[rightIndex], rightIndex, "new")
      );
      rows.appendChild(row);
    }
  }
  return rows;
}

function selectionForStaging() {
  return [...state.selectedLines].map((key) => key.split(":").map(Number));
}

function renderDiffPanel() {
  const holder = el("diff-panel");
  if (!holder) return;
  holder.replaceChildren();

  const repo = currentRepo();
  const file = (state.diffFiles || []).find((item) => item.path === state.selectedFile);
  if (!repo || !file) {
    const hint = document.createElement("p");
    hint.className = "action-note";
    hint.textContent = "選擇左側的檔案檢視差異";
    holder.appendChild(hint);
    return;
  }

  const bar = document.createElement("div");
  bar.className = "diff-toolbar";
  const title = document.createElement("span");
  title.className = "diff-title";
  title.textContent = file.old_path ? `${file.old_path} → ${file.path}` : file.path;
  const counts = document.createElement("span");
  counts.className = "diff-counts";
  counts.textContent = `+${file.added} −${file.removed}`;
  const modeButton = button(
    state.diffMode === "split" ? "改為行內顯示" : "改為並排顯示",
    () => {
      state.diffMode = state.diffMode === "split" ? "inline" : "split";
      renderDiffPanel();
    }
  );
  const historyButton = button("這個檔案的歷史", () => showFileHistory(file.path));
  bar.append(title, counts, modeButton, historyButton);
  holder.appendChild(bar);

  if (state.selectedLines.size > 0) {
    const selBar = document.createElement("div");
    selBar.className = "action-bar";
    const label = document.createElement("span");
    label.className = "action-note";
    label.style.flexBasis = "auto";
    label.textContent = `已選取 ${state.selectedLines.size} 行`;
    selBar.append(
      label,
      button(state.diffSource === "staged" ? "取消暫存選取的行" : "暫存選取的行", async () => {
        const command = state.diffSource === "staged" ? "op_unstage_selection" : "op_stage_selection";
        const done = await runOp(command, {
          path: repo.path,
          file: file.path,
          selection: selectionForStaging(),
        });
        if (done) state.selectedLines.clear();
      }, "primary"),
      button("清除選取", () => {
        state.selectedLines.clear();
        renderDiffPanel();
      }, "ghost")
    );
    holder.appendChild(selBar);
  }

  if (file.is_binary) {
    const note = document.createElement("p");
    note.className = "action-note";
    note.textContent = "二進位檔案，無法顯示逐行差異。";
    holder.appendChild(note);
    return;
  }

  file.hunks.forEach((hunk, hunkIndex) => {
    const block = document.createElement("div");
    block.className = "hunk";
    if (hunk.collides_with_incoming) block.classList.add("collides");

    const header = document.createElement("div");
    header.className = "hunk-header";
    const text = document.createElement("span");
    text.textContent = hunk.header || `@@ -${hunk.old_start} +${hunk.new_start} @@`;
    header.appendChild(text);

    if (hunk.collides_with_incoming) {
      const warn = document.createElement("span");
      warn.className = "collide-tag";
      warn.textContent = "即將進來的變更也會改到這一段";
      header.appendChild(warn);
    }
    if (hunk.whitespace_only) {
      const tag = document.createElement("span");
      tag.className = "ws-tag";
      tag.textContent = "只有空白差異";
      header.appendChild(tag);
    }
    header.appendChild(
      button(state.diffSource === "staged" ? "取消暫存這一段" : "暫存這一段", () => {
        const selection = [];
        hunk.lines.forEach((line, lineIndex) => {
          if (line.kind !== "context") selection.push([hunkIndex, lineIndex]);
        });
        const command = state.diffSource === "staged" ? "op_unstage_selection" : "op_stage_selection";
        return runOp(command, { path: repo.path, file: file.path, selection });
      })
    );
    block.appendChild(header);

    const body = document.createElement("div");
    body.className = `diff-body ${state.diffMode}`;
    body.appendChild(
      state.diffMode === "split" ? splitRows(file, hunkIndex, hunk) : inlineRows(file, hunkIndex, hunk)
    );
    block.appendChild(body);
    holder.appendChild(block);
  });
}

async function loadDiff() {
  const repo = currentRepo();
  if (!repo) return;
  try {
    state.diffFiles = await invoke("repo_diff", { path: repo.path, source: state.diffSource });
    if (!state.diffFiles.some((file) => file.path === state.selectedFile)) {
      state.selectedFile = state.diffFiles.length > 0 ? state.diffFiles[0].path : null;
    }
  } catch (error) {
    state.diffFiles = [];
    setStatus(String(error));
  }
}

async function showFileHistory(file) {
  const repo = currentRepo();
  if (!repo) return;
  try {
    const oids = await invoke("file_history", { path: repo.path, file, limit: 30 });
    const holder = el("diff-panel");
    holder.replaceChildren();
    const back = document.createElement("div");
    back.className = "action-bar";
    back.append(button("← 回到差異", () => renderDiffPanel()));
    holder.appendChild(back);
    holder.appendChild(heading(`${file} 的變更歷史（${oids.length}）`));

    const list = document.createElement("div");
    list.className = "change-list";
    for (const oid of oids) {
      const detail = await invoke("commit_detail", { path: repo.path, oid });
      const row = document.createElement("div");
      row.className = "change-row";
      const id = document.createElement("span");
      id.className = "oid";
      id.textContent = detail.short_oid;
      const summary = document.createElement("span");
      summary.className = "path";
      summary.textContent = detail.summary;
      row.append(id, summary, button("看這次的變更", () => showCommitDetail(oid)));
      list.appendChild(row);
    }
    holder.appendChild(list);
  } catch (error) {
    setStatus(String(error));
  }
}

/* ---------- Commit 詳情 ---------- */

async function showCommitDetail(oid) {
  const repo = currentRepo();
  if (!repo) return;
  try {
    const detail = await invoke("commit_detail", { path: repo.path, oid });
    state.diffFiles = detail.files;
    state.selectedFile = detail.files.length > 0 ? detail.files[0].path : null;
    state.selectedLines.clear();
    state.diffSource = "commit";
    state.commitDetail = detail;
    switchTab("workspace");
  } catch (error) {
    setStatus(String(error));
  }
}
