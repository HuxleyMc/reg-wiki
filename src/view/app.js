"use strict";
(function () {
  const data = JSON.parse(document.getElementById("data").textContent);
  const nodes = data.nodes;
  const edges = data.edges;
  const $ = (id) => document.getElementById(id);
  const canvas = $("graph");
  const ctx = canvas.getContext("2d");

  // ---- Derived data -------------------------------------------------------
  const bySlug = new Map();
  nodes.forEach((n, i) => {
    n.i = i;
    n.out = [];
    n.back = [];
    n.refs = [];
    n.degree = 0;
    bySlug.set(n.slug.toLowerCase(), n);
  });
  for (const e of edges) {
    const s = nodes[e.s], t = nodes[e.t];
    s.degree++;
    t.degree++;
    if (e.kind === "ref") {
      s.refs.push(t);
    }
  }
  // Direction matters for the panel, so take it from the rendered links.
  for (const n of nodes) {
    const seen = new Set();
    for (const m of n.html.matchAll(/href="#p=([^"]+)"/g)) {
      const t = bySlug.get(decodeURIComponent(m[1]).toLowerCase());
      if (t && t !== n && !seen.has(t)) {
        seen.add(t);
        n.out.push(t);
        t.back.push(n);
      }
    }
  }
  const neighbours = nodes.map(() => new Set());
  for (const e of edges) {
    neighbours[e.s].add(e.t);
    neighbours[e.t].add(e.s);
  }

  // ---- Colours ------------------------------------------------------------
  const PALETTE = ["#3b7dd8", "#d9822b", "#2e9e6a", "#c94f7c", "#8a63d2",
    "#c2a320", "#1f9eb3", "#d0563f", "#6b8e23", "#b05cc4", "#4a6fa5", "#a0522d"];
  const SHARED = "_shared";
  const projects = [...new Set(nodes.map((n) => n.project))].sort((a, b) =>
    a === SHARED ? 1 : b === SHARED ? -1 : a.localeCompare(b));
  const colour = new Map();
  let pi = 0;
  for (const p of projects) {
    colour.set(p, p === SHARED ? "#8b8b85" : PALETTE[pi++ % PALETTE.length]);
  }

  // ---- Controls -----------------------------------------------------------
  const allProjects = [...new Set(nodes.flatMap((n) => [n.project, ...n.also]))].sort();
  const tags = [...new Set(nodes.flatMap((n) => n.tags.map((t) => t.toLowerCase())))].sort();
  for (const p of allProjects) $("project").append(new Option(p, p));
  for (const t of tags) $("tag").append(new Option(t, t));
  $("scope").textContent = data.scope ? data.scope + " + " + SHARED : "all projects";

  const hidden = new Set(); // projects switched off in the legend
  const legend = $("legend");
  for (const p of projects) {
    const li = document.createElement("li");
    li.innerHTML = '<i></i><span></span>';
    li.firstChild.style.background = colour.get(p);
    li.lastChild.textContent = p;
    li.title = "Show or hide " + p;
    li.onclick = () => {
      hidden.has(p) ? hidden.delete(p) : hidden.add(p);
      li.classList.toggle("off", hidden.has(p));
      applyFilters();
    };
    legend.append(li);
  }

  let query = "";
  let matches = null; // Set of matching nodes, or null when not searching
  let visible = nodes.map(() => true);

  function applyFilters() {
    const proj = $("project").value;
    const tag = $("tag").value;
    const refs = $("refs").checked;
    visible = nodes.map((n) =>
      !hidden.has(n.project) &&
      (!proj || n.project === proj || n.also.includes(proj)) &&
      (!tag || n.tags.some((t) => t.toLowerCase() === tag)) &&
      (refs || !n.is_ref));
    applySearch();
    reheat(0.3);
  }

  function applySearch() {
    query = $("search").value.trim().toLowerCase();
    if (!query) {
      matches = null;
    } else {
      const words = query.split(/\s+/);
      matches = new Set(nodes.filter((n) => visible[n.i] && words.every((w) =>
        n.title.toLowerCase().includes(w) || n.slug.toLowerCase().includes(w) ||
        n.summary.toLowerCase().includes(w) || n.tags.some((t) => t.toLowerCase().includes(w)))));
    }
    const shown = visible.filter(Boolean).length;
    const links = edges.filter((e) => visible[e.s] && visible[e.t]).length;
    $("counts").textContent = (matches ? matches.size + " of " : "") + shown + " pages · " +
      links + " links" + (data.broken ? " · " + data.broken + " broken" : "");
    draw();
  }

  $("project").onchange = applyFilters;
  $("tag").onchange = applyFilters;
  $("refs").onchange = applyFilters;
  $("search").oninput = applySearch;
  $("search").onkeydown = (e) => {
    if (e.key === "Enter" && matches && matches.size) {
      const best = [...matches].sort((a, b) => score(b) - score(a))[0];
      go(best.slug);
    } else if (e.key === "Escape") {
      $("search").value = "";
      applySearch();
    }
  };
  function score(n) {
    const t = n.title.toLowerCase();
    return (t === query ? 100 : 0) + (t.startsWith(query) ? 50 : 0) +
      (t.includes(query) ? 20 : 0) + n.degree;
  }

  // ---- Layout -------------------------------------------------------------
  const radius = (n) => (n.is_ref ? 3 : 5) + Math.sqrt(n.degree) * (n.is_ref ? 1.6 : 2.4) + (n.hub ? 3 : 0);
  {
    // Start each project in its own sector so clusters form quickly.
    const golden = Math.PI * (3 - Math.sqrt(5));
    nodes.forEach((n, i) => {
      const sector = projects.indexOf(n.project) / Math.max(projects.length, 1);
      const a = sector * 2 * Math.PI + i * golden * 0.15;
      const r = 40 + Math.sqrt(i) * 18;
      n.x = Math.cos(a) * r;
      n.y = Math.sin(a) * r;
      n.vx = 0;
      n.vy = 0;
    });
  }
  let alpha = 1;
  const LINK = 70, REPEL = 1400, GRAVITY = 0.012, DAMP = 0.6;

  function tick() {
    const live = nodes.filter((n) => visible[n.i]);
    for (let a = 0; a < live.length; a++) {
      const p = live[a];
      for (let b = a + 1; b < live.length; b++) {
        const q = live[b];
        let dx = q.x - p.x, dy = q.y - p.y;
        let d2 = dx * dx + dy * dy;
        if (d2 < 1) { dx = Math.random() - 0.5; dy = Math.random() - 0.5; d2 = 1; }
        if (d2 > 250000) continue;
        const f = (REPEL * alpha) / d2;
        const d = Math.sqrt(d2);
        const fx = (dx / d) * f, fy = (dy / d) * f;
        p.vx -= fx; p.vy -= fy;
        q.vx += fx; q.vy += fy;
      }
    }
    for (const e of edges) {
      if (!visible[e.s] || !visible[e.t]) continue;
      const s = nodes[e.s], t = nodes[e.t];
      const dx = t.x - s.x, dy = t.y - s.y;
      const d = Math.sqrt(dx * dx + dy * dy) || 1;
      const want = e.kind === "ref" ? LINK * 0.55 : LINK;
      const f = ((d - want) / d) * 0.08 * alpha;
      s.vx += dx * f; s.vy += dy * f;
      t.vx -= dx * f; t.vy -= dy * f;
    }
    for (const n of live) {
      n.vx -= n.x * GRAVITY * alpha;
      n.vy -= n.y * GRAVITY * alpha;
      if (n === dragNode) { n.vx = n.vy = 0; continue; }
      n.vx *= DAMP; n.vy *= DAMP;
      n.x += n.vx; n.y += n.vy;
    }
    alpha *= 0.985;
  }

  let running = false;
  function reheat(a) {
    alpha = Math.max(alpha, a);
    if (!running) { running = true; requestAnimationFrame(loop); }
  }
  function loop() {
    tick();
    draw();
    if (alpha > 0.01 || dragNode) requestAnimationFrame(loop);
    else running = false;
  }

  // ---- View transform & drawing ---------------------------------------------
  let view = { x: 0, y: 0, k: 1 };
  let W = 0, H = 0, dpr = 1;
  function resize() {
    const r = canvas.getBoundingClientRect();
    dpr = window.devicePixelRatio || 1;
    W = r.width; H = r.height;
    canvas.width = Math.round(W * dpr);
    canvas.height = Math.round(H * dpr);
    draw();
  }
  const toScreen = (n) => [(n.x - view.x) * view.k + W / 2, (n.y - view.y) * view.k + H / 2];
  const toWorld = (sx, sy) => [(sx - W / 2) / view.k + view.x, (sy - H / 2) / view.k + view.y];

  function fit() {
    const live = nodes.filter((n) => visible[n.i]);
    if (!live.length || !W) return;
    let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
    for (const n of live) {
      x0 = Math.min(x0, n.x); y0 = Math.min(y0, n.y);
      x1 = Math.max(x1, n.x); y1 = Math.max(y1, n.y);
    }
    view.x = (x0 + x1) / 2;
    view.y = (y0 + y1) / 2;
    view.k = Math.min(2, Math.max(0.1, Math.min(W / (x1 - x0 + 120), H / (y1 - y0 + 120))));
  }

  let hover = null;
  let selected = null;
  const css = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();

  function draw() {
    if (!W) return;
    const fg = css("--fg"), bg = css("--bg"), edge = css("--edge"), edgeHi = css("--edge-hi");
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, W, H);
    const focus = hover || selected;
    const near = focus ? neighbours[focus.i] : null;
    const dim = (n) => (matches && !matches.has(n)) || (near && n !== focus && !near.has(n.i));

    ctx.lineWidth = 1;
    for (const e of edges) {
      if (!visible[e.s] || !visible[e.t]) continue;
      const s = nodes[e.s], t = nodes[e.t];
      const hi = focus && (s === focus || t === focus);
      ctx.strokeStyle = hi ? edgeHi : edge;
      ctx.globalAlpha = !hi && (dim(s) || dim(t)) ? 0.35 : 1;
      ctx.setLineDash(e.kind === "ref" ? [3, 3] : []);
      const [x1, y1] = toScreen(s), [x2, y2] = toScreen(t);
      ctx.beginPath(); ctx.moveTo(x1, y1); ctx.lineTo(x2, y2); ctx.stroke();
    }
    ctx.setLineDash([]);

    const labels = [];
    for (const n of nodes) {
      if (!visible[n.i]) continue;
      const [x, y] = toScreen(n);
      if (x < -50 || y < -50 || x > W + 50 || y > H + 50) continue;
      const r = radius(n) * Math.sqrt(view.k);
      ctx.globalAlpha = dim(n) ? 0.18 : 1;
      ctx.beginPath();
      ctx.arc(x, y, r, 0, 2 * Math.PI);
      if (n.is_ref) {
        ctx.fillStyle = bg; ctx.fill();
        ctx.lineWidth = 2; ctx.strokeStyle = colour.get(n.project); ctx.stroke();
      } else {
        ctx.fillStyle = colour.get(n.project); ctx.fill();
      }
      if (n === selected || (matches && matches.has(n))) {
        ctx.lineWidth = 2; ctx.strokeStyle = fg;
        ctx.beginPath(); ctx.arc(x, y, r + 3, 0, 2 * Math.PI); ctx.stroke();
      }
      const show = n === focus || (near && near.has(n.i)) || (matches && matches.has(n)) ||
        n.hub || view.k * radius(n) > 9 || (view.k > 1.4 && !n.is_ref);
      if (show && !dim(n)) labels.push([n, x, y + r + 12]);
    }
    ctx.globalAlpha = 1;
    ctx.font = "12px system-ui, sans-serif";
    ctx.textAlign = "center";
    ctx.lineWidth = 3;
    ctx.strokeStyle = bg;
    ctx.fillStyle = fg;
    for (const [n, x, y] of labels) {
      const text = n.title.length > 40 ? n.title.slice(0, 39) + "…" : n.title;
      ctx.strokeText(text, x, y);
      ctx.fillText(text, x, y);
    }
  }

  // ---- Pointer --------------------------------------------------------------
  function nodeAt(sx, sy) {
    const [wx, wy] = toWorld(sx, sy);
    let best = null, bestD = Infinity;
    for (const n of nodes) {
      if (!visible[n.i]) continue;
      const r = radius(n) / Math.sqrt(view.k) + 4 / view.k;
      const d = Math.hypot(n.x - wx, n.y - wy);
      if (d < r && d < bestD) { best = n; bestD = d; }
    }
    return best;
  }
  let dragNode = null, panning = null, moved = false, down = null;
  const pos = (e) => { const r = canvas.getBoundingClientRect(); return [e.clientX - r.left, e.clientY - r.top]; };

  canvas.addEventListener("pointerdown", (e) => {
    const [sx, sy] = pos(e);
    canvas.setPointerCapture(e.pointerId);
    down = [sx, sy];
    moved = false;
    const n = nodeAt(sx, sy);
    if (n) { dragNode = n; reheat(0.1); }
    else { panning = { sx, sy, x: view.x, y: view.y }; canvas.classList.add("dragging"); }
  });
  canvas.addEventListener("pointermove", (e) => {
    const [sx, sy] = pos(e);
    if (down && Math.hypot(sx - down[0], sy - down[1]) > 3) moved = true;
    if (dragNode) {
      const [wx, wy] = toWorld(sx, sy);
      dragNode.x = wx; dragNode.y = wy;
      reheat(0.1);
    } else if (panning) {
      view.x = panning.x - (sx - panning.sx) / view.k;
      view.y = panning.y - (sy - panning.sy) / view.k;
      draw();
    } else {
      const n = nodeAt(sx, sy);
      if (n !== hover) {
        hover = n;
        canvas.classList.toggle("over", !!n);
        canvas.title = n ? n.title + " — " + n.summary : "";
        draw();
      }
    }
  });
  canvas.addEventListener("pointerup", (e) => {
    const [sx, sy] = pos(e);
    if (!moved) {
      const n = nodeAt(sx, sy);
      if (n) go(n.slug);
      else if (!dragNode) close();
    }
    dragNode = null; panning = null; down = null;
    canvas.classList.remove("dragging");
  });
  canvas.addEventListener("pointerleave", () => { if (hover) { hover = null; draw(); } });
  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    const [sx, sy] = pos(e);
    const [wx, wy] = toWorld(sx, sy);
    view.k = Math.min(8, Math.max(0.05, view.k * Math.exp(-e.deltaY * 0.0015)));
    view.x = wx - (sx - W / 2) / view.k;
    view.y = wy - (sy - H / 2) / view.k;
    draw();
  }, { passive: false });

  // ---- Panel ------------------------------------------------------------------
  const esc = (s) => String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
  const link = (n) => '<li><a href="#p=' + encodeURI(n.slug) + '">' + esc(n.title) +
    "</a> <span>— " + esc(n.summary || "(no summary)") + "</span></li>";
  function list(title, items) {
    if (!items.length) return "";
    const sorted = [...items].sort((a, b) => a.title.localeCompare(b.title));
    return "<h2>" + title + " (" + items.length + ")</h2><ul>" + sorted.map(link).join("") + "</ul>";
  }

  function show(n) {
    selected = n;
    const chip = (t, cls, style) => '<span class="chip ' + (cls || "") + '"' +
      (style ? ' style="' + style + '"' : "") + ">" + esc(t) + "</span>";
    const meta = [chip(n.project, "dot", "--c:" + colour.get(n.project))]
      .concat(n.also.map((p) => chip("+ " + p)))
      .concat(n.is_ref ? [chip("ref")] : [])
      .concat(n.hub ? [chip("hub")] : [])
      .concat(n.status ? [chip(n.status)] : [])
      .concat(n.tags.map((t) => chip("#" + t)))
      .concat(n.updated ? [chip("updated " + n.updated)] : [])
      .concat([chip(n.words + " words")]);
    const parent = n.parent && bySlug.get(n.parent.toLowerCase());
    $("page").innerHTML =
      '<h1 class="title">' + esc(n.title) + "</h1>" +
      '<div class="slug muted">' + esc(n.slug) + "</div>" +
      (n.summary ? '<p class="summary">' + esc(n.summary) + "</p>" : "") +
      '<div class="meta">' + meta.join("") + "</div>" +
      '<div class="body">' + n.html + "</div>" +
      '<div class="related">' +
      (parent ? list("Parent", [parent]) : "") +
      list("Refs", n.refs) +
      list("Links", n.out.filter((t) => !n.refs.includes(t) && t !== parent)) +
      list("Backlinks", n.back) +
      "</div>";
    $("panel").hidden = false;
    $("panel").scrollTop = 0;
    resize();
    draw();
  }
  function close() {
    selected = null;
    $("panel").hidden = true;
    if (location.hash) history.replaceState(null, "", location.pathname + location.search);
    resize();
  }
  $("close").onclick = close;
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && document.activeElement !== $("search")) close();
    if (e.key === "/" && document.activeElement !== $("search")) { e.preventDefault(); $("search").focus(); }
  });

  function go(slug) {
    const hash = "#p=" + encodeURI(slug);
    if (location.hash === hash) route(); else location.hash = hash;
  }
  function route() {
    const h = location.hash;
    if (!h.startsWith("#p=")) return;
    const n = bySlug.get(decodeURIComponent(h.slice(3)).toLowerCase());
    if (!n) return;
    if (!visible[n.i]) {
      // Following a link to a page the filters hide: clear them so it can be shown.
      $("project").value = ""; $("tag").value = ""; $("refs").checked = true;
      hidden.clear();
      legend.querySelectorAll("li").forEach((li) => li.classList.remove("off"));
      applyFilters();
    }
    show(n);
    const [sx, sy] = toScreen(n);
    if (sx < 40 || sy < 40 || sx > W - 40 || sy > H - 40) { view.x = n.x; view.y = n.y; draw(); }
  }
  window.addEventListener("hashchange", route);
  document.addEventListener("click", (e) => {
    const a = e.target.closest && e.target.closest("a");
    if (!a) return;
    const href = a.getAttribute("href") || "";
    if (href === "#" || href.startsWith("#missing=")) {
      e.preventDefault();
      if (href !== "#") a.title = "No page " + decodeURIComponent(href.slice(9));
    }
  });
  window.addEventListener("resize", resize);
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", draw);

  // ---- Start ------------------------------------------------------------------
  resize();
  // Settle small graphs before the first frame so the page opens on a stable layout.
  const warm = nodes.length <= 400 ? 220 : nodes.length <= 1500 ? 60 : 0;
  for (let i = 0; i < warm; i++) tick();
  fit();
  applyFilters();
  if (location.hash) route();
})();
