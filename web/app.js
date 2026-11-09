// The playground: an editor, the Spola VM compiled to WebAssembly, and the effects a browser can perform.

const EXAMPLES = [
  { file: "countdown.sp", title: "Countdown (kill it mid-sleep)" },
  { file: "approval.sp", title: "Approval (waits for a human)" },
  { file: "todo.sp", title: "Todo assistant (long-lived state)" },
  { file: "retry.sp", title: "Flaky service with retries" },
  { file: "fib.sp", title: "Closures and recursion" },
];
const SPEEDS = [
  [1, "crawl"],
  [4, "slow"],
  [40, "walk"],
  [400, "fast"],
  [5000, "faster"],
  [250000, "max"],
];
const STORE_KEY = "spola.checkpoint";

const $ = (id) => document.getElementById(id);
const enc = new TextEncoder();
const dec = new TextDecoder();

const wasm = (await WebAssembly.instantiateStreaming(fetch("spola.wasm"))).instance.exports;

function put(fn, data, ...rest) {
  const bytes = typeof data === "string" ? enc.encode(data) : data;
  const ptr = wasm.alloc(bytes.length);
  new Uint8Array(wasm.memory.buffer, ptr, bytes.length).set(bytes);
  return fn(ptr, bytes.length, ...rest);
}
const resultBytes = () => new Uint8Array(wasm.memory.buffer, wasm.result_ptr(), wasm.result_len()).slice();
const resultJson = () => JSON.parse(dec.decode(resultBytes()));

// ---------------------------------------------------------------- state

const s = {
  loaded: false, // a program is in the VM
  playing: false, // the animation loop is driving the VM
  busy: false, // an effect is being performed (sleep timer, ask form)
  killed: false,
  view: null,
  program: null,
  frozen: "", // console HTML from before the current session (kills, restores)
  tail: "", // console HTML after the current output (finished, failed...)
  sessionStart: 0,
  cutAt: null,
  lostFrom: null,
  knots: new Set(),
  ckpt: null, // bytes of the latest checkpoint: what survives a crash
  ckptStep: -1,
  ckptSize: 0,
  timer: null,
  exampleName: "program",
  errorLine: null,
  prevLocals: new Map(),
};

// ---------------------------------------------------------------- editor

const KEYWORDS = new Set("let fn return if else while for in break continue true false nil and or not".split(" "));

function esc(t) {
  return t.replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[c]);
}

function highlightLine(line) {
  let out = "";
  let i = 0;
  let afterAwait = false;
  let afterFn = false;
  while (i < line.length) {
    const rest = line.slice(i);
    let m;
    if (rest.startsWith("//")) {
      out += `<span class="t-com">${esc(rest)}</span>`;
      break;
    } else if (rest[0] === '"') {
      let j = 1;
      let depth = 0;
      let html = '<span class="t-str">"';
      let seg = "";
      while (j < rest.length) {
        const c = rest[j];
        if (c === "\\" && depth === 0) {
          seg += rest.slice(j, j + 2);
          j += 2;
          continue;
        }
        if (c === "{") {
          if (depth === 0) {
            html += esc(seg) + "{</span>";
            seg = "";
          } else seg += c;
          depth++;
        } else if (c === "}" && depth > 0) {
          depth--;
          if (depth === 0) {
            html += highlightLine(seg) + '<span class="t-str">}';
            seg = "";
          } else seg += c;
        } else if (c === '"' && depth === 0) {
          j++;
          break;
        } else seg += c;
        j++;
      }
      html += (depth > 0 ? highlightLine(seg) : esc(seg)) + (rest[j - 1] === '"' && depth === 0 ? '"' : "") + "</span>";
      out += html;
      i += j;
      continue;
    } else if ((m = rest.match(/^\d[\d_]*(\.\d+)?/))) {
      out += `<span class="t-num">${m[0]}</span>`;
    } else if ((m = rest.match(/^[A-Za-z_]\w*/))) {
      const w = m[0];
      if (w === "await") {
        out += `<span class="t-await">await</span>`;
        afterAwait = true;
        i += w.length;
        continue;
      }
      if (afterAwait) out += `<span class="t-await">${w}</span>`;
      else if (KEYWORDS.has(w)) out += `<span class="t-kw">${w}</span>`;
      else if (afterFn || rest.slice(w.length).startsWith("(")) out += `<span class="t-fn">${w}</span>`;
      else out += w;
      afterFn = w === "fn";
      afterAwait = false;
      i += w.length;
      continue;
    } else {
      m = [rest[0]];
      out += esc(rest[0]);
      if (rest[0] !== " ") afterAwait = false;
    }
    i += m[0].length;
  }
  return out;
}

