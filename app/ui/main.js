"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const ROW = 20; // px, must match --row in style.css
// Browsers cap element heights (WebKit around 33 million px). Above this the
// scrollbar maps proportionally onto the line range instead of 1 row = ROW px.
const MAX_SPACER = 8_000_000;

const el = (id) => document.getElementById(id);
const viewport = el("viewport");
const spacer = el("spacer");
const rows = el("rows");

let info = null; // { path, bytes, lines, index_ms } of the open file
let first = 0; // zero-based number of the top visible line
let lastHit = null; // line of the most recent search hit
let searching = false; // a find_next call is in flight
let request = 0; // sequence number, drops stale get_lines responses
let expectedTop = -1; // scrollTop we set ourselves, to tell it from user scrolling

const visibleRows = () => Math.max(1, Math.floor(viewport.clientHeight / ROW));
const maxFirst = () => (info ? Math.max(0, info.lines - visibleRows()) : 0);
const scrollRange = () => Math.max(0, spacer.offsetHeight - viewport.clientHeight);
const fmt = (n) => n.toLocaleString("en-US");

function formatBytes(n) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i += 1;
  }
  return `${n.toFixed(i ? 1 : 0)} ${units[i]}`;
}

function setMessage(text) {
  el("message").textContent = text;
}

function appendHighlighted(parent, text, query) {
  if (!query) {
    parent.append(text);
    return;
  }
  let from = 0;
  for (let at = text.indexOf(query); at !== -1; at = text.indexOf(query, from)) {
    parent.append(text.slice(from, at));
    const mark = document.createElement("mark");
    mark.textContent = query;
    parent.append(mark);
    from = at + query.length;
  }
  parent.append(text.slice(from));
}

async function render() {
  if (!info) return;
  const mine = ++request;
  const count = visibleRows() + 1;
  const lines = await invoke("get_lines", { start: first, count });
  if (mine !== request) return; // a newer render started while this one waited

  const query = el("query").value;
  const width = String(info.lines).length;
  const fragment = document.createDocumentFragment();
  lines.forEach((line, i) => {
    const n = first + i;
    const row = document.createElement("div");
    row.className = n === lastHit ? "row hit" : "row";
    const num = document.createElement("span");
    num.className = "num";
    num.textContent = String(n + 1).padStart(width, " ");
    const body = document.createElement("span");
    appendHighlighted(body, line.text, query);
    if (line.truncated) {
      const more = document.createElement("span");
      more.className = "more";
      more.textContent = " ...";
      body.append(more);
    }
    row.append(num, body);
    fragment.append(row);
  });
  rows.replaceChildren(fragment);
  el("stats").textContent =
    `line ${fmt(first + 1)} of ${fmt(info.lines)} | ${formatBytes(info.bytes)} | indexed in ${fmt(info.index_ms)} ms`;
}

function goTo(line) {
  first = Math.min(Math.max(0, Math.round(line)), maxFirst());
  const range = scrollRange();
  const max = maxFirst();
  expectedTop = max > 0 ? Math.round((first / max) * range) : 0;
  viewport.scrollTop = expectedTop;
  render();
}

viewport.addEventListener("scroll", () => {
  // Ignore the echo of our own scrollTop assignment; in proportional mode it
  // would round back to a different line and make the view jitter.
  if (Math.abs(viewport.scrollTop - expectedTop) < 1) return;
  expectedTop = -1;
  const range = scrollRange();
  first = range > 0 ? Math.round((viewport.scrollTop / range) * maxFirst()) : 0;
  render();
});

viewport.addEventListener(
  "wheel",
  (event) => {
    if (!info || event.ctrlKey) return;
    // Scroll by lines ourselves: with a scaled scrollbar one native wheel
    // tick would jump thousands of lines in a large file.
    event.preventDefault();
    const step = Math.sign(event.deltaY) * Math.max(1, Math.round(Math.abs(event.deltaY) / ROW));
    goTo(first + step);
  },
  { passive: false },
);

viewport.addEventListener("keydown", (event) => {
  if (!info) return;
  const page = visibleRows() - 1;
  const moves = {
    ArrowDown: first + 1,
    ArrowUp: first - 1,
    PageDown: first + page,
    PageUp: first - page,
    Home: 0,
    End: maxFirst(),
  };
  if (event.key in moves) {
    event.preventDefault();
    goTo(moves[event.key]);
  }
});

window.addEventListener("resize", () => goTo(first));

window.addEventListener("keydown", (event) => {
  if (!event.ctrlKey && !event.metaKey) return;
  const target = { f: "query", g: "goto" }[event.key.toLowerCase()];
  if (!target) return;
  event.preventDefault();
  el(target).focus();
  el(target).select();
});

async function openPath(path) {
  setMessage("Indexing...");
  try {
    info = await invoke("open_file", { path });
  } catch (error) {
    setMessage(String(error));
    return;
  }
  lastHit = null;
  el("empty").hidden = true;
  el("path").textContent = info.path;
  el("goto").max = String(Math.max(1, info.lines));
  spacer.style.height = `${Math.min(info.lines * ROW, MAX_SPACER)}px`;
  setMessage("");
  goTo(0);
  viewport.focus();
}

el("open").addEventListener("click", async () => {
  const path = await invoke("plugin:dialog|open", { options: { multiple: false, directory: false } });
  if (path) openPath(path);
});

el("query").addEventListener("keydown", async (event) => {
  // One search at a time: a second Enter while the first is still scanning
  // would start from the same line and report the same match again.
  if (event.key !== "Enter" || !info || searching) return;
  const query = event.target.value;
  if (!query) return;
  const from = lastHit === null ? first : lastHit + 1;
  setMessage("Searching...");
  searching = true;
  let hit;
  try {
    hit = await invoke("find_next", { query, fromLine: from });
  } finally {
    searching = false;
  }
  if (hit === null) {
    setMessage(`No match from line ${fmt(from + 1)} to the end`);
    lastHit = null;
    render();
    return;
  }
  lastHit = hit;
  setMessage(`Match on line ${fmt(hit + 1)}`);
  goTo(hit - 3);
});

// Typing a different query restarts the search from the top of the view.
el("query").addEventListener("input", () => {
  lastHit = null;
  render();
});

el("goto").addEventListener("keydown", (event) => {
  if (event.key !== "Enter" || !info) return;
  const line = Number(event.target.value);
  if (Number.isFinite(line) && line >= 1) goTo(line - 1);
});

listen("index-progress", ({ payload }) => {
  if (payload.total > 0 && payload.done < payload.total) {
    setMessage(`Indexing... ${Math.floor((payload.done / payload.total) * 100)}%`);
  }
});

invoke("initial_path").then((path) => {
  if (path) openPath(path);
});
