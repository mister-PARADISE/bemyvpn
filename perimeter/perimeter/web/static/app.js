/* Периметр — поведение интерфейса. Без зависимостей.
   Всё навешивается через data-атрибуты, поэтому новые страницы получают поведение сами:
   data-tip, data-confirm, data-copy, data-href, data-autosubmit, data-scope-check. */
(() => {
  "use strict";

  const $ = (sel, root = document) => root.querySelector(sel);
  const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
  const root = document.documentElement;
  const csrf = () => document.body.dataset.csrf || "";
  const store = {
    get(k) { try { return localStorage.getItem(k); } catch { return null; } },
    set(k, v) { try { localStorage.setItem(k, v); } catch { /* приватный режим */ } },
  };
  const session = {
    get(k) { try { return JSON.parse(sessionStorage.getItem(k)); } catch { return null; } },
    set(k, v) { try { sessionStorage.setItem(k, JSON.stringify(v)); } catch { /* нет хранилища */ } },
    del(k) { try { sessionStorage.removeItem(k); } catch { /* нет хранилища */ } },
  };
  const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  const svg = (name, cls = "") => `<svg class="i ${cls}" aria-hidden="true"><use href="#i-${esc(name)}"></use></svg>`;

  /* ---------- Уведомления: снизу по центру; одинаковый ключ заменяет прежнее ---------- */
  const toasts = new Map();
  function toast(text, tone = "success", key = null) {
    const box = $("#toasts");
    if (!box) return;
    if (key && toasts.has(key)) toasts.get(key)();
    const el = document.createElement("div");
    const bad = tone === "error" || tone === "danger";
    el.className = `toast tone-${bad ? "error" : tone}${bad ? " bad" : ""}`;
    el.setAttribute("role", bad ? "alert" : "status");
    const ico = { error: "x-circle", danger: "x-circle", warning: "alert", info: "info" }[tone] || "check-circle";
    el.innerHTML = `${svg(ico)}<div class="text">${esc(text)}</div>`;
    let gone = false;
    const close = () => {
      if (gone) return;
      gone = true;
      if (key) toasts.delete(key);
      el.classList.add("leave");
      setTimeout(() => el.remove(), 200);
    };
    if (key) toasts.set(key, close);
    el.addEventListener("click", close);
    box.appendChild(el);
    setTimeout(close, bad ? 8000 : key === "copy" ? 1600 : 4000);
  }
  window.toast = toast;

  /* ---------- Панель: свернуть (Ctrl+B), выдвижная на телефоне ---------- */
  function toggleCollapse() {
    const closed = root.classList.toggle("collapsed");
    store.set("perimeter.side", closed ? "closed" : "open");
    hideTip();
  }
  const toggleDrawer = (open) => root.classList.toggle("drawer", open);

  /* ---------- Подсказки: только у кнопок-иконок, с задержкой, спокойные ---------- */
  let tipEl = null, tipTimer = 0, tipFor = null;
  function showTip(target) {
    const when = target.dataset.tipWhen;
    if (when === "collapsed" && !root.classList.contains("collapsed")) return;
    if (!tipEl) {
      tipEl = document.createElement("div");
      tipEl.className = "tooltip";
      document.body.appendChild(tipEl);
    }
    const key = target.dataset.tipKey;
    tipEl.innerHTML = esc(target.dataset.tip) + (key ? `<kbd>${esc(key)}</kbd>` : "");
    const r = target.getBoundingClientRect();
    const side = when === "collapsed" ? "right" : target.dataset.tipSide || "bottom";
    tipEl.classList.remove("on");
    tipEl.style.left = "0px"; tipEl.style.top = "0px";
    const t = tipEl.getBoundingClientRect();
    let x, y;
    if (side === "right") { x = r.right + 8; y = r.top + (r.height - t.height) / 2; }
    else if (side === "top") { x = r.left + (r.width - t.width) / 2; y = r.top - t.height - 6; }
    else { x = r.left + (r.width - t.width) / 2; y = r.bottom + 6; }
    x = Math.max(8, Math.min(x, innerWidth - t.width - 8));
    y = Math.max(8, Math.min(y, innerHeight - t.height - 8));
    tipEl.style.left = `${x}px`; tipEl.style.top = `${y}px`;
    tipEl.classList.add("on");
  }
  function hideTip() { clearTimeout(tipTimer); tipFor = null; tipEl?.classList.remove("on"); }
  document.addEventListener("pointerover", (e) => {
    if (e.pointerType !== "mouse") return;
    const t = e.target.closest("[data-tip]");
    if (t === tipFor) return;
    hideTip();
    if (!t) return;
    tipFor = t;
    tipTimer = setTimeout(() => showTip(t), 450);
  });
  document.addEventListener("pointerdown", hideTip, true);
  addEventListener("scroll", hideTip, true);

  /* ---------- Подтверждение опасных действий ---------- */
  function confirmDialog({ title, text, ok }) {
    const dlg = $("#confirm");
    if (!dlg) return Promise.resolve(window.confirm(text));
    $('[data-slot="title"]', dlg).textContent = title || "Точно?";
    $('[data-slot="text"]', dlg).textContent = text || "";
    $('[data-slot="ok"]', dlg).textContent = ok || "Подтвердить";
    return new Promise((resolve) => {
      dlg.returnValue = "";
      dlg.addEventListener("close", () => resolve(dlg.returnValue === "ok"), { once: true });
      dlg.showModal();
      $('[data-slot="ok"]', dlg).focus();
    });
  }

  /* ---------- После действия страница остаётся там же, где была ---------- */
  document.addEventListener("submit", async (e) => {
    const form = e.target;
    if (!(form instanceof HTMLFormElement)) return;
    if (form.matches("[data-scope-check]")) { e.preventDefault(); scopeCheck(form); return; }
    if (form.dataset.confirm && !form.dataset.confirmed) {
      e.preventDefault();
      const ok = await confirmDialog({ title: form.dataset.confirmTitle, text: form.dataset.confirm, ok: form.dataset.confirmOk });
      if (ok) { form.dataset.confirmed = "1"; form.requestSubmit(); delete form.dataset.confirmed; }
      return;
    }
    // getAttribute, а не form.method: поле с name="method" подменило бы свойство
    if ((form.getAttribute("method") || "").toLowerCase() === "post") {
      session.set("perimeter.scroll", { path: location.pathname, y: scrollY, t: Date.now() });
    }
  });
  function restoreScroll() {
    const s = session.get("perimeter.scroll");
    session.del("perimeter.scroll");
    if (s && s.path === location.pathname && Date.now() - s.t < 15000) {
      if ("scrollRestoration" in history) history.scrollRestoration = "manual";
      scrollTo(0, s.y);
    }
  }

  /* ---------- Копирование по клику ---------- */
  async function copyText(text, el) {
    try { await navigator.clipboard.writeText(text); }
    catch {
      const ta = Object.assign(document.createElement("textarea"), { value: text });
      ta.style.cssText = "position:fixed;opacity:0";
      document.body.appendChild(ta); ta.select(); document.execCommand("copy"); ta.remove();
    }
    if (el) { el.classList.add("done"); setTimeout(() => el.classList.remove("done"), 600); }
    toast("Скопировано", "success", "copy");
  }

  /* ---------- Поиск и переходы ---------- */
  const palette = {
    dlg: null, input: null, list: null, items: [], active: 0, timer: 0, seq: 0,
    init() {
      this.dlg = $("#palette");
      if (!this.dlg) return;
      this.input = $("input", this.dlg);
      this.list = $(".palette-list", this.dlg);
      this.input.addEventListener("input", () => { clearTimeout(this.timer); this.timer = setTimeout(() => this.load(), 120); });
      this.input.addEventListener("keydown", (e) => this.key(e));
      this.dlg.addEventListener("click", (e) => { if (e.target === this.dlg) this.dlg.close(); });
      this.list.addEventListener("mousemove", (e) => {
        const it = e.target.closest(".palette-item");
        if (it && +it.dataset.index !== this.active) this.select(+it.dataset.index);
      });
    },
    open() {
      if (!this.dlg || this.dlg.open) return;
      toggleDrawer(false);
      this.input.value = "";
      this.dlg.showModal();
      this.input.focus();
      this.load();
    },
    async load() {
      const seq = ++this.seq;
      try {
        const r = await fetch(`/api/search?q=${encodeURIComponent(this.input.value.trim())}`, { headers: { Accept: "application/json" } });
        const data = await r.json();
        if (seq === this.seq) this.render(data.items || []);
      } catch { if (seq === this.seq) this.render([]); }
    },
    render(items) {
      this.items = items;
      this.active = 0;
      if (!items.length) { this.list.innerHTML = `<div class="palette-empty">Ничего не нашлось</div>`; return; }
      let html = "", group = null;
      items.forEach((it, i) => {
        if (it.kind !== group) { group = it.kind; html += `<div class="palette-group">${esc(group)}</div>`; }
        html += `<a class="palette-item" href="${esc(it.url)}" data-index="${i}" role="option">${svg(it.icon || "circle")}
          <span class="cell grow"><span class="l1"><span>${esc(it.title)}</span></span>${it.subtitle ? `<span class="l2">${esc(it.subtitle)}</span>` : ""}</span></a>`;
      });
      this.list.innerHTML = html;
      this.select(0);
    },
    select(i) {
      const els = $$(".palette-item", this.list);
      if (!els.length) return;
      this.active = (i + els.length) % els.length;
      els.forEach((el, n) => el.classList.toggle("on", n === this.active));
      els[this.active].scrollIntoView({ block: "nearest" });
    },
    key(e) {
      if (e.key === "ArrowDown") { e.preventDefault(); this.select(this.active + 1); }
      else if (e.key === "ArrowUp") { e.preventDefault(); this.select(this.active - 1); }
      else if (e.key === "Enter") { e.preventDefault(); const it = this.items[this.active]; if (it) location.href = it.url; }
    },
  };

  /* ---------- Проверка адреса на границы цели ---------- */
  async function scopeCheck(form) {
    const out = $("[data-scope-result]", form);
    const host = new FormData(form).get("host");
    if (!host) return;
    try {
      const r = await fetch(`${form.getAttribute("action")}?host=${encodeURIComponent(host)}`, { headers: { Accept: "application/json" } });
      const d = await r.json();
      out.hidden = false;
      out.className = `note tone-${d.allowed ? "success" : "danger"}`;
      out.innerHTML = `${svg(d.allowed ? "check-circle" : "x-circle")}<div class="cell"><span class="l1"><span>${
        d.allowed ? "Входит в границы" : "Не входит"}</span></span><span class="l2">${esc(d.host || host)}</span><span class="small">${esc(d.reason)}</span></div>`;
    } catch { toast("Проверка не удалась", "error"); }
  }

  /* ---------- Вход как в консоль: эхо ввода, блочный курсор, шаги по Enter ---------- */
  function initTerm() {
    const form = $("form[data-term]");
    if (!form) return;
    root.classList.add("js-term");
    const inputs = $$(".tprompt input", form);
    const line = (input) => input.closest(".tprompt");
    const echo = (input) => {
      const out = $(".echo", line(input));
      if (out) out.textContent = input.type === "password" ? "" : input.value;
    };
    inputs.forEach((input) => {
      echo(input);
      input.addEventListener("input", () => echo(input));
      input.addEventListener("change", () => echo(input));
    });
    setTimeout(() => inputs.forEach(echo), 700); // браузер мог подставить сохранённый логин
    $$(".tprompt[data-later]", form).forEach((p) => { p.hidden = true; });
    const shown = () => inputs.filter((input) => !line(input).hidden);
    (inputs.find((input) => input.hasAttribute("data-focus")) || shown()[0])?.focus();

    form.addEventListener("keydown", (e) => {
      if (e.key !== "Enter") return;
      const i = inputs.indexOf(document.activeElement);
      if (i < 0) return;
      e.preventDefault();
      if (!inputs[i].value) return; // пустая строка — ждём ввода, как терминал
      const next = inputs[i + 1];
      if (next && line(next).hidden) {
        line(next).style.setProperty("--i", "0");
        line(next).hidden = false;
      }
      if (next && !next.value) { next.focus(); return; }
      form.classList.add("sending");
      form.requestSubmit();
    });
    // клик в любом месте экрана возвращает курсор в текущую строку
    document.addEventListener("click", (e) => {
      if (e.target.closest("a, button, input, label")) return;
      const visible = shown();
      (visible.find((input) => !input.value) || visible[visible.length - 1])?.focus();
    });
  }

  /* ---------- Сервер жив? ---------- */
  async function watchServer() {
    const el = $("[data-server]");
    if (!el) return;
    let ok = true;
    try { ok = (await fetch("/health", { cache: "no-store" })).ok; } catch { ok = false; }
    el.classList.toggle("down", !ok);
    $(".txt", el).textContent = ok ? "online" : "offline";
    setTimeout(watchServer, ok ? 15000 : 3000);
  }

  /* ---------- Монитор на обзоре: раз в 2 с, пока вкладка открыта ---------- */
  function watchMonitor() {
    const mon = $("[data-monitor]");
    if (!mon) return;
    // длина полосы — сколько знаков влезает в строку, как в htop (запас под ключ и подпись)
    const charWidth = () => {
      const probe = document.createElement("span");
      probe.textContent = "0".repeat(10);
      probe.style.cssText = "position:absolute;visibility:hidden;white-space:pre";
      mon.appendChild(probe);
      const w = probe.getBoundingClientRect().width / 10;
      probe.remove();
      return w || 8;
    };
    // запас: ключ 6 + скобки 2 + промежутки 2 + самая длинная подпись (~22 знака) + 1 на округление
    const width = () => {
      const row = $(".mon-row", mon);
      return Math.max(8, Math.min(40, Math.floor((row ? row.clientWidth : mon.clientWidth) / charWidth()) - 33));
    };
    const draw = (row, pct) => {
      const bar = $("[data-bar]", row);
      if (!bar) return;
      const n = width();
      const filled = Math.max(0, Math.min(n, Math.round((pct / 100) * n)));
      bar.textContent = "|".repeat(filled) + " ".repeat(n - filled);
    };
    const redraw = () => $$(".mon-row[data-pct]", mon).forEach((row) => draw(row, +row.dataset.pct));
    redraw();
    addEventListener("resize", redraw);
    const tick = async () => {
      if (!document.hidden) {
        try {
          const r = await fetch("/api/system", { headers: { Accept: "application/json" }, cache: "no-store" });
          if (r.ok) {
            const data = await r.json();
            data.rows.forEach((row) => {
              const el = $(`.mon-row[data-key="${CSS.escape(row.key)}"]`, mon);
              if (!el) return;
              const bar = $("[data-bar]", el);
              if (bar && row.bar !== null) {
                el.dataset.pct = row.pct;
                draw(el, row.pct);
                bar.parentElement.className = `mon-bar t-${row.tone}`;
              }
              $("[data-text]", el).textContent = row.text;
            });
          }
        } catch { /* сервер недоступен — об этом скажет строка состояния */ }
      }
      setTimeout(tick, 2000);
    };
    setTimeout(tick, 2000);
  }

  /* ---------- Консоль запуска: живой вывод, пока модуль работает ---------- */
  function initConsole() {
    const box = $("[data-console]");
    if (!box || box.dataset.active !== "1") return;
    const body = $("[data-c-body]", box);
    const prog = $("[data-c-progress]", box);
    let after = +box.dataset.after || 0;
    body.scrollTop = body.scrollHeight;
    const bar = (p) => {
      const w = 30, fill = Math.round((p.done / (p.total || 1)) * w);
      return `[${"#".repeat(fill)}${"-".repeat(w - fill)}] ${p.done}/${p.total} ${p.label || ""}`;
    };
    const tick = async () => {
      try {
        const r = await fetch(`/runs/${box.dataset.console}/log?after=${after}`, { headers: { Accept: "application/json" }, cache: "no-store" });
        if (r.ok) {
          const s = await r.json();
          const stick = body.scrollHeight - body.scrollTop - body.clientHeight < 40;
          s.lines.forEach((ln) => {
            const div = document.createElement("div");
            div.className = `cl lvl-${ln.level}`;
            div.innerHTML = `<span class="ts">${esc(ln.at)}</span>`;
            div.append(ln.text);
            body.append(div);
            after = ln.n;
          });
          if (stick) body.scrollTop = body.scrollHeight;
          const badge = $("[data-c-status]", box);
          badge.className = `badge tone-${s.tone} dot`;
          badge.textContent = s.title;
          $("[data-c-time]", box).textContent = `${s.seconds} с`;
          prog.hidden = !(s.active && s.progress);
          if (s.active && s.progress) prog.textContent = bar(s.progress);
          if (!s.active) {
            $("[data-c-stop]", box).hidden = true;
            $("[data-c-again]", box).hidden = false;
            if (box.hasAttribute("data-reload")) setTimeout(() => location.reload(), 700); // показать новые находки
            return;
          }
        }
      } catch { /* сервер недоступен — попробуем ещё */ }
      setTimeout(tick, 700);
    };
    setTimeout(tick, 400);
  }

  /* ---------- Вкладки без перезагрузки: [data-tabs] > [data-tab] / [data-panel] ---------- */
  document.addEventListener("click", (e) => {
    const tab = e.target.closest("[data-tabs] [data-tab]");
    if (!tab) return;
    const group = tab.closest("[data-tabs]");
    $$("[data-tab]", group).forEach((b) => b.classList.toggle("on", b === tab));
    $$("[data-panel]", group).forEach((p) => { p.hidden = p.dataset.panel !== tab.dataset.tab; });
  });

  /* ---------- Часы в строке состояния ---------- */
  function tickClock() {
    const el = $("[data-clock]");
    if (!el) return;
    const now = new Date();
    try {
      el.textContent = new Intl.DateTimeFormat("ru-RU", { hour: "2-digit", minute: "2-digit", timeZone: el.dataset.tz }).format(now);
    } catch { /* неизвестный пояс — оставляем время сервера */ }
    setTimeout(tickClock, (60 - now.getSeconds()) * 1000 + 50);
  }

  /* ---------- Делегирование ---------- */
  document.addEventListener("click", (e) => {
    const act = e.target.closest("[data-action]");
    if (act) {
      const name = act.dataset.action;
      if (name === "collapse") toggleCollapse();
      else if (name === "drawer") toggleDrawer();
      else if (name === "palette") palette.open();
      return;
    }
    const cp = e.target.closest("[data-copy]");
    if (cp) { e.preventDefault(); copyText(cp.dataset.copy, cp); return; }
    const row = e.target.closest("[data-href]");
    if (row && !e.target.closest("a, button, input, select, textarea, label, form")) {
      if (e.metaKey || e.ctrlKey) window.open(row.dataset.href, "_blank");
      else location.href = row.dataset.href;
    }
  });

  /* ---------- Поле видно, только когда включён переключатель (Field.show_if) ---------- */
  function syncShowIf(scope = document) {
    $$("[data-show-if]", scope).forEach((el) => {
      const name = CSS.escape(el.dataset.showIf);
      const toggle = el.closest("form")?.querySelector(`input[type=checkbox][name="${name}"]`);
      if (toggle) el.hidden = !toggle.checked;
    });
  }

  document.addEventListener("change", (e) => {
    if (e.target.matches("[data-autosubmit]")) e.target.form?.requestSubmit();
    if (e.target.matches("input[type=checkbox]") && e.target.form) syncShowIf(e.target.form);
  });

  document.addEventListener("keydown", (e) => {
    const el = document.activeElement;
    const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(el?.tagName || "") || el?.isContentEditable;
    const mod = e.ctrlKey || e.metaKey;
    if (mod && e.key.toLowerCase() === "k") { e.preventDefault(); palette.open(); }
    else if (mod && e.key.toLowerCase() === "b") { e.preventDefault(); toggleCollapse(); }
    else if (e.key === "/" && !typing && !mod) { e.preventDefault(); palette.open(); }
    else if (e.key === "Escape") toggleDrawer(false);
  });

  document.addEventListener("DOMContentLoaded", () => {
    restoreScroll();
    syncShowIf();
    initTerm();
    watchMonitor();
    initConsole();
    const refresh = $("[data-refresh]");  // страница с идущей проверкой обновляет себя сама
    if (refresh) setTimeout(() => location.reload(), +refresh.dataset.refresh || 2000);
    palette.init();
    setTimeout(tickClock, (60 - new Date().getSeconds()) * 1000 + 50);
    watchServer();
    const box = $("#toasts");
    if (box?.dataset.flashes) {
      try { JSON.parse(box.dataset.flashes).forEach((f) => toast(f.text, f.tone)); } catch { /* пусто */ }
    }
    $(".is-invalid :is(input, textarea, select)")?.focus();
  });
})();
