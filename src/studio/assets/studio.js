/* brain studio — the brain, drawn and driven.
 *
 * The temporal predicates here mirror `TemporalFilter` in src/recall.rs exactly,
 * and the comments say so where they do. A debugger that filters time even
 * slightly differently from the thing it is debugging is worse than no debugger:
 * it invents disagreements and hides real ones.
 *
 * One addition the CLI has no equivalent for: the transaction-time cursor. The
 * brain can travel in valid time (`--as-of`) but not in transaction time, and
 * yet everything needed to reconstruct "what did the brain believe on day T" is
 * in the snapshot -- `recorded_at` on every fact, and `superseded_by` pointing
 * at the fact whose own `recorded_at` is when the closure was learned. That
 * reconstruction happens in `asKnownAt` below and nowhere else.
 */
(function () {
  'use strict';

  const SNAP = JSON.parse(document.getElementById('brain-snapshot').textContent);
  const TOKEN = new URLSearchParams(location.search).get('t');
  const LIVE = !!SNAP.live && !!TOKEN;

  const $ = (id) => document.getElementById(id);
  const ms = (iso) => (iso == null ? null : Date.parse(iso));
  const NOW = Date.now();

  // --- model ----------------------------------------------------------------

  const entities = new Map();
  const factById = new Map();

  /* Builds the in-memory model from a snapshot.
   *
   * A function rather than the top-level loop it wants to be, because a write
   * answers with a whole new snapshot and the page has to be able to swallow one
   * without reloading. */
  function ingest(snap) {
    entities.clear();
    factById.clear();
    for (const e of snap.entities) {
      entities.set(e.id, Object.assign({ facts: [], out: [], in: [] }, e));
    }
    for (const f of snap.facts) {
      f._from = ms(f.valid_from);
      f._to = ms(f.valid_to);
      f._rec = ms(f.recorded_at);
      f._ret = ms(f.retracted_at);
      f._edge = f.object_entity_id != null;
      factById.set(f.id, f);

      const subj = entities.get(f.entity_id);
      if (subj) subj.facts.push(f);
      if (f._edge) {
        if (subj) subj.out.push(f);
        const obj = entities.get(f.object_entity_id);
        if (obj) obj.in.push(f);
      }
    }
    for (const ent of entities.values()) ent.group = groupOf(ent);
  }

  /* What a node is coloured by.
   *
   * `entity.kind` is the schema's own answer and wins wherever it is set. It
   * mostly is not -- no write path fills that column yet -- so the fallback is
   * whichever predicate the entity carries most of, which separates a brain into
   * its natural families for nothing: the things with a `preco` are products,
   * the things with a `pais` are places, the things with a `porta` are services.
   * Ties break on the predicate name, so the palette does not shift between
   * reloads of the same brain. */
  function groupOf(ent) {
    if (ent.kind) return ent.kind;
    const counts = new Map();
    for (const f of ent.facts) {
      if (!f._edge) counts.set(f.predicate, (counts.get(f.predicate) || 0) + 1);
    }
    let best = null;
    let bestN = 0;
    for (const [p, n] of [...counts.entries()].sort((a, b) => (a[0] < b[0] ? -1 : 1))) {
      if (n > bestN) { best = p; bestN = n; }
    }
    return best;
  }

  ingest(SNAP);

  /* Two domains, not one, because the axes measure different things and one of
   * them is usually far narrower. A brain populated by a script holds two years
   * of valid time recorded inside a single second; drawn against a shared
   * domain, every bar on the plane stacks onto its top edge and the transaction
   * axis shows nothing at all. */
  let DOMAIN = { lo: 0, hi: 1 };   // valid time — the x axis everywhere
  let TX = { lo: 0, hi: 1 };       // transaction time — the plane's y axis

  function computeDomains() {
    let lo = Infinity, hi = -Infinity, tlo = Infinity, thi = -Infinity;
    for (const f of SNAP.facts) {
      for (const t of [f._from, f._to]) {
        if (t == null) continue;
        if (t < lo) lo = t;
        if (t > hi) hi = t;
      }
      for (const t of [f._rec, f._ret]) {
        if (t == null) continue;
        if (t < tlo) tlo = t;
        if (t > thi) thi = t;
      }
    }
    if (!isFinite(lo)) { lo = NOW - 86400e3; hi = NOW; }
    if (hi <= lo) hi = lo + 86400e3;
    const pad = (hi - lo) * 0.06;
    DOMAIN = { lo: lo - pad, hi: Math.max(hi, NOW) + pad };

    if (!isFinite(tlo)) { tlo = NOW - 1000; thi = NOW; }
    // A brain written in one sitting has a transaction span of milliseconds.
    // A floor keeps the cursor draggable rather than degenerate.
    if (thi - tlo < 1000) thi = tlo + 1000;
    const tpad = (thi - tlo) * 0.08;
    TX = { lo: tlo - tpad, hi: thi + tpad };
  }
  computeDomains();

  const state = {
    mode: 'now',                 // now | asof | history
    asOf: DOMAIN.hi,
    knownAt: TX.hi,              // transaction-time cursor
    showRetracted: false,
    showIsolated: true,
    showLabels: true,
    selected: null,
    pathTarget: null,
    selectedFact: null,
    hits: [],
    hitChannels: new Map(),      // entity id -> Set(channel)
    which: null,                 // predicate whose whole set is selected
  };

  // --- temporal predicates ---------------------------------------------------

  /* What the brain knew at the transaction-time cursor.
   *
   * `valid_to` is written onto a fact when a later fact closes it, and the
   * schema records *which* fact did that but not when the closing was learned.
   * It does not need to: the closer's own `recorded_at` is that instant. So at a
   * cursor before it, this fact was still open. */
  function asKnownAt(f, knownAt) {
    if (f._rec > knownAt) return null;               // not yet told
    let to = f._to;
    if (to != null && f.superseded_by != null) {
      const closer = factById.get(f.superseded_by);
      if (closer && closer._rec > knownAt) to = null;
    }
    const retracted = f._ret != null && f._ret <= knownAt;
    return { to, retracted };
  }

  /* Mirrors TemporalFilter::for_when in src/recall.rs.
   *
   *   Now      -> retracted_at IS NULL AND valid_from <= now AND (valid_to IS NULL OR now < valid_to)
   *   AsOf(t)  -> the same, against t
   *   History  -> retracted_at IS NULL
   *
   * Two deliberate divergences, both because a debugger has to be able to see
   * what a retriever is right to hide:
   *
   * - `showRetracted`. Recall hides retractions in every mode, since replaying
   *   one would state something that was never true. Off by default here, and
   *   struck through when on.
   * - A fact dated in the future stays in the `now` view, ringed in violet. The
   *   core excludes it -- it is not true yet -- but "what is scheduled" is a
   *   question you ask a memory while looking at it, and the alternative is a
   *   fact that exists and appears nowhere until its day comes. */
  function visible(f) {
    const k = asKnownAt(f, state.knownAt);
    if (k === null) return null;
    if (k.retracted && !state.showRetracted) return null;

    let ok;
    // Not "has no valid_to" but "has not ended": a fact can carry its own end
    // now, so `valid_to` set no longer means something replaced it.
    if (state.mode === 'now') ok = k.to == null || NOW < k.to;
    else if (state.mode === 'asof') ok = f._from <= state.asOf && (k.to == null || state.asOf < k.to);
    else ok = true;
    if (!ok) return null;

    const at = state.mode === 'asof' ? state.asOf : NOW;
    return {
      fact: f,
      to: k.to,
      retracted: k.retracted,
      // Open but not yet true.
      future: !k.retracted && f._from > at,
      // True, with an end already on it. Nothing has to happen for this to stop
      // being the answer, which is the one fact state that changes on its own.
      expiring: !k.retracted && k.to != null && at < k.to,
      closed: k.to != null,
    };
  }

  /* The precedence matters. An expiring fact is `closed` too -- it has a
   * `valid_to` -- and drawing it as a ghost would say it had already ended. A
   * scheduled window is both future and expiring, and not-yet-true is the more
   * useful thing to know about it. */
  function factState(v) {
    if (v.retracted) return 'retracted';
    if (v.future) return 'future';
    if (v.expiring) return 'expiring';
    if (v.closed) return 'closed';
    return 'open';
  }

  /* Whether a fact is in force, for anything that draws ended things faintly. */
  const isLive = (st) => st === 'open' || st === 'future' || st === 'expiring';

  // --- visible graph ---------------------------------------------------------

  let view = { nodes: [], edges: [], nodeIndex: new Map(), adjacency: new Map() };

  function rebuildView() {
    const edges = [];
    const touched = new Set();
    const visibleFacts = new Map();

    for (const f of SNAP.facts) {
      const v = visible(f);
      if (!v) continue;
      visibleFacts.set(f.id, v);
      touched.add(f.entity_id);
      if (f._edge) {
        touched.add(f.object_entity_id);
        edges.push({
          fact: f,
          src: f.entity_id,
          dst: f.object_entity_id,
          state: factState(v),
        });
      }
    }

    const linked = new Set();
    for (const e of edges) { linked.add(e.src); linked.add(e.dst); }

    const nodes = [];
    for (const ent of entities.values()) {
      const isLinked = linked.has(ent.id);
      if (!isLinked && !state.showIsolated) continue;
      // An entity with nothing visible at this instant is not in the picture.
      if (!touched.has(ent.id) && !isLinked) continue;
      nodes.push(ent);
    }

    const nodeIndex = new Map(nodes.map((n, i) => [n.id, i]));
    const kept = edges.filter((e) => nodeIndex.has(e.src) && nodeIndex.has(e.dst));

    const adjacency = new Map(nodes.map((n) => [n.id, []]));
    for (const e of kept) {
      adjacency.get(e.src).push({ to: e.dst, edge: e });
      adjacency.get(e.dst).push({ to: e.src, edge: e });
    }

    view = { nodes, edges: kept, nodeIndex, adjacency, visibleFacts };
    sim.sync();
    scene3d.rebuild();
    renderInspector();
    drawScrub();
    drawPlane();
    updateCounts();
  }

  function shortestPath(a, b) {
    if (a == null || b == null || a === b) return null;
    const prev = new Map([[a, null]]);
    const queue = [a];
    while (queue.length) {
      const cur = queue.shift();
      if (cur === b) break;
      for (const step of view.adjacency.get(cur) || []) {
        if (prev.has(step.to)) continue;
        prev.set(step.to, { from: cur, edge: step.edge });
        queue.push(step.to);
      }
    }
    if (!prev.has(b)) return null;
    const nodes = [b], edges = [];
    let cur = b;
    while (prev.get(cur)) {
      edges.push(prev.get(cur).edge);
      cur = prev.get(cur).from;
      nodes.push(cur);
    }
    return { nodes: new Set(nodes), edges: new Set(edges) };
  }

  function highlight() {
    if (state.selected == null) return null;
    const path = shortestPath(state.selected, state.pathTarget);
    if (path) return path;
    const nodes = new Set([state.selected]);
    const edges = new Set();
    for (const step of view.adjacency.get(state.selected) || []) {
      nodes.add(step.to);
      edges.add(step.edge);
    }
    return { nodes, edges };
  }

  // --- layout ----------------------------------------------------------------

  /* A deterministic force layout.
   *
   * Seeded from the brain's own id, so the same brain lays out the same way on
   * every open. Spatial memory is most of what makes a graph readable a second
   * time, and a layout that reshuffles on reload throws it away for nothing. */
  const sim = (() => {
    const pos = new Map();          // entity id -> {x,y,z,vx,vy,vz}
    let alpha = 1;
    let shown = 0;                  // size of the node set the layout last saw
    let seedState = 0;
    for (const c of SNAP.brain_id) seedState = (seedState * 31 + c.charCodeAt(0)) >>> 0;

    function rand() {
      seedState ^= seedState << 13; seedState >>>= 0;
      seedState ^= seedState >> 17;
      seedState ^= seedState << 5;  seedState >>>= 0;
      return seedState / 4294967296;
    }

    function place(id) {
      // On a sphere rather than in a cube: a cube's corners are further from the
      // centre than its faces, and the sim spends its first seconds undoing that.
      const u = rand() * 2 - 1, a = rand() * Math.PI * 2, r = 26 * Math.cbrt(rand());
      const s = Math.sqrt(1 - u * u);
      return { x: r * s * Math.cos(a), y: r * s * Math.sin(a), z: r * u, vx: 0, vy: 0, vz: 0 };
    }

    return {
      pos,
      reheat(v = 0.9) { alpha = Math.max(alpha, v); },
      sync() {
        let added = 0;
        for (const n of view.nodes) if (!pos.has(n.id)) { pos.set(n.id, place(n.id)); added++; }
        // Only when the set actually moved. Dragging the timeline rebuilds the
        // view on every pointer event, and reheating there pins the layout at
        // full temperature for the whole drag: the cloud boils instead of
        // settling, and nothing that waits on it settling ever happens.
        if (added || shown !== view.nodes.length) this.reheat();
        shown = view.nodes.length;
      },
      step() {
        const nodes = view.nodes;
        const n = nodes.length;
        if (n === 0 || alpha < 0.002) { alpha = Math.max(alpha * 0.98, 0); return false; }

        /* Repulsion is summed over every other node, so holding it constant makes
         * the cloud inflate with the brain: 20 entities settle inside a radius of
         * 97, 615 inside 434, 1500 inside 803. Past a few hundred that is a graph
         * you cannot get far enough away from to see, drawn in nodes four pixels
         * across. Damping it by a square root keeps the radius growing with n --
         * a bigger brain should look bigger -- at n^(1/6) instead of n^(1/3), and
         * the clamp leaves every graph up to REF laid out exactly as before. */
        const REF = 60;
        const REPULSION = 900 * Math.min(1, Math.sqrt(REF / n));
        const SPRING = 0.035, REST = 16, CENTER = 0.012, DAMP = 0.82;

        /* An inverse square with no floor is an explosion waiting for the first
         * pair of nodes that happens to start close together. Measured on a
         * 683-entity brain: the cloud reached a radius of 4.3e13 two and a half
         * seconds after opening and took twenty-four more to fall back to its
         * real 180. Nothing reported it, because a graph that has left the solar
         * system looks the same as a graph you cannot see.
         *
         * MIN_SEP2 is a node diameter squared -- two spheres cannot be nearer
         * than touching, so pretending they can buys nothing but the blow-up.
         * MAX_STEP then bounds the whole system rather than one term of it: no
         * node crosses more than a spring's rest length in a frame, whatever the
         * forces say, so a layout can be wrong but never unbounded. */
        const MIN_SEP2 = 16, MAX_STEP = REST;

        for (let i = 0; i < n; i++) {
          const a = pos.get(nodes[i].id);
          for (let j = i + 1; j < n; j++) {
            const b = pos.get(nodes[j].id);
            let dx = a.x - b.x, dy = a.y - b.y, dz = a.z - b.z;
            let d2 = dx * dx + dy * dy + dz * dz;
            if (d2 < 0.01) { dx = rand() - 0.5; dy = rand() - 0.5; dz = rand() - 0.5; d2 = 0.75; }
            const d = Math.sqrt(d2);
            const f = (REPULSION / Math.max(d2, MIN_SEP2)) * alpha;
            const ux = dx / d, uy = dy / d, uz = dz / d;
            a.vx += ux * f; a.vy += uy * f; a.vz += uz * f;
            b.vx -= ux * f; b.vy -= uy * f; b.vz -= uz * f;
          }
        }

        for (const e of view.edges) {
          const a = pos.get(e.src), b = pos.get(e.dst);
          if (!a || !b) continue;
          const dx = b.x - a.x, dy = b.y - a.y, dz = b.z - a.z;
          const d = Math.sqrt(dx * dx + dy * dy + dz * dz) || 0.001;
          const f = (d - REST) * SPRING * alpha;
          const ux = dx / d, uy = dy / d, uz = dz / d;
          a.vx += ux * f; a.vy += uy * f; a.vz += uz * f;
          b.vx -= ux * f; b.vy -= uy * f; b.vz -= uz * f;
        }

        for (const node of nodes) {
          const p = pos.get(node.id);
          p.vx -= p.x * CENTER * alpha;
          p.vy -= p.y * CENTER * alpha;
          p.vz -= p.z * CENTER * alpha;
          p.vx *= DAMP; p.vy *= DAMP; p.vz *= DAMP;
          const speed = Math.hypot(p.vx, p.vy, p.vz);
          if (speed > MAX_STEP) {
            const k = MAX_STEP / speed;
            p.vx *= k; p.vy *= k; p.vz *= k;
          }
          p.x += p.vx; p.y += p.vy; p.z += p.vz;
        }

        alpha *= 0.985;
        return true;
      },
    };
  })();

  // --- colours ---------------------------------------------------------------

  const CSS = getComputedStyle(document.documentElement);
  const cssColor = (name) => CSS.getPropertyValue(name).trim();

  const COLORS = {
    open: cssColor('--open'),
    closed: cssColor('--closed'),
    retracted: cssColor('--retracted'),
    future: cssColor('--future'),
    expiring: cssColor('--expiring'),
    bm25: cssColor('--ch-bm25'),
    alias: cssColor('--ch-alias'),
    semantic: cssColor('--ch-semantic'),
    graph: cssColor('--ch-graph'),
    // Neither of these is a channel. A static export has no retriever behind it,
    // and a set query does not rank -- colouring either in a channel's hue would
    // claim a retriever ran.
    match: cssColor('--text'),
    set: cssColor('--ch-set'),
  };

  // Muted, evenly spaced hues. A group is free-form text, so the palette is a
  // hash of it and the legend is the graph itself.
  const GROUP_HUES = [188, 40, 268, 210, 150, 12, 322, 90];
  const groupColorCache = new Map();
  function groupColor(group) {
    if (!group) return new THREE.Color('#7c88a6');
    if (!groupColorCache.has(group)) {
      let h = 0;
      for (const c of group) h = (h * 31 + c.charCodeAt(0)) >>> 0;
      const hue = GROUP_HUES[h % GROUP_HUES.length];
      groupColorCache.set(group, new THREE.Color().setHSL(hue / 360, 0.58, 0.66));
    }
    return groupColorCache.get(group);
  }

  // --- three.js scene --------------------------------------------------------

  const scene3d = (() => {
    const canvas = $('scene');
    const renderer = new THREE.WebGLRenderer({ canvas, antialias: true, alpha: false });
    renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
    renderer.outputColorSpace = THREE.SRGBColorSpace;

    const scene = new THREE.Scene();
    // Light enough to give depth, far from enough to grey out the middle of the
    // graph -- which is where whatever you are looking at usually is. Every
    // number below is set by `applyScale`, including this one.
    scene.fog = new THREE.FogExp2(0x08090d, 0);

    const camera = new THREE.PerspectiveCamera(52, 1, 0.5, 4000);
    camera.position.set(0, 12, 92);

    const controls = new THREE.OrbitControls(camera, canvas);
    controls.enableDamping = true;
    controls.dampingFactor = 0.08;
    controls.rotateSpeed = 0.7;
    /* Zoom goes where the pointer is, not where the orbit target happens to be.
     *
     * Dollying toward a fixed target is unusable on a graph of any size, because
     * the thing worth looking at is almost never the exact centre of the cloud.
     * Measured on a 683-entity brain: aiming at a node 141 units off-centre and
     * scrolling in twenty-five notches moved the camera 4.7x closer to the
     * target and left the distance from the target to that node at exactly
     * 141.56 -- unchanged, because nothing about scrolling could change it. You
     * rush past what you were pointing at and end up among edges you cannot
     * place. This makes the target follow the cursor, so zoom lands where you
     * are looking and `f` remains the way back out. */
    controls.zoomToCursor = true;

    /* Everything that has to know how big the graph got.
     *
     * A brain with twenty entities lays out inside a radius of about 97 and one
     * with fifteen hundred inside 294, so a fog density, a far plane or a zoom
     * stop that suits one is wrong for the other by a factor of three. Fixed
     * constants here are what made a large brain render as a black rectangle:
     * every node was inside the frustum and inside the viewport, and the fog had
     * eaten 99.96% of it. These are derived from the measured extent instead. */
    const FOG_FLOOR = 0.25;   // of the far side must survive at maximum zoom-out
    const FOG_K = Math.sqrt(-Math.log(FOG_FLOOR));
    const ZOOM_OUT = 2.0;     // how far past a snug fit the camera may pull back
    let extent = 0;

    function measureExtent() {
      let r = 0;
      for (const ent of view.nodes) {
        const p = sim.pos.get(ent.id);
        if (p) r = Math.max(r, Math.hypot(p.x, p.y, p.z));
      }
      return Math.max(r, 8);
    }

    /* The distance at which a sphere of radius r fills the frame.
     *
     * Both axes, not just the vertical field: a tall narrow window is bounded by
     * its width, and framing on the vertical alone crops the graph exactly when
     * there is least room for it. */
    function fitDistance(r) {
      const vt = Math.tan((camera.fov * Math.PI) / 360);
      return (r * 1.06) / Math.max(1e-3, Math.min(vt, vt * camera.aspect));
    }

    function applyScale(r) {
      extent = r;
      const fit = fitDistance(r);
      // Low enough to get inside a cluster and read it. With zoom following the
      // cursor there is somewhere worth being that close to.
      controls.minDistance = Math.max(0.6, r * 0.008);
      controls.maxDistance = fit * ZOOM_OUT;
      camera.far = (controls.maxDistance + r) * 1.6;
      camera.near = Math.max(0.1, camera.far / 4e4);
      camera.updateProjectionMatrix();
      // Depth cue, not a curtain. The density is pinned to the one place fog can
      // do real damage -- the far side of the graph, seen from as far out as the
      // controls allow -- and told to leave FOG_FLOOR of it standing there.
      scene.fog.density = FOG_K / (controls.maxDistance + r);
    }

    // A starting scale, so an empty brain and the first frames of a rebuild have
    // a coherent camera rather than an infinite one.
    applyScale(60);

    scene.add(new THREE.AmbientLight(0xffffff, 0.85));
    const key = new THREE.DirectionalLight(0xbfd4ff, 1.5);
    key.position.set(1, 1.4, 1);
    scene.add(key);
    const rim = new THREE.DirectionalLight(0x7fe8dc, 0.7);
    rim.position.set(-1, -0.6, -0.8);
    scene.add(rim);

    const SEGMENTS = 14;                 // samples per curved edge
    const nodeGeo = new THREE.IcosahedronGeometry(1, 3);
    const nodeMat = new THREE.MeshStandardMaterial({ roughness: 0.42, metalness: 0.08 });

    let nodes = null;                    // InstancedMesh
    let glow = null;                     // additive halo layer
    let lines = null;                    // LineSegments for every visible edge
    let radii = [];

    const glowTexture = (() => {
      const c = document.createElement('canvas');
      c.width = c.height = 128;
      const g = c.getContext('2d').createRadialGradient(64, 64, 0, 64, 64, 64);
      g.addColorStop(0, 'rgba(255,255,255,0.95)');
      g.addColorStop(0.28, 'rgba(255,255,255,0.30)');
      g.addColorStop(1, 'rgba(255,255,255,0)');
      const ctx = c.getContext('2d');
      ctx.fillStyle = g;
      ctx.fillRect(0, 0, 128, 128);
      return new THREE.CanvasTexture(c);
    })();

    const dummy = new THREE.Object3D();
    const tmpColor = new THREE.Color();

    /* Drops an object built by `rebuild`.
     *
     * The sphere geometry and its material are shared across every rebuild and
     * must survive one: disposing them frees the GPU upload, and rebuild runs on
     * every frame of a timeline scrub, so this would re-upload the mesh dozens of
     * times a second to no purpose. Only what this object owns is disposed. */
    function drop(obj, owned) {
      if (!obj) return;
      scene.remove(obj);
      if (owned) {
        obj.geometry.dispose();
        obj.material.dispose();
      }
    }

    function rebuild() {
      drop(nodes, false); drop(glow, true); drop(lines, true);
      nodes = glow = lines = null;
      const n = view.nodes.length;
      $('empty').hidden = n > 0;
      if (n === 0) return;

      // Log rather than linear: an entity with forty facts is more important
      // than one with four, but it is not ten times the sphere.
      radii = view.nodes.map((ent) => 1.15 + Math.log2(1 + ent.facts.length) * 0.7);

      nodes = new THREE.InstancedMesh(nodeGeo, nodeMat, n);
      nodes.instanceMatrix.setUsage(THREE.DynamicDrawUsage);
      scene.add(nodes);

      const glowGeo = new THREE.BufferGeometry();
      glowGeo.setAttribute('position', new THREE.Float32BufferAttribute(new Float32Array(n * 3), 3));
      glowGeo.setAttribute('color', new THREE.Float32BufferAttribute(new Float32Array(n * 3), 3));
      glowGeo.setAttribute('size', new THREE.Float32BufferAttribute(new Float32Array(n), 1));
      glow = new THREE.Points(glowGeo, new THREE.ShaderMaterial({
        uniforms: { map: { value: glowTexture }, scale: { value: innerHeight } },
        vertexShader: `
          attribute float size; varying vec3 vColor;
          uniform float scale;
          void main() {
            vColor = color;
            vec4 mv = modelViewMatrix * vec4(position, 1.0);
            gl_PointSize = size * scale / max(-mv.z, 1.0);
            gl_Position = projectionMatrix * mv;
          }`,
        fragmentShader: `
          uniform sampler2D map; varying vec3 vColor;
          void main() {
            vec4 t = texture2D(map, gl_PointCoord);
            gl_FragColor = vec4(vColor, 1.0) * t;
          }`,
        transparent: true,
        blending: THREE.AdditiveBlending,
        depthWrite: false,
        vertexColors: true,
      }));
      glow.frustumCulled = false;
      scene.add(glow);

      const verts = view.edges.length * SEGMENTS * 2;
      const lineGeo = new THREE.BufferGeometry();
      lineGeo.setAttribute('position', new THREE.Float32BufferAttribute(new Float32Array(verts * 3), 3));
      lineGeo.setAttribute('color', new THREE.Float32BufferAttribute(new Float32Array(verts * 3), 3));
      lines = new THREE.LineSegments(lineGeo, new THREE.LineBasicMaterial({
        vertexColors: true, transparent: true, opacity: 0.92, depthWrite: false,
      }));
      lines.frustumCulled = false;
      scene.add(lines);

      paint();
      // A rebuild is a filter change, not a request to move the camera. Toggling
      // "show retracted" on a graph the user has already framed should leave
      // their view where it is -- but the fog and the zoom stops still have to
      // follow the set that is now on screen.
      if (framed) applyScale(measureExtent());
      else frameCamera();
    }

    /* Colour is the only thing selection changes, so it is the only thing
     * recomputed on select -- geometry is left alone. */
    function paint() {
      if (!nodes) return;
      const hi = highlight();
      const colorAttr = glow.geometry.getAttribute('color');
      const sizeAttr = glow.geometry.getAttribute('size');

      view.nodes.forEach((ent, i) => {
        const lit = !hi || hi.nodes.has(ent.id);
        const isSel = ent.id === state.selected || ent.id === state.pathTarget;
        const channels = state.hitChannels.get(ent.id);

        let color;
        if (channels && channels.size) {
          // A node the last recall surfaced wears the channel that found it.
          // Several channels agreeing is exactly what RRF rewards, so the colours
          // are averaged rather than one winning.
          color = new THREE.Color(0, 0, 0);
          for (const ch of channels) color.add(new THREE.Color(COLORS[ch] || '#888'));
          color.multiplyScalar(1 / channels.size);
        } else {
          color = groupColor(ent.group).clone();
        }
        if (!lit) color.multiplyScalar(0.16);
        if (isSel) color.lerp(new THREE.Color('#ffffff'), 0.35);

        nodes.setColorAt(i, color);
        colorAttr.setXYZ(i, color.r, color.g, color.b);
        const halo = (isSel ? 5.2 : lit ? 2.6 : 0.9) * radii[i];
        sizeAttr.setX(i, halo * (channels && channels.size ? 1.5 : 1));
      });

      if (nodes.instanceColor) nodes.instanceColor.needsUpdate = true;
      colorAttr.needsUpdate = true;
      sizeAttr.needsUpdate = true;

      const lc = lines.geometry.getAttribute('color');
      view.edges.forEach((e, ei) => {
        const lit = !hi || hi.edges.has(e);
        tmpColor.set(COLORS[e.state] || COLORS.open);
        // WebGL caps line width at one pixel on every desktop driver, so an edge
        // can only be emphasised by brightness. The unlit floor stays well above
        // black: an invisible edge does not read as "not selected", it reads as
        // a graph with fewer relations than it has.
        if (!lit) tmpColor.multiplyScalar(0.3);
        else if (hi) tmpColor.lerp(new THREE.Color('#ffffff'), 0.32);
        else tmpColor.multiplyScalar(0.78);
        const base = ei * SEGMENTS * 2;
        for (let k = 0; k < SEGMENTS * 2; k++) {
          lc.setXYZ(base + k, tmpColor.r, tmpColor.g, tmpColor.b);
        }
      });
      lc.needsUpdate = true;
    }

    /* Positions only. Called every frame the layout is still moving. */
    function syncGeometry() {
      if (!nodes) return;
      view.nodes.forEach((ent, i) => {
        const p = sim.pos.get(ent.id);
        dummy.position.set(p.x, p.y, p.z);
        const s = radii[i] * (ent.id === state.selected ? 1.35 : 1);
        dummy.scale.setScalar(s);
        dummy.updateMatrix();
        nodes.setMatrixAt(i, dummy.matrix);
        glow.geometry.getAttribute('position').setXYZ(i, p.x, p.y, p.z);
      });
      nodes.instanceMatrix.needsUpdate = true;
      nodes.computeBoundingSphere();
      glow.geometry.getAttribute('position').needsUpdate = true;

      // Curved edges. Straight lines between the same pair of nodes overlap into
      // one stroke; an arc whose bulge is derived from the fact id keeps parallel
      // relations legible without a layout pass to separate them.
      const lp = lines.geometry.getAttribute('position');
      const mid = new THREE.Vector3(), ctrl = new THREE.Vector3();
      const a = new THREE.Vector3(), b = new THREE.Vector3(), off = new THREE.Vector3();
      view.edges.forEach((e, ei) => {
        const pa = sim.pos.get(e.src), pb = sim.pos.get(e.dst);
        a.set(pa.x, pa.y, pa.z); b.set(pb.x, pb.y, pb.z);
        mid.addVectors(a, b).multiplyScalar(0.5);
        off.subVectors(b, a);
        const len = off.length() || 1;
        // A stable pseudo-normal: cross with whichever axis it is least parallel
        // to, so the arc never collapses into the line itself.
        const axis = Math.abs(off.y / len) < 0.9 ? new THREE.Vector3(0, 1, 0) : new THREE.Vector3(1, 0, 0);
        off.cross(axis).normalize();
        const bulge = len * 0.12 * (((e.fact.id % 5) - 2) / 2 || 0.6);
        ctrl.copy(mid).addScaledVector(off, bulge);

        const base = ei * SEGMENTS * 2;
        // Each sample segment is drawn at 55% length when the edge is closed,
        // which turns the same buffer into a dashed ghost with no second object
        // and no `computeLineDistances`.
        const shrink = isLive(e.state) ? 1 : 0.55;
        for (let s = 0; s < SEGMENTS; s++) {
          const t0 = s / SEGMENTS, t1 = (s + 1) / SEGMENTS;
          const gap = (t1 - t0) * (1 - shrink) * 0.5;
          quad(a, ctrl, b, t0 + gap, mid);
          lp.setXYZ(base + s * 2, mid.x, mid.y, mid.z);
          quad(a, ctrl, b, t1 - gap, mid);
          lp.setXYZ(base + s * 2 + 1, mid.x, mid.y, mid.z);
        }
      });
      lp.needsUpdate = true;
    }

    function quad(p0, p1, p2, t, out) {
      const u = 1 - t;
      out.set(
        u * u * p0.x + 2 * u * t * p1.x + t * t * p2.x,
        u * u * p0.y + 2 * u * t * p1.y + t * t * p2.y,
        u * u * p0.z + 2 * u * t * p1.z + t * t * p2.z,
      );
      return out;
    }

    /* The layout inflates by an order of magnitude in its first second, so
     * framing it at rebuild time frames noise and the scale-dependent constants
     * have to be re-derived while it moves -- not once, at rest. The camera
     * follows only until the user touches it, because moving it under them would
     * be rude; `f` hands framing back. */
    let framed = false;
    controls.addEventListener('start', () => { framed = true; });

    function placeCamera() {
      const dist = fitDistance(extent);
      camera.position.set(0, dist * 0.16, dist);
      controls.target.set(0, 0, 0);
      controls.update();
    }

    /* Snap to the graph as it stands. For a rebuild and for `f`, where the point
     * is to arrive rather than to ease. */
    function frameCamera() {
      if (!view.nodes.length) return;
      applyScale(measureExtent());
      placeCamera();
    }

    /* Called every frame the layout is still moving.
     *
     * Eased rather than snapped, and that is not a flourish. The extent is a
     * maximum over every node, so it is precisely the statistic that one badly
     * placed node dominates; chasing it frame by frame let a layout blow-up drag
     * the camera across thirteen orders of magnitude at thirty-six reversals a
     * second. That blow-up is fixed in `step` above, but a follower that one
     * number can drive mad should not be the only thing between a bad frame and
     * an unusable screen. Easing bounds what any single frame can do to the
     * camera, whatever it claims the graph now measures. */
    const FOLLOW = 0.08;

    function trackExtent() {
      if (!view.nodes.length) return;
      const next = extent + (measureExtent() - extent) * FOLLOW;
      if (extent && Math.abs(next - extent) < extent * 0.004) return;
      applyScale(next);
      if (!framed) placeCamera();
    }

    function flyTo(entityId) {
      const p = sim.pos.get(entityId);
      if (!p) return;
      // Asking to go somewhere is a camera the user placed, even though no drag
      // happened. Without this the follower reclaims it the moment the layout
      // shifts, and a double-click during the first seconds does nothing.
      framed = true;
      const target = new THREE.Vector3(p.x, p.y, p.z);
      const dir = camera.position.clone().sub(controls.target).normalize();
      const from = { t: controls.target.clone(), c: camera.position.clone() };
      const to = { t: target, c: target.clone().addScaledVector(dir, 34) };
      const t0 = performance.now();
      (function ease() {
        const k = Math.min(1, (performance.now() - t0) / 520);
        const s = 1 - Math.pow(1 - k, 3);
        controls.target.lerpVectors(from.t, to.t, s);
        camera.position.lerpVectors(from.c, to.c, s);
        controls.update();
        if (k < 1) requestAnimationFrame(ease);
      })();
    }

    // --- picking ------------------------------------------------------------

    const raycaster = new THREE.Raycaster();
    const pointer = new THREE.Vector2();
    let hovered = null;

    function pick(event) {
      if (!nodes) return null;
      const rect = canvas.getBoundingClientRect();
      pointer.x = ((event.clientX - rect.left) / rect.width) * 2 - 1;
      pointer.y = -((event.clientY - rect.top) / rect.height) * 2 + 1;
      raycaster.setFromCamera(pointer, camera);
      const hit = raycaster.intersectObject(nodes, false)[0];
      return hit && hit.instanceId != null ? view.nodes[hit.instanceId] : null;
    }

    canvas.addEventListener('pointermove', (e) => {
      const ent = pick(e);
      if (ent !== hovered) {
        hovered = ent;
        canvas.style.cursor = ent ? 'pointer' : 'grab';
        showTooltip(ent, e);
      } else if (ent) {
        positionTooltip(e);
      }
    });

    canvas.addEventListener('pointerleave', () => { hovered = null; showTooltip(null); });

    let downAt = null;
    canvas.addEventListener('pointerdown', (e) => { downAt = { x: e.clientX, y: e.clientY }; });
    canvas.addEventListener('pointerup', (e) => {
      // Distinguish a click from the end of an orbit drag.
      if (!downAt || Math.hypot(e.clientX - downAt.x, e.clientY - downAt.y) > 4) return;
      const ent = pick(e);
      if (!ent) { select(null); return; }
      if (e.shiftKey && state.selected != null && state.selected !== ent.id) {
        state.pathTarget = ent.id;
        paint();
        renderInspector();
      } else {
        select(ent.id);
      }
    });

    canvas.addEventListener('dblclick', (e) => {
      const ent = pick(e);
      if (ent) flyTo(ent.id);
    });

    // --- labels -------------------------------------------------------------

    const labelHost = $('labels');
    const labelEls = new Map();
    const projected = new THREE.Vector3();

    function syncLabels() {
      const hi = highlight();
      const budget = 46;
      let shown = 0;

      // Ranked so the labels that survive the budget are the ones carrying the
      // most: whatever is selected, then its neighbourhood, then the busiest
      // nodes. Labelling everything at 300 nodes is labelling nothing.
      const ranked = state.showLabels
        ? view.nodes
            .map((ent, i) => ({ ent, i, rank: (hi && hi.nodes.has(ent.id) ? 1e6 : 0) + ent.facts.length }))
            .sort((a, b) => b.rank - a.rank)
        : [];

      const wanted = new Set();
      const placed = [];
      for (const { ent, i } of ranked) {
        if (shown >= budget && !(hi && hi.nodes.has(ent.id))) continue;
        const p = sim.pos.get(ent.id);
        projected.set(p.x, p.y, p.z).project(camera);
        if (projected.z > 1) continue;
        const x = (projected.x * 0.5 + 0.5) * canvas.clientWidth;
        const y = (-projected.y * 0.5 + 0.5) * canvas.clientHeight;
        const ly = y - radii[i] * 9 - 8;

        // Overlap rejection, from an estimated box rather than a measured one:
        // reading `offsetWidth` for every label every frame would force a layout
        // per frame, and two names printed on top of each other are unreadable
        // whether or not the estimate was exact.
        const halfW = ent.label.length * 3.2 + 5;
        const box = { x0: x - halfW, x1: x + halfW, y0: ly - 8, y1: ly + 8 };
        const forced = hi && hi.nodes.has(ent.id);
        const clash = placed.some((q) => box.x0 < q.x1 && box.x1 > q.x0 && box.y0 < q.y1 && box.y1 > q.y0);
        if (clash && !forced) continue;
        placed.push(box);

        let el = labelEls.get(ent.id);
        if (!el) {
          el = document.createElement('div');
          el.className = 'node-label';
          el.textContent = ent.label;
          labelHost.appendChild(el);
          labelEls.set(ent.id, el);
        }
        el.style.transform = `translate(-50%,-50%) translate(${x.toFixed(1)}px, ${ly.toFixed(1)}px)`;
        el.className = 'node-label' +
          (ent.id === state.selected || ent.id === state.pathTarget ? ' selected' : (hi && !hi.nodes.has(ent.id) ? ' muted' : ''));
        wanted.add(ent.id);
        shown++;
      }

      for (const [id, el] of labelEls) {
        if (!wanted.has(id)) { el.remove(); labelEls.delete(id); }
      }
    }

    // --- loop ---------------------------------------------------------------

    function resize() {
      const w = canvas.clientWidth, h = canvas.clientHeight;
      if (w === 0 || h === 0) return;
      renderer.setSize(w, h, false);
      camera.aspect = w / h;
      camera.updateProjectionMatrix();
      if (glow) glow.material.uniforms.scale.value = h;
      // The fit distance is a function of the aspect ratio, so narrowing the
      // window changes what "the whole graph" costs to see.
      if (extent) { if (framed) applyScale(extent); else frameCamera(); }
    }

    new ResizeObserver(resize).observe(canvas);
    resize();

    (function tick() {
      requestAnimationFrame(tick);
      const moving = sim.step();
      syncGeometry();
      if (moving) trackExtent();
      controls.update();
      syncLabels();
      renderer.render(scene, camera);
    })();

    // `f` resumes following rather than framing once: pressed while the layout is
    // still inflating, a single fit would be out of date by the next second.
    return { rebuild, paint, flyTo, fit: () => { framed = false; frameCamera(); } };
  })();

  // --- tooltip ---------------------------------------------------------------

  const tooltip = $('tooltip');

  function showTooltip(ent, event) {
    if (!ent) { tooltip.hidden = true; return; }
    const open = ent.facts.filter((f) => view.visibleFacts && view.visibleFacts.has(f.id)).length;
    const rel = ent.out.length + ent.in.length;
    tooltip.innerHTML = '';
    const b = document.createElement('b');
    b.textContent = ent.label;
    const meta = document.createElement('div');
    meta.className = 'mono';
    meta.textContent = `${ent.key} · ${open} visible fact${open === 1 ? '' : 's'} · ${rel} relation${rel === 1 ? '' : 's'}`;
    tooltip.append(b, meta);
    tooltip.hidden = false;
    if (event) positionTooltip(event);
  }

  function positionTooltip(event) {
    const stage = $('stage').getBoundingClientRect();
    const x = event.clientX - stage.left + 14;
    const y = event.clientY - stage.top + 14;
    tooltip.style.left = Math.min(x, stage.width - tooltip.offsetWidth - 8) + 'px';
    tooltip.style.top = Math.min(y, stage.height - tooltip.offsetHeight - 8) + 'px';
  }

  // --- selection -------------------------------------------------------------

  function select(id, opts = {}) {
    state.selected = id;
    state.pathTarget = null;
    state.selectedFact = null;
    scene3d.paint();
    renderInspector();
    if (id != null && opts.fly) scene3d.flyTo(id);
  }

  // --- inspector -------------------------------------------------------------

  const el = (tag, cls, text) => {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text != null) n.textContent = text;
    return n;
  };

  function objectText(f) {
    if (f.object_entity != null) return f.object_entity;
    if (f.object_num != null) return f.unit ? `${f.object_num} ${f.unit}` : String(f.object_num);
    return f.object_text ?? '';
  }

  const fmtDate = (t) =>
    t == null ? '—' : new Date(t).toISOString().slice(0, 10);

  function intervalText(f, v) {
    const from = fmtDate(f._from);
    if (v.retracted) return `${from} · retracted ${fmtDate(f._ret)} — never true`;
    if (v.future) return `${from} → ${v.to == null ? 'open' : fmtDate(v.to)} (not yet true)`;
    if (v.expiring) return `${from} → ${fmtDate(v.to)} (expires on its own)`;
    if (v.to != null) return `${from} → ${fmtDate(v.to)}`;
    return `${from} → now`;
  }

  function renderInspector() {
    const body = $('inspector-body');
    const empty = $('inspector-empty');
    if (state.selected == null || !entities.has(state.selected)) {
      body.hidden = true;
      empty.hidden = false;
      return;
    }
    empty.hidden = true;
    body.hidden = false;
    body.innerHTML = '';

    const ent = entities.get(state.selected);

    const head = el('div', 'ins-head');
    head.append(el('h3', null, ent.label), el('div', 'key', ent.key));
    if (ent.kind) head.append(el('span', 'kind', ent.kind));
    body.append(head);

    // names
    const names = el('div', 'ins-section');
    names.append(el('h4', null, 'names'));
    const pills = el('div', 'names');
    pills.append(el('span', 'name-pill primary', ent.key));
    for (const a of ent.aliases) {
      const p = el('span', `name-pill ${a.source}`, a.key);
      p.title = a.source === 'declared'
        ? 'declared — decides identity: later facts under this name land here'
        : 'learned — widens retrieval only, never moves a fact';
      pills.append(p);
    }
    names.append(pills);
    if (LIVE) names.append(aliasForm(ent));
    body.append(names);

    // facts, grouped by predicate
    const byPredicate = new Map();
    for (const f of ent.facts) {
      if (f._edge) continue;
      if (!byPredicate.has(f.predicate)) byPredicate.set(f.predicate, []);
      byPredicate.get(f.predicate).push(f);
    }

    if (byPredicate.size) {
      const section = el('div', 'ins-section');
      section.append(el('h4', null, 'facts'));
      for (const [pred, facts] of byPredicate) {
        const group = el('div', 'fact-group');
        const head = el('div', 'pred');
        head.append(el('span', null, pred));
        const meta = SNAP.predicates.find((p) => p.key === pred);
        if (meta) {
          const c = el('span', `card ${meta.cardinality}`, meta.cardinality);
          c.title = meta.cardinality === 'single'
            ? 'single-valued — a new value closes the previous one'
            : 'multi-valued — values coexist until retracted';
          head.append(c);
        }
        group.append(head);

        // Oldest first: a timeline read top to bottom.
        for (const f of facts.slice().sort((a, b) => a._from - b._from || a.id - b.id)) {
          const v = visible(f) || { to: f._to, retracted: f._ret != null, closed: f._to != null, future: false };
          const shown = view.visibleFacts && view.visibleFacts.has(f.id);
          const row = el('div', `fact ${factState(v)}${state.selectedFact === f.id ? ' sel' : ''}`);
          if (!shown) row.style.opacity = '.42';
          row.append(el('span', 'bar'));
          const val = el('div', 'val');
          val.append(el('span', 'v', objectText(f)));
          val.append(el('span', 'when', intervalText(f, v)));
          row.append(val, el('span', 'fid', `#${f.id}`));
          row.addEventListener('click', () => {
            state.selectedFact = state.selectedFact === f.id ? null : f.id;
            renderInspector();
          });
          group.append(row);
          if (state.selectedFact === f.id) group.append(whyPanel(f));
        }
        section.append(group);
      }
      body.append(section);
    }

    // relations
    const rels = [
      ...ent.out.map((f) => ({ f, dir: 'out', other: f.object_entity_id })),
      ...ent.in.map((f) => ({ f, dir: 'in', other: f.entity_id })),
    ];
    if (rels.length) {
      const section = el('div', 'ins-section');
      section.append(el('h4', null, `relations (${rels.length})`));
      for (const { f, dir, other } of rels) {
        const target = entities.get(other);
        if (!target) continue;
        const shown = view.visibleFacts && view.visibleFacts.has(f.id);
        const row = el('div', `rel${shown ? '' : ' closed'}`);
        row.append(el('span', 'dir', dir === 'out' ? '→' : '←'));
        row.append(el('span', 'via', f.predicate));
        row.append(el('span', 'to', target.label));
        row.title = shown ? intervalText(f, visible(f) || {}) : 'not visible at the current time cursor';
        row.addEventListener('click', () => select(other, { fly: true }));
        section.append(row);
      }
      body.append(section);
    }

    if (state.pathTarget != null) {
      const path = shortestPath(state.selected, state.pathTarget);
      const section = el('div', 'ins-section');
      section.append(el('h4', null, 'path'));
      if (!path) {
        section.append(el('p', 'dim', `no route to ${entities.get(state.pathTarget).label} at this instant`));
      } else {
        const chain = [...path.nodes].map((id) => entities.get(id).label);
        section.append(el('div', 'why-chain', chain.join('  ·  ')));
        section.append(el('p', 'note', `${path.edges.size} hop${path.edges.size === 1 ? '' : 's'}`));
      }
      body.append(section);
    }

    if (LIVE) body.append(editor(ent));
    else body.append(readonlyNote());
  }

  /* Provenance, from the snapshot rather than from `/api/why`.
   *
   * The same three pointers the endpoint would return -- what this closed, what
   * closed this -- are already here, and reading them locally means the panel
   * works identically in a static export. */
  function whyPanel(f) {
    const box = el('div', 'why');
    const step = (role, text) => {
      const row = el('div', 'why-step');
      row.append(el('span', 'role', role), el('span', null, text));
      return row;
    };

    const supersedes = SNAP.facts.find((o) => o.superseded_by === f.id);
    if (supersedes) box.append(step('supersedes', `#${supersedes.id} ${objectText(supersedes)} (${fmtDate(supersedes._from)})`));
    box.append(step('this', `#${f.id} ${objectText(f)}`));
    box.append(step('recorded', new Date(f._rec).toISOString().replace('T', ' ').slice(0, 19) + 'Z'));
    if (f.superseded_by != null) {
      const next = factById.get(f.superseded_by);
      if (next) box.append(step('closed by', `#${next.id} ${objectText(next)} (${fmtDate(next._from)})`));
    }
    if (f._ret != null) box.append(step('retracted', `${fmtDate(f._ret)} — this was never true`));
    if (f.source) box.append(step('source', f.source));
    if (f.locator) box.append(step('locator', JSON.stringify(f.locator)));
    if (f.reassert_count) box.append(step('reasserted', `${f.reassert_count}×`));
    if (f.confidence !== 1) box.append(step('confidence', String(f.confidence)));

    if (LIVE) {
      const btn = el('button', 'danger', 'retract');
      btn.title = 'mark as never having been true — not the same as it having ended';
      btn.style.marginTop = '8px';
      btn.addEventListener('click', () => {
        const reason = prompt(`Retract #${f.id}?\n\n"${f.statement}"\n\nThis says the fact was never true. If it merely changed, record the new value instead.\n\nReason (optional):`);
        if (reason === null) return;
        post('/api/retract', { fact_id: f.id, reason: reason || null });
      });
      box.append(btn);
    }
    return box;
  }

  function readonlyNote() {
    const box = el('div', 'readonly-note');
    box.append(document.createTextNode('This is a snapshot taken '));
    box.append(el('span', 'mono', new Date(ms(SNAP.generated_at)).toISOString().slice(0, 19).replace('T', ' ') + 'Z'));
    box.append(document.createTextNode('. To edit the brain, run '));
    const code = el('code', null, 'brain studio');
    box.append(code, document.createTextNode('.'));
    return box;
  }

  // --- editing ---------------------------------------------------------------

  function editor(ent) {
    const box = el('div', 'editor');
    box.append(el('h4', null, 'record'));

    const form = document.createElement('form');
    const row1 = el('div', 'row');
    const pred = document.createElement('input');
    pred.placeholder = 'predicate';
    pred.setAttribute('list', 'predicate-options');
    const value = document.createElement('input');
    value.placeholder = 'value';
    row1.append(pred, value);

    const row2 = el('div', 'row');
    const kind = document.createElement('select');
    for (const [v, t] of [['value', 'literal'], ['entity', '→ entity']]) {
      const o = document.createElement('option');
      o.value = v; o.textContent = t;
      kind.append(o);
    }
    const at = document.createElement('input');
    at.placeholder = 'valid from (YYYY-MM-DD)';
    const until = document.createElement('input');
    until.placeholder = 'until (optional)';
    until.title = 'When it stops being true. The fact closes itself then, with nobody having to come back for it.';
    const submit = el('button', null, 'record');
    submit.type = 'submit';
    row2.append(kind, at, until, submit);

    form.append(row1, row2);
    form.addEventListener('submit', (e) => {
      e.preventDefault();
      if (!pred.value.trim() || !value.value.trim()) return;
      const payload = {
        subject: ent.key,
        predicate: pred.value.trim(),
        at: at.value.trim() || null,
        until: until.value.trim() || null,
      };
      payload[kind.value === 'entity' ? 'entity' : 'value'] = value.value.trim();
      post('/api/remember', payload).then(() => {
        pred.value = ''; value.value = ''; at.value = ''; until.value = '';
      });
    });
    box.append(form);

    const linkForm = document.createElement('form');
    const lrow = el('div', 'row');
    const rel = document.createElement('input');
    rel.placeholder = 'relation';
    const to = document.createElement('input');
    to.placeholder = 'to entity';
    const lbtn = el('button', null, 'link');
    lbtn.type = 'submit';
    lrow.append(rel, to, lbtn);
    linkForm.append(lrow);
    linkForm.addEventListener('submit', (e) => {
      e.preventDefault();
      if (!rel.value.trim() || !to.value.trim()) return;
      post('/api/link', { from: ent.key, rel: rel.value.trim(), to: to.value.trim() })
        .then(() => { rel.value = ''; to.value = ''; });
    });
    box.append(el('h4', null, 'link'), linkForm);

    const msg = el('div', 'editor-msg');
    msg.id = 'editor-msg';
    box.append(msg);
    return box;
  }

  function aliasForm(ent) {
    const form = document.createElement('form');
    form.style.marginTop = '8px';
    form.className = 'editor';
    form.style.borderTop = 'none';
    form.style.paddingTop = '0';
    const row = el('div', 'row');
    const input = document.createElement('input');
    input.placeholder = 'declare another name';
    const btn = el('button', null, 'add');
    btn.type = 'submit';
    row.append(input, btn);
    form.append(row);
    form.addEventListener('submit', (e) => {
      e.preventDefault();
      if (!input.value.trim()) return;
      post('/api/alias', { entity: ent.key, alias: input.value.trim() }).then(() => { input.value = ''; });
    });
    return form;
  }

  async function post(path, body) {
    const msg = $('editor-msg');
    const say = (text, cls) => { if (msg) { msg.textContent = text; msg.className = `editor-msg ${cls}`; } };
    try {
      const res = await fetch(path, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'X-Brain-Token': TOKEN },
        body: JSON.stringify(body),
      });
      const data = await res.json();
      if (!res.ok) throw new Error(data.error || res.statusText);
      if (data.snapshot) applySnapshot(data.snapshot);
      // The kind of write is the interesting part: "superseded" means something
      // was closed that the caller may not have realised was open.
      say(data.outcome ? `${data.outcome.kind}` : 'done', 'ok');
      return data;
    } catch (e) {
      say(String(e.message || e), 'err');
      throw e;
    }
  }

  /* Replaces the model in place after a write.
   *
   * Positions survive because `sim.pos` is keyed by entity id and only ever
   * gains entries -- an edit that adds one node must not reshuffle the twenty
   * the user was already looking at. */
  function applySnapshot(next) {
    SNAP.entities = next.entities;
    SNAP.predicates = next.predicates;
    SNAP.facts = next.facts;
    SNAP.generated_at = next.generated_at;
    SNAP.lint = next.lint;
    ingest(SNAP);
    computeDomains();
    renderPredicates();
    // An edit can close a finding or open one -- linking the last loose entity
    // should empty this panel without a reload.
    renderHealth();
    rebuildView();
  }

  // --- recall ----------------------------------------------------------------

  $('recall-form').addEventListener('submit', (e) => {
    e.preventDefault();
    runRecall($('recall-input').value.trim());
  });

  async function runRecall(query) {
    const list = $('recall-hits');
    list.innerHTML = '';
    state.hitChannels = new Map();
    // Asking a question puts the set query away. Showing a ranking and a complete
    // set in the same list would make the one property that separates them -- that
    // the set is all of it -- unreadable.
    if (state.which != null) { state.which = null; renderPredicates(); }
    if (!query) { scene3d.paint(); return; }

    if (!LIVE) {
      // No server, no channels. A substring filter is not recall and is not
      // dressed up as one: fusing four retrievers is the brain's job, and
      // pretending otherwise here would misrepresent the thing being debugged.
      const needle = query.toLowerCase();
      const found = SNAP.facts.filter((f) => f.statement.toLowerCase().includes(needle)).slice(0, 25);
      for (const f of found) {
        state.hitChannels.set(f.entity_id, new Set(['match']));
        list.append(hitRow(f, null, ['match']));
      }
      $('recall-note').textContent = found.length
        ? `${found.length} literal match${found.length === 1 ? '' : 'es'} — run \`brain studio\` for real four-channel recall`
        : 'no literal match — this snapshot can only filter text';
      scene3d.paint();
      return;
    }

    $('recall-note').textContent = 'recalling…';
    try {
      const res = await fetch('/api/recall', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'X-Brain-Token': TOKEN },
        body: JSON.stringify({ query, limit: 20, history: state.mode === 'history',
          as_of: state.mode === 'asof' ? new Date(state.asOf).toISOString() : null }),
      });
      const data = await res.json();
      if (!res.ok) throw new Error(data.error || res.statusText);

      for (const hit of data.hits) {
        const f = factById.get(hit.fact.id);
        if (f) {
          const set = state.hitChannels.get(f.entity_id) || new Set();
          for (const c of hit.channels) set.add(c);
          state.hitChannels.set(f.entity_id, set);
        }
        list.append(hitRow(hit.fact, hit.score, hit.channels));
      }
      $('recall-note').textContent = data.hits.length
        ? `${data.hits.length} hits · colour on the graph is the channel that found it`
        : 'nothing — no channel cleared its floor';
    } catch (e) {
      $('recall-note').textContent = String(e.message || e);
    }
    scene3d.paint();
  }

  function hitRow(fact, score, channels) {
    const li = el('li', 'hit');
    li.append(el('span', 'statement', fact.statement));
    const meta = el('div', 'meta');
    for (const c of channels) meta.append(el('span', `chip ${c}`, c));
    if (score != null) meta.append(el('span', 'score', score.toFixed(4)));
    li.append(meta);
    li.addEventListener('click', () => {
      select(fact.entity_id ?? entityIdOf(fact), { fly: true });
      state.selectedFact = fact.id;
      renderInspector();
    });
    return li;
  }

  // The recall endpoint returns the CLI's `Fact`, which carries `entity_key`
  // rather than the numeric id the graph is built on.
  function entityIdOf(fact) {
    for (const e of entities.values()) if (e.key === fact.entity_key) return e.id;
    return null;
  }

  // --- the time axes ---------------------------------------------------------

  function setMode(mode) {
    state.mode = mode;
    for (const b of document.querySelectorAll('.time-modes button')) {
      b.classList.toggle('active', b.dataset.mode === mode);
    }
    rebuildView();
    updateReadout();
  }

  for (const b of document.querySelectorAll('.time-modes button')) {
    b.addEventListener('click', () => setMode(b.dataset.mode));
  }

  function updateReadout() {
    const parts = [];
    if (state.mode === 'now') parts.push('true now');
    else if (state.mode === 'asof') parts.push('valid ' + new Date(state.asOf).toISOString().slice(0, 10));
    else parts.push('whole trajectory');
    if (state.knownAt < TX.hi - 1) {
      parts.push('as known ' + new Date(state.knownAt).toISOString().slice(0, 19).replace('T', ' ') + 'Z');
    }
    $('time-readout').textContent = parts.join('  ·  ');
  }

  const scrub = $('scrub');
  const scrubCtx = scrub.getContext('2d');

  function fitCanvas(canvas) {
    const r = Math.min(devicePixelRatio, 2);
    const w = canvas.clientWidth, h = canvas.clientHeight;
    if (canvas.width !== w * r || canvas.height !== h * r) {
      canvas.width = w * r; canvas.height = h * r;
    }
    const ctx = canvas.getContext('2d');
    ctx.setTransform(r, 0, 0, r, 0, 0);
    ctx.clearRect(0, 0, w, h);
    return { w, h, ctx };
  }

  const xOf = (t, w, pad) => pad + ((t - DOMAIN.lo) / (DOMAIN.hi - DOMAIN.lo)) * (w - pad * 2);
  const tOf = (x, w, pad) => DOMAIN.lo + ((x - pad) / (w - pad * 2)) * (DOMAIN.hi - DOMAIN.lo);

  /* The valid-time ruler: one lane per predicate-bearing fact, so a supersession
   * reads as a baton pass rather than as two unrelated bars. */
  function drawScrub() {
    const { w, h, ctx } = fitCanvas(scrub);
    if (w === 0) return;
    const pad = 10;
    const top = 14, bottom = h - 16;

    ctx.strokeStyle = '#1d2130';
    ctx.lineWidth = 1;
    const ticks = 6;
    ctx.font = '10px ui-monospace, monospace';
    ctx.fillStyle = '#4c5364';
    for (let i = 0; i <= ticks; i++) {
      const t = DOMAIN.lo + ((DOMAIN.hi - DOMAIN.lo) * i) / ticks;
      const x = xOf(t, w, pad);
      ctx.beginPath();
      ctx.moveTo(x, top - 6); ctx.lineTo(x, bottom + 4);
      ctx.stroke();
      ctx.fillText(new Date(t).toISOString().slice(0, 10), x + 3, h - 4);
    }

    const rows = SNAP.facts.filter((f) => state.showRetracted || f._ret == null);
    if (!rows.length) return;
    const lane = Math.max(2, Math.min(9, (bottom - top) / rows.length));

    rows.forEach((f, i) => {
      const y = top + i * lane;
      if (y > bottom) return;
      const k = asKnownAt(f, state.knownAt);
      if (k === null) return;
      const x0 = xOf(f._from, w, pad);
      const x1 = xOf(k.to == null ? DOMAIN.hi : k.to, w, pad);
      const st = factState({ retracted: k.retracted, to: k.to, closed: k.to != null,
                             future: !k.retracted && f._from > NOW,
                             expiring: !k.retracted && k.to != null && NOW < k.to });
      ctx.strokeStyle = COLORS[st];
      ctx.globalAlpha = st === 'closed' ? 0.5 : st === 'retracted' ? 0.75 : 0.9;
      ctx.lineWidth = Math.max(1.5, lane - 2.5);
      ctx.beginPath();
      ctx.moveTo(x0, y); ctx.lineTo(Math.max(x1, x0 + 1.5), y);
      ctx.stroke();
      if (k.retracted) {
        ctx.strokeStyle = '#08090d';
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.moveTo(x0, y); ctx.lineTo(Math.max(x1, x0 + 1.5), y);
        ctx.stroke();
      }
      ctx.globalAlpha = 1;
    });

    if (state.mode === 'asof') {
      const x = xOf(state.asOf, w, pad);
      ctx.strokeStyle = COLORS.open;
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.moveTo(x, 2); ctx.lineTo(x, bottom + 6);
      ctx.stroke();
      ctx.fillStyle = COLORS.open;
      ctx.beginPath();
      ctx.moveTo(x, 2); ctx.lineTo(x - 4, 9); ctx.lineTo(x + 4, 9);
      ctx.fill();
    }
  }

  function scrubTo(clientX) {
    const rect = scrub.getBoundingClientRect();
    state.asOf = Math.max(DOMAIN.lo, Math.min(DOMAIN.hi, tOf(clientX - rect.left, rect.width, 10)));
    if (state.mode !== 'asof') setMode('asof');
    else { rebuildView(); updateReadout(); }
  }

  let scrubbing = false;
  scrub.addEventListener('pointerdown', (e) => {
    scrubbing = true;
    scrub.setPointerCapture(e.pointerId);
    scrubTo(e.clientX);
  });
  scrub.addEventListener('pointermove', (e) => { if (scrubbing) scrubTo(e.clientX); });
  scrub.addEventListener('pointerup', () => { scrubbing = false; });

  /* The bitemporal plane.
   *
   * x is when a fact was true, y is when the brain was told. That second axis is
   * what makes the three write outcomes look like three different things: a
   * supersession is a bar that stops and a new bar starting higher up; a
   * correction is a struck bar with nothing taking its place on the x axis; a
   * reassertion is one bar that got thicker instead of a second bar. */
  const plane = $('plane');

  function drawPlane() {
    const { w, h, ctx } = fitCanvas(plane);
    if (w === 0) return;
    const pad = 8, top = 8, bottom = h - 10;
    const yOf = (t) => bottom - ((t - TX.lo) / (TX.hi - TX.lo)) * (bottom - top);

    ctx.strokeStyle = '#161a26';
    ctx.lineWidth = 1;
    for (let i = 0; i <= 4; i++) {
      const y = top + ((bottom - top) * i) / 4;
      ctx.beginPath(); ctx.moveTo(pad, y); ctx.lineTo(w - pad, y); ctx.stroke();
      const x = pad + ((w - pad * 2) * i) / 4;
      ctx.beginPath(); ctx.moveTo(x, top); ctx.lineTo(x, bottom); ctx.stroke();
    }

    for (const f of SNAP.facts) {
      if (f._ret != null && !state.showRetracted) continue;
      const y = yOf(f._rec);
      const x0 = xOf(f._from, w, pad);
      const x1 = xOf(f._to == null ? DOMAIN.hi : f._to, w, pad);
      const st = factState({ retracted: f._ret != null, to: f._to, closed: f._to != null,
                             future: f._ret == null && f._from > NOW,
                             expiring: f._ret == null && f._to != null && NOW < f._to });
      ctx.strokeStyle = COLORS[st];
      ctx.globalAlpha = f._rec > state.knownAt ? 0.13 : st === 'closed' ? 0.55 : 0.9;
      ctx.lineWidth = Math.min(6, 1.5 + f.reassert_count * 1.2);
      ctx.beginPath();
      ctx.moveTo(x0, y); ctx.lineTo(Math.max(x1, x0 + 2), y);
      ctx.stroke();
      if (f._ret != null) {
        ctx.strokeStyle = COLORS.retracted;
        ctx.globalAlpha = 0.9;
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.moveTo(x0 - 2, y - 3); ctx.lineTo(Math.max(x1, x0 + 2) + 2, y + 3);
        ctx.stroke();
      }
      ctx.globalAlpha = 1;
    }

    // The cursors: vertical is the as-of instant, horizontal is how much of the
    // brain's own history has been revealed.
    if (state.mode === 'asof') {
      ctx.strokeStyle = COLORS.open;
      ctx.globalAlpha = 0.7;
      ctx.lineWidth = 1;
      const x = xOf(state.asOf, w, pad);
      ctx.beginPath(); ctx.moveTo(x, top); ctx.lineTo(x, bottom); ctx.stroke();
    }
    ctx.strokeStyle = COLORS.semantic;
    ctx.globalAlpha = state.knownAt < TX.hi - 1 ? 0.85 : 0.25;
    ctx.setLineDash([3, 3]);
    const ky = yOf(state.knownAt);
    ctx.beginPath(); ctx.moveTo(pad, ky); ctx.lineTo(w - pad, ky); ctx.stroke();
    ctx.setLineDash([]);
    ctx.globalAlpha = 1;

    // The y axis is only as tall as the brain's own writing history, which for a
    // brain filled by a script is a fraction of a second. Saying so is what stops
    // a flat-looking plane from reading as a bug.
    const span = TX.hi - TX.lo;
    $('plane-range').textContent =
      span < 90e3 ? `told over ${(span / 1000).toFixed(1)}s`
      : span < 86400e3 ? `told over ${(span / 3600e3).toFixed(1)}h`
      : `told ${new Date(TX.lo).toISOString().slice(0, 10)} → ${new Date(TX.hi).toISOString().slice(0, 10)}`;
  }

  let planeDrag = false;
  function planeTo(e) {
    const rect = plane.getBoundingClientRect();
    const top = 8, bottom = rect.height - 10;
    const y = Math.max(top, Math.min(bottom, e.clientY - rect.top));
    state.knownAt = TX.lo + ((bottom - y) / (bottom - top)) * (TX.hi - TX.lo);
    state.asOf = Math.max(DOMAIN.lo, Math.min(DOMAIN.hi, tOf(e.clientX - rect.left, rect.width, 8)));
    if (state.mode !== 'asof') setMode('asof');
    else { rebuildView(); updateReadout(); }
  }
  plane.addEventListener('pointerdown', (e) => { planeDrag = true; plane.setPointerCapture(e.pointerId); planeTo(e); });
  plane.addEventListener('pointermove', (e) => { if (planeDrag) planeTo(e); });
  plane.addEventListener('pointerup', () => { planeDrag = false; });
  plane.addEventListener('dblclick', () => { state.knownAt = TX.hi; rebuildView(); updateReadout(); });

  // --- chrome ----------------------------------------------------------------

  function updateCounts() {
    const visibleFacts = view.visibleFacts ? view.visibleFacts.size : 0;
    $('counts').innerHTML = '';
    const add = (n, label) => {
      const d = document.createElement('div');
      d.append(el('b', null, String(n)), el('span', null, label));
      $('counts').append(d);
    };
    add(view.nodes.length, `/ ${entities.size} entities`);
    add(view.edges.length, 'relations');
    add(visibleFacts, `/ ${SNAP.facts.length} facts`);
  }

  function renderPredicates() {
    const list = $('predicate-list');
    list.innerHTML = '';
    const counts = new Map();
    for (const f of SNAP.facts) counts.set(f.predicate, (counts.get(f.predicate) || 0) + 1);
    for (const p of SNAP.predicates) {
      const li = document.createElement('li');
      if (state.which === p.key) li.classList.add('sel');
      li.append(el('span', null, p.key));

      const card = el('span', `card ${p.cardinality}`, p.cardinality === 'single' ? '1' : 'n');
      card.title = p.declared ? 'declared by hand' : 'observed';
      li.append(card);

      // Whether the object names a thing. The snapshot has carried the
      // cardinality since there was a snapshot and never this, which is the more
      // consequential half: a relation stored as a string reads back perfectly
      // and no walk can follow it, and that is the defect this whole viewer was
      // built to make visible.
      if (p.relational) {
        const rel = el('span', 'card rel', '→');
        rel.title = 'relational — the object names an entity, so the graph can be walked through it';
        li.append(rel);
      }

      li.append(el('span', 'n', String(counts.get(p.key) || 0)));
      li.tabIndex = 0;
      li.title = `which ${p.key} — every subject holding it`;
      li.addEventListener('click', () => runWhich(p.key));
      li.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); runWhich(p.key); }
      });
      list.append(li);
    }
  }

  /* `brain which <predicate>`, in the viewer.
   *
   * The one question the graph cannot be read for. A node shows what it holds, and
   * nothing shows which nodes hold a given thing -- so "what is open", "who owns
   * something", "what has no source" were invisible here for the same reason they
   * were unaskable at the CLI. Clicking a predicate answers it.
   *
   * Distinct from recall on purpose: recall ranks and truncates, and cannot say
   * what it left out. This is the whole set, at the cursor, and the count is the
   * count. */
  function runWhich(predicate) {
    state.which = state.which === predicate ? null : predicate;
    state.hitChannels = new Map();
    const list = $('recall-hits');
    list.innerHTML = '';

    if (state.which == null) {
      $('recall-note').textContent = '';
      renderPredicates();
      scene3d.paint();
      return;
    }

    const found = [];
    for (const f of SNAP.facts) {
      if (f.predicate !== predicate) continue;
      if (!visible(f)) continue;
      found.push(f);
      state.hitChannels.set(f.entity_id, new Set(['set']));
    }
    // By subject, so the list reads as a list of things rather than of rows.
    found.sort((a, b) => (a.entity_key < b.entity_key ? -1 : a.entity_key > b.entity_key ? 1 : a.id - b.id));

    for (const f of found) list.append(hitRow(f, null, ['set']));
    const subjects = new Set(found.map((f) => f.entity_id)).size;
    $('recall-note').textContent = found.length
      ? `which ${predicate} — ${subjects} subject${subjects === 1 ? '' : 's'}, all of them`
      : `which ${predicate} — nothing holds it at this instant`;

    renderPredicates();
    scene3d.paint();
  }

  // The panel that explains the loose dots.
  //
  // A brain whose relations were written as strings looks, in here, exactly like
  // a brain that simply has few relations -- a field of unconnected nodes, with
  // nothing to say which it is. The findings come from the snapshot rather than
  // being recomputed here, so this panel and `brain lint` can never disagree.
  function renderHealth() {
    const L = SNAP.lint;
    if (!L) return;
    const list = $('health-list');
    const items = [];

    for (const m of L.mixed_predicates || []) {
      items.push({
        n: m.as_text,
        head: `${m.predicate} — ${m.as_text} stored as text, ${m.as_entity} as relation`,
        note: m.examples.length
          ? `the text ones point at nothing: ${m.examples.join(', ')}`
          : 'the text ones are unreachable by any walk',
      });
    }
    if ((L.candidate_classes || []).length) {
      const top = L.candidate_classes.slice(0, 4)
        .map((c) => `${c.value} (${c.entities})`).join(', ');
      items.push({
        n: L.candidate_classes.length,
        head: `${L.candidate_classes.length} shared values that are not nodes`,
        note: `several entities have these in common, and nothing can be reached through them: ${top}`,
      });
    }
    if ((L.orphans || []).length) {
      items.push({
        n: L.orphans.length,
        head: `${L.orphans.length} of ${L.entities} entities have no open relation`,
        note: 'drawn here only while “show unlinked entities” is on',
      });
    }
    for (const t of L.twins || []) {
      items.push({
        n: 1,
        head: `${t.a} ↔ ${t.b}`,
        note: `${t.distance} edit${t.distance === 1 ? '' : 's'} apart — possibly one thing under two names`,
      });
    }

    $('health-panel').hidden = items.length === 0;
    list.innerHTML = '';
    for (const it of items) {
      const li = document.createElement('li');
      li.append(el('span', 'health-head', it.head), el('small', null, it.note));
      list.append(li);
    }
  }

  function renderLegends() {
    const channels = [
      ['bm25', 'words, over FTS5'],
      ['alias', 'a name the question used'],
      ['semantic', 'meaning, not words'],
      ['graph', 'reached by walking relations'],
    ];
    const cl = $('channel-legend');
    for (const [key, desc] of channels) {
      const li = document.createElement('li');
      const sw = el('i', 'swatch');
      sw.style.background = COLORS[key];
      li.append(sw, el('span', null, key), el('small', null, desc));
      cl.append(li);
    }

    const edges = [
      ['open', 'still true'],
      ['closed', 'ended — drawn dashed'],
      ['expiring', 'true now, ends on its own'],
      ['future', 'open, not yet true'],
      ['retracted', 'never true'],
    ];
    const eg = $('edge-legend');
    for (const [key, desc] of edges) {
      const li = document.createElement('li');
      const sw = el('i', 'swatch line');
      sw.style.background = COLORS[key];
      li.append(sw, el('span', null, key), el('small', null, desc));
      eg.append(li);
    }
  }

  $('opt-retracted').addEventListener('change', (e) => { state.showRetracted = e.target.checked; rebuildView(); });
  $('opt-isolated').addEventListener('change', (e) => { state.showIsolated = e.target.checked; rebuildView(); });
  $('opt-labels').addEventListener('change', (e) => { state.showLabels = e.target.checked; });

  $('btn-help').addEventListener('click', () => { $('help').hidden = false; });
  $('help-close').addEventListener('click', () => { $('help').hidden = true; });
  $('help').addEventListener('click', (e) => { if (e.target.id === 'help') $('help').hidden = true; });

  addEventListener('keydown', (e) => {
    if (e.target.tagName === 'INPUT' || e.target.tagName === 'SELECT') {
      if (e.key === 'Escape') e.target.blur();
      return;
    }
    if (e.key === 'Escape') { $('help').hidden = true; select(null); }
    else if (e.key === '/') { e.preventDefault(); $('recall-input').focus(); }
    else if (e.key === '?') $('help').hidden = false;
    else if (e.key === 'f') scene3d.fit();
    else if (e.key === '1') setMode('now');
    else if (e.key === '2') setMode('asof');
    else if (e.key === '3') setMode('history');
  });

  addEventListener('resize', () => { drawScrub(); drawPlane(); });

  // --- boot ------------------------------------------------------------------

  $('brain-label').textContent = SNAP.brain_label;
  // Elided from the front, because the end of a path is the part that
  // identifies the brain and the beginning is the part that is always the same.
  $('brain-path').textContent =
    SNAP.brain_path.length > 58 ? '…' + SNAP.brain_path.slice(-57) : SNAP.brain_path;
  $('brain-path').title = SNAP.brain_path;
  const badge = $('mode-badge');
  badge.textContent = LIVE ? 'live' : 'snapshot';
  badge.className = 'badge ' + (LIVE ? 'live' : 'snapshot');
  badge.title = LIVE
    ? 'connected to a running `brain studio` — edits are written to the brain file'
    : 'a static export — read only';

  const datalist = document.createElement('datalist');
  datalist.id = 'predicate-options';
  for (const p of SNAP.predicates) {
    const o = document.createElement('option');
    o.value = p.key;
    datalist.append(o);
  }
  document.body.append(datalist);

  renderLegends();
  renderPredicates();
  renderHealth();
  rebuildView();
  updateReadout();
  setTimeout(() => { const h = document.querySelector('.hint'); if (h) h.style.opacity = '0'; }, 6000);
})();