function renderEditor() {
  const lines = $("source").value.split("\n");
  const cur = s.view && s.loaded && !["done"].includes(s.view.status) ? s.view.line : null;
  const curClass = s.view?.past ? "past" : "current";
  $("highlight").innerHTML = lines
    .map((l, i) => {
      const n = i + 1;
      const cls = n === s.errorLine ? " error" : n === cur ? ` ${curClass}` : "";
      return `<span class="ln${cls}">${highlightLine(l) || " "}</span>`;
    })
    .join("");
  $("gutter").innerHTML = lines.map((_, i) => (i + 1 === s.errorLine ? `<span class="err">${i + 1}</span>` : i + 1)).join("\n");
}

function scrollToLine(line) {
  const el = $("highlight").children[line - 1];
  const box = $("editor");
  if (!el) return;
  const top = el.offsetTop;
  if (top < box.scrollTop + 20 || top > box.scrollTop + box.clientHeight - 40) box.scrollTop = top - box.clientHeight / 3;
}

$("source").addEventListener("input", () => {
  if (s.loaded || s.killed) unload();
  s.errorLine = null;
  renderEditor();
});
$("source").addEventListener("keydown", (e) => {
  if (e.key === "Tab") {
    e.preventDefault();
    document.execCommand("insertText", false, "    ");
  }
  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
    e.preventDefault();
    runClicked();
  }
});

// ---------------------------------------------------------------- console and panels

function sys(text, cls = "") {
  return `<span class="sys ${cls}">${esc(text)}</span>`;
}

function renderConsole() {
  const out = s.view && (s.loaded || s.killed) ? esc(s.view.output) : "";
  const el = $("console");
  const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 30;
  el.innerHTML = s.frozen + out + (s.view?.past ? "" : s.tail);
  if (atBottom) el.scrollTop = el.scrollHeight;
}

function renderStack() {
  const v = s.view;
  if (!v || !s.loaded || !v.stack.length) {
    $("stack").innerHTML = `<p class="empty">${s.killed ? "Gone with the process. Restore it from the checkpoint." : "Run the program to see its variables."}</p>`;
    return;
  }
  const seen = new Map();
  $("stack").innerHTML = v.stack
    .map((f, fi) => {
      const vars = f.locals
        .map(([k, val]) => {
          const key = `${fi}:${f.function}:${k}`;
          seen.set(key, val);
          const prev = s.prevLocals.get(key);
          const changed = prev !== undefined && prev !== val ? " changed" : "";
          return `<div class="var${changed}"><span class="k">${esc(k)}</span><span class="v" title="${esc(val)}">${esc(val)}</span></div>`;
        })
        .join("");
      return `<div class="frame"><div class="frame-head">${esc(f.function)} <span>line ${f.line}</span></div>${vars || '<div class="var"><span class="k"></span><span class="v">no variables yet</span></div>'}</div>`;
    })
    .join("");
  s.prevLocals = seen;
}

function renderBytecode() {
  if (!s.program) {
    $("bytecode").innerHTML = "";
    return;
  }
  $("bytecode").innerHTML = s.program.functions
    .map(
      (f, fi) =>
        `<div class="bc-fn"><h3>${esc(f.name)}</h3>${f.ops
          .map(([line, text, effect], pc) => `<div class="bc-op${effect ? " effect" : ""}" data-at="${fi}:${pc}"><span class="pc">${String(pc).padStart(3, "0")}</span>  ${esc(text)}</div>`)
          .join("")}</div>`,
    )
    .join("");
}

let currentOp = null;
function markOp() {
  currentOp?.classList.remove("current");
  currentOp = null;
  if (!s.view || !s.loaded || !$("bytecode").closest("details").open) return;
  currentOp = $("bytecode").querySelector(`[data-at="${s.view.func}:${s.view.pc}"]`);
  currentOp?.classList.add("current");
  currentOp?.scrollIntoView({ block: "nearest", inline: "nearest" });
}
$("bytecode").closest("details").addEventListener("toggle", markOp);

// ---------------------------------------------------------------- the thread (timeline)

const SVG = "http://www.w3.org/2000/svg";
function el(name, attrs, parent) {
  const e = document.createElementNS(SVG, name);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  parent.appendChild(e);
  return e;
}

function maxStep() {
  return Math.max(1, s.view?.headStep ?? 0, s.cutAt ?? 0);
}

