"use strict";
// kEngram read-only web UI — vanilla JS, no framework.
// Page is selected by <body data-page="...">; each page wires its own behavior
// against the read-only /api/* endpoints.

const $ = (sel, root = document) => root.querySelector(sel);
const el = (tag, attrs = {}, ...kids) => {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") n.className = v;
    else if (k === "html") n.innerHTML = v;
    else if (v !== null && v !== undefined) n.setAttribute(k, v);
  }
  for (const kid of kids) n.append(kid?.nodeType ? kid : document.createTextNode(kid ?? ""));
  return n;
};

async function getJSON(url) {
  const r = await fetch(url, { headers: { accept: "application/json" } });
  const body = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(body.error || `HTTP ${r.status}`);
  return body;
}

function snippet(text, n = 280) {
  text = (text || "").replace(/\s+/g, " ").trim();
  return text.length > n ? text.slice(0, n) + "…" : text;
}

function tagChips(tags) {
  const wrap = el("span", { class: "scores" });
  if (!tags) return wrap;
  if (tags.kind) wrap.append(el("span", { class: "tag kind" }, tags.kind));
  for (const t of (tags.topics || []).slice(0, 4)) wrap.append(el("span", { class: "tag" }, t));
  return wrap;
}

function score(label, val) {
  if (val === null || val === undefined) return null;
  return el("span", { class: "score" }, `${label} ${Number(val).toFixed(3)}`);
}

// ---- Search page ---------------------------------------------------------
function renderSearchResults(data) {
  const list = $("#search-results");
  const status = $("#search-status");
  const banner = $("#search-banner");
  list.replaceChildren();

  if (data.vector_search_available === false) {
    banner.hidden = false;
    banner.textContent = "Vector search unavailable (embedder down) — showing keyword results only.";
  } else {
    banner.hidden = true;
  }

  const n = (data.results || []).length;
  status.textContent = n
    ? `${n} result${n === 1 ? "" : "s"}${data.rerank_used ? " · reranked" : ""}`
    : "No results.";

  for (const h of data.results || []) {
    const head = el("div", { class: "result-head" },
      el("a", { class: "result-link", href: `/thought/${h.thought_id}` }, snippet(h.content, 90)),
    );
    const meta = el("div", { class: "result-meta" },
      el("span", { class: "tag scope" }, h.scope),
      tagChips(h.tags),
    );
    const scores = el("div", { class: "scores" });
    for (const s of [
      score("vec", h.vector_score), score("tri", h.trigram_score),
      score("rrf", h.rrf_score), score("rk", h.rerank_score),
    ]) if (s) scores.append(s);

    list.append(el("li", { class: "result" },
      head,
      el("p", { class: "result-snippet" }, snippet(h.content)),
      meta,
      scores,
    ));
  }
}

function initSearch() {
  const form = $("#search-form");
  const input = $("#search-input");
  const scope = $("#search-scope");
  const status = $("#search-status");

  async function run() {
    const q = input.value.trim();
    if (!q) { $("#search-results").replaceChildren(); status.textContent = ""; return; }
    const params = new URLSearchParams({ q, limit: "25" });
    if (scope.value.trim()) params.set("scope", scope.value.trim());
    // keep the URL shareable
    history.replaceState(null, "", `/?${params.toString()}`);
    status.textContent = "Searching…";
    try {
      renderSearchResults(await getJSON(`/api/search?${params.toString()}`));
    } catch (e) {
      status.textContent = `Error: ${e.message}`;
    }
  }

  form.addEventListener("submit", (e) => { e.preventDefault(); run(); });

  // Search-as-you-type, debounced, so a quick typist doesn't fire a request
  // per keystroke.
  let timer = null;
  const debounced = () => { clearTimeout(timer); timer = setTimeout(run, 250); };
  input.addEventListener("input", debounced);
  scope.addEventListener("input", debounced);

  if (input.value.trim()) run(); // server seeded `q` from the URL
}

// ---- Thought detail page -------------------------------------------------
function renderThought(data) {
  const root = $("#thought");
  const t = data.thought;
  const p = data.provenance || {};
  const kv = el("dl", { class: "kv" });
  const row = (k, v) => { if (v === null || v === undefined || v === "") return; kv.append(el("dt", {}, k), el("dd", {}, String(v))); };
  row("scope", t.scope);
  row("source", t.source);
  row("created", t.created_at);
  row("kind", (p.tags || {}).kind);
  row("people", ((p.tags || {}).people || []).join(", "));
  row("entities", ((p.tags || {}).entities || []).join(", "));
  row("topics", ((p.tags || {}).topics || []).join(", "));
  row("action items", ((p.tags || {}).action_items || []).join(" · "));
  row("embedding", p.embedding_status);
  row("tagger", p.tags_extractor_model ? `${p.tags_extractor_model} v${p.tags_extractor_version ?? "?"}` : null);
  if (p.retracted_at) row("retracted", `${p.retracted_at}${p.retracted_reason ? " — " + p.retracted_reason : ""}`);

  root.replaceChildren(
    el("h1", {}, `${t.scope}`),
    el("div", { class: "body" }, t.content),
    kv,
  );
}

