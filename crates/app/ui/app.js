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
  } else {
    try {
      renderGraph(await invoke("repo_graph", { path: repo.path, limit: 500 }));
    } catch (error) {
      el("graph-meta").textContent = String(error);
    }
  }
}

function selectRepo(path) {
  state.selected = path;
  renderRepoList();
  renderDetail();
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