function renderThread() {
  const svg = $("thread");
  const w = svg.clientWidth || 800;
  const h = 46;
  const pad = 18;
  const mid = 24;
  svg.setAttribute("viewBox", `0 0 ${w} ${h}`);
  svg.innerHTML = "";
  const max = maxStep();
  const x = (step) => pad + (Math.min(step, max) / max) * (w - 2 * pad);
  el("line", { class: "base", x1: pad, y1: mid, x2: w - pad, y2: mid }, svg);
  if (!s.view && !s.killed) return;
  const head = s.killed ? s.cutAt : s.view.headStep;
  if (s.sessionStart > 0) el("line", { class: "ckpt", x1: x(s.sessionStart), y1: mid - 9, x2: x(s.sessionStart), y2: mid + 9 }, svg);
  if (s.killed) {
    const from = s.lostFrom ?? 0;
    el("line", { class: "spun", x1: x(0), y1: mid, x2: x(from), y2: mid }, svg);
    el("line", { class: "lost", x1: x(from), y1: mid, x2: x(head), y2: mid }, svg);
    el("line", { class: "cut-mark", x1: x(head) - 5, y1: mid - 9, x2: x(head) + 5, y2: mid + 9 }, svg);
    el("circle", { class: "knot-ring", cx: x(from), cy: mid, r: 6 }, svg);
  } else {
    el("line", { class: "spun", x1: x(0), y1: mid, x2: x(head), y2: mid }, svg);
  }
  for (const k of s.knots) if (k <= head) el("circle", { class: "knot", cx: x(k), cy: mid, r: 4 }, svg);
  if (!s.killed && s.view) {
    const g = el("g", { class: `shuttle-mark${s.view.past ? " past" : ""}`, transform: `translate(${x(s.view.step)}, ${mid})` }, svg);
    el("path", { d: "M-15 0C-9-7 9-7 15 0 9 7-9 7-15 0z" }, g);
    el("line", { x1: -15, y1: 0, x2: 15, y2: 0 }, g);
  }
  svg.setAttribute("aria-valuemax", head);
  svg.setAttribute("aria-valuenow", s.view?.step ?? head);
}

function stepAt(clientX) {
  const r = $("thread").getBoundingClientRect();
  const frac = Math.min(1, Math.max(0, (clientX - r.left - 18) / (r.width - 36)));
  return Math.round(frac * maxStep());
}

function seek(step) {
  if (!s.loaded) return;
  const head = s.view.headStep;
  step = Math.max(s.sessionStart, Math.min(head, step));
  if (s.playing) pause();
  wasm.seek(step);
  apply(resultJson(), false);
}