function renderRelated(data) {
  const root = $("#related");
  const groups = [["outbound", data.outbound || []], ["inbound", data.inbound || []]];
  root.replaceChildren();
  let any = false;
  for (const [dir, edges] of groups) {
    if (!edges.length) continue;
    any = true;
    root.append(el("h3", { class: "muted" }, dir));
    for (const e of edges) {
      const label = e.to_kind === "thought"
        ? el("a", { href: `/thought/${e.thought_id}` }, snippet(e.content_preview || e.thought_id, 80))
        : el("span", {}, `${e.to_kind}: ${e.to_value}`);
      const line = el("div", { class: "edge" }, el("span", { class: "rel" }, e.relation), label);
      if (e.retracted) line.append(el("span", { class: "muted" }, " (retracted)"));
      root.append(line);
    }
  }
  if (!any) root.append(el("p", { class: "muted" }, "No links to this thought yet."));
}

async function initThought() {
  const root = $("#thought");
  const id = root.dataset.thoughtId;
  try {
    renderThought(await getJSON(`/api/thoughts/${id}`));
  } catch (e) {
    root.replaceChildren(el("p", { class: "banner" }, `Error: ${e.message}`));
    return;
  }
  try {
    renderRelated(await getJSON(`/api/thoughts/${id}/related`));
  } catch (e) {
    $("#related").replaceChildren(el("p", { class: "muted" }, `Could not load related: ${e.message}`));
  }
}

// ---- Graph page ----------------------------------------------------------
function farNodeId(h) {
  return h.to_kind === "thought" ? h.thought_id : `${h.to_kind}:${h.to_value}`;
}

async function expandNode(cy, id) {
  const status = $("#graph-status");
  const existing = cy.getElementById(id);
  if (existing.length && existing.data("expanded")) return;
  if (existing.length) existing.data("expanded", true);
  status.textContent = `Expanding ${id.slice(0, 8)}…`;
  let data;
  try {
    data = await getJSON(`/api/thoughts/${encodeURIComponent(id)}/related`);
  } catch (e) {
    status.textContent = `Error: ${e.message}`;
    return;
  }
  const add = [];
  const ensureNode = (nid, label, kind) => {
    if (cy.getElementById(nid).length === 0)
      add.push({ group: "nodes", data: { id: nid, label, kind, expanded: false } });
  };
  for (const dir of ["outbound", "inbound"]) {
    for (const h of data[dir] || []) {
      const far = farNodeId(h);
      const label = h.to_kind === "thought" ? snippet(h.content_preview || far, 40) : h.to_value;
      ensureNode(far, label, h.to_kind);
      const source = dir === "outbound" ? id : far;
      const target = dir === "outbound" ? far : id;
      if (cy.getElementById(h.link_id).length === 0)
        add.push({ group: "edges", data: { id: h.link_id, source, target, label: h.relation } });
    }
  }
  cy.add(add);
  cy.layout({ name: "cose", animate: false, fit: true, padding: 30 }).run();
  status.textContent = add.length ? "" : "No further links from this node.";
}

async function loadRoot(cy, rootId) {
  cy.elements().remove();
  let label = rootId;
  try {
    const t = await getJSON(`/api/thoughts/${encodeURIComponent(rootId)}`);
    label = snippet(t.thought.content, 40);
  } catch (_) { /* keep id as label; expand surfaces real errors */ }
  cy.add({ group: "nodes", data: { id: rootId, label, kind: "thought", root: true, expanded: false } });
  await expandNode(cy, rootId);
}

window.initGraph = function () {
  const container = $("#graph");
  if (!window.cytoscape) { $("#graph-status").textContent = "cytoscape failed to load."; return; }
  const cy = window.cytoscape({
    container,
    style: [
      { selector: "node", style: {
        label: "data(label)", "font-size": "9px", color: "#e6e8ec",
        "background-color": "#3a4356", "text-wrap": "wrap", "text-max-width": "120px",
        width: "18px", height: "18px",
      }},
      { selector: 'node[kind="thought"]', style: { "background-color": "#6ea8fe" } },
      { selector: 'node[kind="entity"]', style: { "background-color": "#9fe0b8", shape: "round-rectangle" } },
      { selector: 'node[kind="person"]', style: { "background-color": "#f0d28a" } },
      { selector: 'node[kind="url"]', style: { "background-color": "#c6b8e0", shape: "diamond" } },
      { selector: "node[?root]", style: { "border-width": "3px", "border-color": "#ffffff" } },
      { selector: "edge", style: {
        label: "data(label)", "font-size": "7px", color: "#8b93a1", width: 1,
        "line-color": "#2a2f3a", "target-arrow-color": "#2a2f3a",
        "target-arrow-shape": "triangle", "curve-style": "bezier", "text-rotation": "autorotate",
      }},
    ],
  });

  cy.on("tap", "node", (evt) => {
    const n = evt.target;
    if (n.data("kind") === "thought") expandNode(cy, n.id());
  });

  $("#graph-form").addEventListener("submit", (e) => {
    e.preventDefault();
    const v = $("#graph-root").value.trim();
    if (v) { history.replaceState(null, "", `/graph?root=${encodeURIComponent(v)}`); loadRoot(cy, v); }
  });

  const root = container.dataset.root;
  if (root) loadRoot(cy, root);
  else $("#graph-status").textContent = "Enter a thought id, or open a thought and click “View in graph”.";
};

// ---- Dispatch ------------------------------------------------------------
document.addEventListener("DOMContentLoaded", () => {
  const page = document.body.dataset.page;
  if (page === "search") initSearch();
  else if (page === "thought") initThought();
  else if (page === "graph" && window.initGraph) window.initGraph();
});