let dragging = false;
$("thread").addEventListener("pointerdown", (e) => {
  if (!s.loaded) return;
  dragging = true;
  $("thread").setPointerCapture(e.pointerId);
  seek(stepAt(e.clientX));
});
$("thread").addEventListener("pointermove", (e) => dragging && seek(stepAt(e.clientX)));
$("thread").addEventListener("pointerup", () => (dragging = false));
$("thread").addEventListener("keydown", (e) => {
  if (!s.loaded || !["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) return;
  e.preventDefault();
  const d = e.shiftKey ? 100 : 1;
  const cur = s.view.step;
  seek(e.key === "Home" ? 0 : e.key === "End" ? s.view.headStep : cur + (e.key === "ArrowRight" ? d : -d));
});
$("now").addEventListener("click", () => {
  wasm.view();
  apply(resultJson(), false);
});
new ResizeObserver(renderThread).observe($("thread"));

// ---------------------------------------------------------------- driving the VM

function render() {
  renderEditor();
  renderConsole();
  renderStack();
  renderThread();
  markOp();
  const v = s.view;
  const fmt = (n) => n.toLocaleString("en-US");
  let where = "Not started";
  if (s.killed) where = `Process killed at instruction ${fmt(s.cutAt)}`;
  else if (v && s.loaded) {
    where = v.past ? `Instruction ${fmt(v.step)} of ${fmt(v.headStep)}, in the past` : `Instruction ${fmt(v.step)}, line ${v.line}`;
    if (!v.past && v.status === "waiting") where += `, waiting for ${v.effect.name}`;
    if (!v.past && v.status === "done") where = `Finished after ${fmt(v.step)} instructions`;
  }
  $("where").textContent = where;
  $("now").hidden = !(v?.past && s.loaded);
  $("ckpt").textContent = s.ckpt ? `Last checkpoint: ${s.ckptSize.toLocaleString("en-US")} bytes, instruction ${fmt(s.ckptStep)}` : "";
  $("download").disabled = !s.ckpt;
  $("restore").disabled = !s.ckpt || (s.loaded && !s.killed && v?.status !== "done" && v?.status !== "failed");
  $("kill").disabled = !s.loaded || ["done", "failed"].includes(v?.status);
  $("run").textContent = s.playing ? "Pause" : s.loaded && ["done", "failed"].includes(v?.status) ? "Run again" : s.loaded && v?.step > 0 ? "Continue" : "Run";
}

function apply(view, follow = true) {
  s.view = view;
  if (!view.past) {
    for (const k of view.effectSteps) s.knots.add(k);
    syncCheckpoint();
  }
  if (view.error) {
    s.errorLine = view.error.line ?? null;
    s.tail = `<span class="err">${esc(view.error.text)}</span>`;
    s.playing = false;
  } else if (view.status === "done" && !view.past) {
    s.tail = view.effect && view.effect !== "nil" ? `<span class="ret">returned ${esc(view.effect)}</span>` : "";
  }
  render();
  if (follow || view.past) scrollToLine(view.line);
}

function syncCheckpoint() {
  const v = s.view;
  if (!v || v.checkpointStep === s.ckptStep && v.checkpointSize === s.ckptSize) return;
  wasm.checkpoint();
  const bytes = resultBytes();
  if (!bytes.length) return;
  s.ckpt = bytes;
  s.ckptStep = v.checkpointStep;
  s.ckptSize = bytes.length;
  try {
    let bin = "";
    for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
    localStorage.setItem(STORE_KEY, JSON.stringify({ b64: btoa(bin), savedAt: Date.now(), step: s.ckptStep }));
  } catch {
    // storage full or blocked: the checkpoint still lives in this tab
  }
}

function load() {
  s.errorLine = null;
  s.knots = new Set();
  s.cutAt = null;
  s.killed = false;
  s.frozen = "";
  s.tail = "";
  s.sessionStart = 0;
  s.prevLocals = new Map();
  if (put(wasm.load, $("source").value) !== 0) {
    const e = resultJson();
    s.errorLine = e.line ?? null;
    s.loaded = false;
    s.view = null;
    s.tail = `<span class="err">${esc(e.text)}</span>`;
    render();
    if (e.line) scrollToLine(e.line);
    return false;
  }
  s.program = resultJson();
  s.loaded = true;
  renderBytecode();
  wasm.view();
  apply(resultJson());
  return true;
}

function unload() {
  pause();
  cancelEffect();
  s.loaded = false;
  s.killed = false;
  s.view = null;
  s.program = null;
  s.frozen = "";
  s.tail = "";
  s.knots = new Set();
  s.cutAt = null;
  renderBytecode();
  render();
}

function pause() {
  s.playing = false;
  render();
}

function tick() {
  if (!s.playing) return;
  if (!s.busy) {
    const v = s.view;
    if (v.status === "waiting") {
      perform(v.effect);
    } else if (v.status === "running") {
      wasm.run(SPEEDS[$("speed").value][0]);
      apply(resultJson());
      if (s.view.status === "waiting") perform(s.view.effect);
      if (["done", "failed"].includes(s.view.status)) {
        s.playing = false;
        render();
        return;
      }
    } else {
      s.playing = false;
      render();
      return;
    }
  }
  requestAnimationFrame(tick);
}

function play() {
  if (s.playing) return;
  s.playing = true;
  render();
  requestAnimationFrame(tick);
}

function runClicked() {
  if (s.playing) return pause();
  if (s.killed) return;
  if (!s.loaded || ["done", "failed"].includes(s.view?.status)) {
    if (!load()) return;
  }
  play();
}

$("run").addEventListener("click", runClicked);
$("step").addEventListener("click", () => {
  if (s.killed) return;
  if (!s.loaded || ["done", "failed"].includes(s.view?.status)) {
    if (!load()) return;
  }
  pause();
  if (s.busy) return;
  if (s.view.status === "waiting") return perform(s.view.effect);
  wasm.run(1);
  apply(resultJson());
  if (s.view.status === "waiting") perform(s.view.effect);
});
$("speed").addEventListener("input", () => ($("speed-name").textContent = SPEEDS[$("speed").value][1]));
$("speed-name").textContent = SPEEDS[$("speed").value][1];

// ---------------------------------------------------------------- effects

function cancelEffect() {
  clearTimeout(s.timer);
  s.timer = null;
  s.busy = false;
  $("ask").hidden = true;
}

function finishEffect(value) {
  cancelEffect();
  put(wasm.resume, JSON.stringify(value));
  apply(resultJson());
  if (!s.playing && s.view.status === "running") play();
}

function failEffect(message) {
  cancelEffect();
  put(wasm.fail, message);
  apply(resultJson());
}

function perform(effect) {
  if (s.busy) return;
  s.busy = true;
  const [a] = effect.args;
  switch (effect.name) {
    case "sleep": {
      const ms = Math.max(0, Number(a) || 0) * 1000;
      s.timer = setTimeout(() => finishEffect(null), ms);
      return;
    }
    case "ask": {
      $("ask-prompt").textContent = typeof a === "string" ? a : a == null ? "The program asks:" : JSON.stringify(a);
      $("ask").hidden = false;
      $("ask-input").value = "";
      $("ask-input").focus({ preventScroll: true });
      return;
    }
    case "now":
      return finishEffect(Date.now());
    case "random":
      return finishEffect(Math.random());
    default:
      return failEffect(`\`${effect.name}\` is not available in the browser; run this program with the spola command`);
  }
}

$("ask").addEventListener("submit", (e) => {
  e.preventDefault();
  if (!s.busy) return;
  finishEffect($("ask-input").value);
});

// ---------------------------------------------------------------- crash and recovery

$("kill").addEventListener("click", () => {
  if (!s.loaded) return;
  cancelEffect();
  s.playing = false;
  s.cutAt = s.view.headStep;
  s.lostFrom = s.ckptStep;
  const output = esc(s.view.output);
  wasm.crash();
  s.loaded = false;
  s.killed = true;
  s.frozen += output + sys(`✂ Process killed. Its memory is gone. The checkpoint from instruction ${s.ckptStep.toLocaleString("en-US")} (${s.ckptSize} bytes) survived.`, "cut");
  s.tail = "";
  s.view = null;
  render();
});

function restoreFrom(bytes, how) {
  cancelEffect();
  if (put(wasm.restore, bytes) !== 0) {
    const e = resultJson();
    s.frozen += sys(`Cannot restore: ${e.message}`, "cut");
    render();
    return;
  }
  s.program = resultJson();
  $("source").value = s.program.source;
  s.loaded = true;
  s.killed = false;
  s.errorLine = null;
  s.cutAt = null;
  s.tail = "";
  s.prevLocals = new Map();
  renderBytecode();
  wasm.view();
  const v = resultJson();
  s.sessionStart = v.step;
  s.frozen += sys(`↻ ${how} at instruction ${v.step.toLocaleString("en-US")}, line ${v.line}. Continuing.`, "tie");
  apply(v);
  play();
}

$("restore").addEventListener("click", () => s.ckpt && restoreFrom(s.ckpt, "Restored from the checkpoint"));
$("download").addEventListener("click", () => {
  const a = document.createElement("a");
  a.href = URL.createObjectURL(new Blob([s.ckpt], { type: "application/octet-stream" }));
  a.download = `${s.exampleName}.state`;
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
});
$("open").addEventListener("change", async (e) => {
  const f = e.target.files[0];
  if (!f) return;
  unload();
  s.exampleName = f.name.replace(/\.state$/, "");
  restoreFrom(new Uint8Array(await f.arrayBuffer()), `Opened ${f.name}`);
  e.target.value = "";
});

// ---------------------------------------------------------------- examples and startup

async function openExample(file) {
  const src = await (await fetch(`examples/${file}`)).text();
  unload();
  s.exampleName = file.replace(/\.sp$/, "");
  $("source").value = src;
  $("editor").scrollTop = 0;
  renderEditor();
}

for (const ex of EXAMPLES) $("example").add(new Option(ex.title, ex.file));
$("example").addEventListener("change", () => openExample($("example").value));
await openExample(EXAMPLES[0].file);

try {
  const saved = JSON.parse(localStorage.getItem(STORE_KEY) || "null");
  if (saved?.b64) {
    s.ckpt = Uint8Array.from(atob(saved.b64), (c) => c.charCodeAt(0));
    s.ckptStep = saved.step;
    s.ckptSize = s.ckpt.length;
    const mins = Math.round((Date.now() - saved.savedAt) / 60000);
    const ago = mins < 1 ? "less than a minute ago" : mins < 120 ? `${mins} minutes ago` : `${Math.round(mins / 60)} hours ago`;
    s.frozen = sys(`A checkpoint from your last visit survived (saved ${ago}). Restore it to continue that run.`, "tie");
  }
} catch {
  // no storage: start clean
}
render();
