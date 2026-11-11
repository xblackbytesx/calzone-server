// CalZone calendar page (phase 2 step 3): the EventCalendar grid (decision
// 26) around our own toolbar, the event panel, quick add, undo, keyboard and
// swipe. No inline handlers or styles (the CSP forbids them); every string
// comes from the page (decision 35). Writes go to /api/ (decision 37): a
// drag shows at once, and snaps back with a notice if the server refuses.
(function () {
  "use strict";

  // The page can arrive by a full load or by a boosted swap; start whenever a
  // calendar is on the page. Document-wide listeners are added once and act
  // on the live calendar only.
  var app = null;
  function live() { return app && document.body.contains(app.root) ? app : null; }
  function init() {
    var root = document.getElementById("cal-app");
    if (!root || !window.EventCalendar || root.getAttribute("data-ready")) return;
    root.setAttribute("data-ready", "1");
    app = build(root);
  }
  document.addEventListener("click", function (e) { var a = live(); if (a) a.onClick(e); });
  document.addEventListener("keydown", function (e) { var a = live(); if (a) a.onKey(e); });
  document.addEventListener("DOMContentLoaded", init);
  document.addEventListener("htmx:afterSettle", init);
  if (document.readyState !== "loading") init();

  function build(root) {
  var T = JSON.parse(root.getAttribute("data-strings"));
  var C = JSON.parse(root.getAttribute("data-config"));
  var grid = document.getElementById("cal-grid");
  var panel = document.getElementById("cal-panel");
  var scopeDialog = document.getElementById("cal-scope");
  var toastEl = document.getElementById("cal-toast");
  var titleEl = document.getElementById("cal-title");
  var VIEWS = ["listMonth", "timeGridDay", "timeGridWeek", "dayGridMonth"];

  // ---- Helpers -------------------------------------------------------------

  function pad(n) { return (n < 10 ? "0" : "") + n; }
  function ymd(d) { return d.getFullYear() + "-" + pad(d.getMonth() + 1) + "-" + pad(d.getDate()); }
  function local(d) { return ymd(d) + "T" + pad(d.getHours()) + ":" + pad(d.getMinutes()) + ":00"; }
  function addDays(dateStr, n) {
    var p = dateStr.split("-");
    var d = new Date(+p[0], +p[1] - 1, +p[2] + n);
    return ymd(d);
  }
  function isPhone() { return window.matchMedia("(max-width: 40rem)").matches; }

  /** fetch JSON; resolves {ok, status, data}, never rejects. */
  function api(method, url, body) {
    var opts = { method: method, credentials: "same-origin", headers: {} };
    if (body !== undefined) {
      opts.headers["content-type"] = "application/json";
      opts.body = JSON.stringify(body);
    }
    return fetch(url, opts).then(function (r) {
      return r.json().catch(function () { return null; }).then(function (data) {
        return { ok: r.ok, status: r.status, data: data };
      });
    }, function () { return { ok: false, status: 0, data: null }; });
  }

  /** Black or white text on an event color: whichever contrasts more
      (WCAG ratios), so mid tones get dark text. */
  function textOn(hex) {
    var m = /^#?([0-9a-f]{6})$/i.exec(hex || "");
    if (!m) return "#ffffff";
    var n = parseInt(m[1], 16);
    var lin = function (c) { c /= 255; return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4); };
    var L = 0.2126 * lin(n >> 16) + 0.7152 * lin((n >> 8) & 255) + 0.0722 * lin(n & 255);
    // Pure black and white: with them every color reaches 4.5:1 (the worst
    // case is about 4.58), with near-black some cannot.
    var onWhite = 1.05 / (L + 0.05), onBlack = (L + 0.05) / 0.05;
    return onBlack >= onWhite ? "#000000" : "#ffffff";
  }

  function initials(name) {
    var parts = String(name || "?").replace(/[^\p{L}\p{N} ]/gu, " ").trim().split(/\s+/);
    // First and last word, as the server does ("Bram de Vries" is BV).
    var s = parts.length > 1 ? parts[0][0] + parts[parts.length - 1][0] : (parts[0] || "?").slice(0, 2);
    return s.toUpperCase();
  }

  // ---- Toast with undo -------------------------------------------------------

  var toastTimer = null;
  var undoId = null;
  function toast(msg, changeId) {
    var btn = toastEl.querySelector("[data-cal=undo]");
    undoId = changeId || null;
    btn.hidden = !undoId;
    // Shown first, filled a moment later: screen readers announce a change
    // inside a visible live region, not a region that just appeared.
    var text = toastEl.querySelector(".cal-toast-text");
    text.textContent = "";
    toastEl.hidden = false;
    setTimeout(function () { text.textContent = msg; }, 50);
    clearTimeout(toastTimer);
    toastTimer = setTimeout(function () { toastEl.hidden = true; undoId = null; }, undoId ? 10000 : 5000);
  }

  function failed(r) {
    if (r.status === 412) { toast(T.msg_stale); refresh(); return; }
    if (r.status === 401) { toast(T.msg_signed_out); setTimeout(function () { location.href = "/login"; }, 1500); return; }
    if (r.status === 0) { toast(T.msg_offline); return; }
    if (r.status === 409) { toast(T.msg_cannot_undo); refresh(); return; }
    toast(T.msg_failed + " " + ((r.data && r.data.error) || r.status));
  }

  function undo() {
    if (!undoId) return;
    var id = undoId;
    undoId = null;
    api("POST", "/api/undo/" + encodeURIComponent(id), {}).then(function (r) {
      if (r.ok) { toast(T.msg_undone); refresh(); } else { failed(r); }
    });
  }

  // ---- Recurring scope ------------------------------------------------------

  /** Ask which occurrences: resolves "this", "following", "all" or null. */
  function askScope(question) {
    return new Promise(function (resolve) {
      scopeDialog.querySelector(".cal-scope-q").textContent = question;
      scopeDialog.returnValue = "";
      var done = function () {
        scopeDialog.removeEventListener("close", done);
        resolve(scopeDialog.returnValue || null);
      };
      scopeDialog.addEventListener("close", done);
      scopeDialog.showModal();
    });
  }

  // ---- The grid -------------------------------------------------------------

  function toEc(e) {
    return {
      id: e.id,
      start: e.start,
      end: e.end,
      allDay: e.all_day,
      title: e.title,
      backgroundColor: e.color,
      textColor: textOn(e.color),
      editable: e.editable,
      extendedProps: e
    };
  }

  function eventContent(info) {
    var p = info.event.extendedProps;
    var nodes = [];
    // In the time grid the block's place already shows the time, so the
    // title comes first, and a phone's narrow week columns skip the time.
    var grid = info.view.type.indexOf("timeGrid") === 0;
    var title = document.createElement("span");
    title.className = "cal-ev-title";
    title.textContent = p.title || "";
    if (grid) nodes.push(title);
    if (info.timeText && !info.event.allDay && !(grid && isPhone() && info.view.type === "timeGridWeek")) {
      var time = document.createElement("span");
      time.className = "cal-ev-time";
      time.textContent = info.timeText;
      nodes.push(time);
    }
    if (!grid) nodes.push(title);
    if (!p.own) {
      var who = document.createElement("span");
      who.className = "cal-ev-who";
      who.title = T.from_owner + " " + p.owner;
      who.textContent = initials(p.owner);
      nodes.push(who);
    }
    return { domNodes: nodes };
  }

  /** What a screen reader says for an event: title, when, calendar, and
      whose (the grid's visual cues spelled out). */
  var dayWords = new Intl.DateTimeFormat(C.locale, { weekday: "long", day: "numeric", month: "long" });
  function eventLabel(info) {
    var p = info.event.extendedProps;
    var parts = [p.title || ""];
    var when = dayWords.format(info.event.start);
    parts.push(info.event.allDay ? when + ", " + T.all_day : when + ", " + (info.timeText || ""));
    if (p.location) parts.push(p.location);
    parts.push(p.calendar);
    if (!p.own) parts.push(T.from_owner + " " + p.owner);
    return parts.filter(Boolean).join(", ");
  }

  /** EventCalendar marks its layout up as a table but puts navigation and
      event buttons straight into rows, which screen readers stumble over.
      The grid is a visual layout; the agenda view is the list. So the table
      roles go, and every event button carries a full label instead. */
  var LAYOUT_ROLES = { table: 1, row: 1, rowgroup: 1, columnheader: 1, cell: 1, gridcell: 1, grid: 1, list: 1, listitem: 1 };
  var LAYOUT_ATTRS = ["aria-colindex", "aria-rowindex", "aria-colcount", "aria-rowcount"];
  function strip(e) {
    if (e.hasAttribute("role") && LAYOUT_ROLES[e.getAttribute("role")]) e.removeAttribute("role");
    for (var j = 0; j < LAYOUT_ATTRS.length; j++) if (e.hasAttribute(LAYOUT_ATTRS[j])) e.removeAttribute(LAYOUT_ATTRS[j]);
  }
  function stripLayoutRoles(el) {
    strip(el);
    var all = el.querySelectorAll("[role], [aria-colindex], [aria-rowindex], [aria-colcount], [aria-rowcount]");
    for (var i = 0; i < all.length; i++) strip(all[i]);
  }

  var params = new URLSearchParams(location.search);
  var startView = VIEWS.indexOf(params.get("view")) >= 0 ? params.get("view") : (isPhone() ? "listMonth" : "timeGridWeek");
  var startDate = /^\d{4}-\d{2}-\d{2}$/.test(params.get("date") || "") ? params.get("date") : ymd(new Date());
  var timeFormat = { hour: "numeric", minute: "2-digit", hour12: C.hour12 };

  var cal = EventCalendar.create(grid, {
    view: startView,
    date: startDate,
    locale: C.locale,
    firstDay: C.first_day,
    headerToolbar: { start: "", center: "", end: "" },
    height: "100%",
    nowIndicator: true,
    editable: true,
    selectable: true,
    dayMaxEvents: true,
    scrollTime: "07:00:00",
    eventTimeFormat: timeFormat,
    // Narrow columns on a phone: "M 2" instead of a cut-off "Mon 02/11";
    // the month view's columns are weekdays only.
    dayHeaderFormat: isPhone() ? { weekday: "narrow", day: "numeric" } : { weekday: "short", day: "numeric", month: "numeric" },
    views: { dayGridMonth: { dayHeaderFormat: { weekday: isPhone() ? "narrow" : "short" } } },
    slotLabelFormat: timeFormat,
    allDayContent: T.all_day,
    noEventsContent: T.no_events,
    eventLongPressDelay: 400,
    selectLongPressDelay: 400,
    eventSources: [{
      events: function (info, ok, fail) {
        api("GET", "/api/occurrences?from=" + ymd(info.start) + "&to=" + ymd(info.end)).then(function (r) {
          if (r.ok) { ok(r.data.map(toEc)); } else { failed(r); fail(r); }
        });
      }
    }],
    eventContent: eventContent,
    eventDidMount: function (info) {
      info.el.setAttribute("aria-label", eventLabel(info));
      // Short events get one line with an ellipsis instead of a clipped wrap.
      var ev = info.event;
      if (!ev.allDay && ev.end) {
        var minutes = (ev.end - ev.start) / 60000;
        if (minutes < 40) info.el.classList.add("cal-ev-short");
        if (minutes <= 20) info.el.classList.add("cal-ev-tiny");
      }
    },
    eventClick: function (info) { openEvent(info.event.extendedProps); },
    dateClick: function (info) {
      if (!C.calendars.length) return;
      if (info.allDay) { openNew({ start: ymd(info.date), end: addDays(ymd(info.date), 1), allDay: true }); }
      else { openNew({ start: local(info.date) }); }
    },
    select: function (info) {
      if (!C.calendars.length) return;
      if (info.allDay) { openNew({ start: ymd(info.start), end: ymd(info.end), allDay: true }); }
      else { openNew({ start: local(info.start), end: local(info.end) }); }
      cal.unselect();
    },
    eventDrop: function (info) { move(info); },
    eventResize: function (info) { move(info); },
    datesSet: function () { afterNavigate(); }
  });

  function refresh() { cal.refetchEvents(); }

  var ecRoot = grid.querySelector(".ec") || grid;
  stripLayoutRoles(ecRoot);
  new MutationObserver(function () { stripLayoutRoles(ecRoot); })
    .observe(grid, { subtree: true, childList: true, attributes: true, attributeFilter: ["role"].concat(LAYOUT_ATTRS) });

  function afterNavigate() {
    var v = cal.getView();
    titleEl.textContent = v.title;
    var date = ymd(cal.getOption("date"));
    var url = new URL(location.href);
    url.searchParams.set("view", v.type);
    url.searchParams.set("date", date);
    history.replaceState(null, "", url);
    root.querySelectorAll("[data-view]").forEach(function (b) {
      b.setAttribute("aria-pressed", b.getAttribute("data-view") === v.type ? "true" : "false");
    });
  }
  afterNavigate();

  /** A drag or resize: ask the scope for a recurring event, send, and snap
      back if refused. */
  function move(info) {
    var p = info.event.extendedProps;
    var go = p.recurring ? askScope(T.scope_edit) : Promise.resolve("all");
    go.then(function (scope) {
      if (!scope) { info.revert(); return; }
      var allDay = info.event.allDay;
      var start = info.event.start;
      var end = info.event.end || start;
      var body = {
        etag: p.etag,
        instance: p.instance || undefined,
        scope: scope,
        start: allDay ? ymd(start) : local(start),
        end: allDay ? ymd(end) : local(end)
      };
      api("POST", "/api/events/" + p.collection + "/" + encodeURIComponent(p.name), body).then(function (r) {
        if (r.ok) { toast(T.msg_moved, r.data.change_id); refresh(); }
        else { info.revert(); failed(r); }
      });
    });
  }

  // ---- The panel ------------------------------------------------------------

  var lastFocus = null;
  function showPanel(html) {
    lastFocus = document.activeElement;
    panel.innerHTML = html;
    panel.hidden = false;
    document.body.classList.add("cal-panel-open");
    var first = panel.querySelector("input[name=title]");
    if (first && !first.disabled && !first.value) first.focus();
    else (panel.querySelector("[data-cal=close]") || panel).focus();
  }
  function closePanel() {
    panel.hidden = true;
    panel.innerHTML = "";
    document.body.classList.remove("cal-panel-open");
    if (lastFocus && lastFocus.focus) lastFocus.focus();
  }
  function loadPanel(url) {
    fetch(url, { credentials: "same-origin" }).then(function (r) {
      // A redirect means the session ended (it went to /login).
      if (r.redirected) return { ok: false, status: 401 };
      return r.text().then(function (t) { return { ok: r.ok, status: r.status, text: t }; });
    }, function () { return { ok: false, status: 0 }; }).then(function (r) {
      if (r.ok) showPanel(r.text); else failed({ status: r.status, data: null });
    });
  }
  function openEvent(p) {
    var q = "c=" + encodeURIComponent(p.collection) + "&n=" + encodeURIComponent(p.name);
    if (p.instance) q += "&i=" + encodeURIComponent(p.instance);
    loadPanel("/calendar/event?" + q);
  }
  function openNew(opts) {
    var q = new URLSearchParams();
    if (opts.start) q.set("start", opts.start);
    if (opts.end) q.set("end", opts.end);
    if (opts.allDay) q.set("all_day", "1");
    if (opts.title) q.set("title", opts.title);
    loadPanel("/calendar/event/new?" + q.toString());
  }

  function formValues(form) {
    var f = form.elements;
    return {
      title: f.title.value.trim(),
      all_day: f.all_day.checked,
      start_date: f.start_date.value,
      start_time: f.start_time.value,
      end_date: f.end_date.value,
      end_time: f.end_time.value,
      repeat: f.repeat.value,
      repeat_end: f.repeat_end.value,
      repeat_until: f.repeat_until.value,
      repeat_count: f.repeat_count.value,
      location: f.location.value,
      notes: f.notes.value,
      alarm: f.alarm.value
    };
  }

  /** The API body: only what changed (new events: everything). */
  function changeBody(v, initial, isNew) {
    var b = {};
    var changed = function (k) { return isNew || v[k] !== initial[k]; };
    if (changed("title")) b.summary = v.title;
    if (changed("location")) b.location = v.location;
    if (changed("notes")) b.description = v.notes;
    var timeChanged = ["all_day", "start_date", "start_time", "end_date", "end_time"].some(changed);
    if (timeChanged) {
      if (v.all_day) {
        b.start = v.start_date;
        // Shown as the last day; stored as the day after.
        b.end = addDays(v.end_date < v.start_date ? v.start_date : v.end_date, 1);
      } else {
        b.start = v.start_date + "T" + (v.start_time || "09:00");
        b.end = v.end_date + "T" + (v.end_time || v.start_time || "10:00");
      }
    }
    var repeatChanged = ["repeat", "repeat_end", "repeat_until", "repeat_count"].some(changed);
    if (repeatChanged && v.repeat !== "custom") b.repeat = repeatOf(v);
    if (changed("alarm") && v.alarm !== "keep") b.alarms = v.alarm === "none" ? [] : [parseInt(v.alarm, 10)];
    return b;
  }

  /** One of the panel's repeats with its end, or null for none. An end
      left empty means it never stops. */
  function repeatOf(v) {
    if (v.repeat === "none") return null;
    var r = { freq: v.repeat };
    var n = parseInt(v.repeat_count, 10);
    if (v.repeat_end === "until" && v.repeat_until) r.until = v.repeat_until;
    if (v.repeat_end === "count" && n > 0) r.count = Math.min(n, 1000);
    return r;
  }

  function save(form) {
    var v = formValues(form);
    if (!v.title) { toast(T.msg_need_title); return; }
    var initial = JSON.parse(form.getAttribute("data-initial"));
    var isNew = form.getAttribute("data-new") === "1";
    var body = changeBody(v, initial, isNew);
    if (isNew) {
      body.collection = form.elements.collection.value;
      api("POST", "/api/events", body).then(function (r) {
        if (r.ok) { closePanel(); toast(T.msg_added, r.data.change_id); refresh(); } else { failed(r); }
      });
      return;
    }
    if (Object.keys(body).length === 0) { closePanel(); return; }
    var recurring = form.getAttribute("data-recurring") === "1";
    // Reminders are yours and cover every occurrence: no scope question.
    var remindersOnly = Object.keys(body).length === 1 && body.alarms !== undefined;
    var go = recurring && !remindersOnly ? askScope(T.scope_edit) : Promise.resolve("all");
    go.then(function (scope) {
      if (!scope) return;
      body.etag = form.getAttribute("data-etag");
      body.scope = scope;
      var inst = form.getAttribute("data-instance");
      if (inst) body.instance = inst;
      var url = "/api/events/" + form.getAttribute("data-collection") + "/" + encodeURIComponent(form.getAttribute("data-name"));
      api("POST", url, body).then(function (r) {
        if (r.ok) { closePanel(); toast(T.msg_saved, r.data.change_id); refresh(); } else { failed(r); }
      });
    });
  }

  function remove(form) {
    var recurring = form.getAttribute("data-recurring") === "1";
    var go = recurring ? askScope(T.scope_delete) : Promise.resolve("all");
    go.then(function (scope) {
      if (!scope) return;
      var body = { etag: form.getAttribute("data-etag"), scope: scope };
      var inst = form.getAttribute("data-instance");
      if (inst) body.instance = inst;
      var url = "/api/events/" + form.getAttribute("data-collection") + "/" + encodeURIComponent(form.getAttribute("data-name")) + "/delete";
      api("POST", url, body).then(function (r) {
        // Undo instead of "are you sure?" (decision 12).
        if (r.ok) { closePanel(); toast(T.msg_deleted, r.data.change_id); refresh(); } else { failed(r); }
      });
    });
  }

  panel.addEventListener("submit", function (e) {
    if (e.target.id === "ev-form") { e.preventDefault(); save(e.target); }
    if (e.target.id === "cal-prefs") { e.preventDefault(); savePrefs(e.target); }
    if (e.target.classList.contains("cal-share")) { e.preventDefault(); share(e.target); }
    if (e.target.id === "cal-link") { e.preventDefault(); makeLink(e.target); }
    if (e.target.id === "cal-subscribe") { e.preventDefault(); subscribe(e.target); }
  });

  // ---- The calendar list ----------------------------------------------------

  var calBody = document.getElementById("cal-body");
  var listToggle = root.querySelector("[data-cal=list]");
  var LIST_KEY = "calzone.calendarList";
  function wide() { return window.matchMedia("(min-width: 64rem)").matches; }
  function setList(open) {
    calBody.classList.toggle("list-closed", !open);
    listToggle.setAttribute("aria-expanded", open ? "true" : "false");
  }
  function toggleList() {
    var open = calBody.classList.contains("list-closed");
    setList(open);
    try { localStorage.setItem(LIST_KEY, open ? "open" : "closed"); } catch (err) { /* private window */ }
  }
  var remembered = null;
  try { remembered = localStorage.getItem(LIST_KEY); } catch (err) { remembered = null; }
  // Open beside the grid on a desk, folded away on a phone, unless chosen.
  setList(remembered ? remembered === "open" : wide());

  function prefsOf(id) {
    var li = root.querySelector("[data-cal-id='" + id + "']");
    return li ? JSON.parse(li.getAttribute("data-prefs")) : {};
  }
  function postPrefs(id, prefs) {
    return api("POST", "/api/calendars/" + encodeURIComponent(id) + "/prefs", prefs);
  }

  root.addEventListener("change", function (e) {
    if (!e.target.hasAttribute("data-cal-show")) return;
    var li = e.target.closest("[data-cal-id]");
    var id = li.getAttribute("data-cal-id");
    var prefs = prefsOf(id);
    prefs.hidden = !e.target.checked;
    postPrefs(id, prefs).then(function (r) {
      if (r.ok) { li.setAttribute("data-prefs", JSON.stringify(prefs)); refresh(); }
      else { e.target.checked = !e.target.checked; failed(r); }
    });
  });

  function savePrefs(form) {
    var id = form.getAttribute("data-id");
    var prefs = prefsOf(id);
    var f = form.elements;
    prefs.display_name = f.display_name.value.trim() || null;
    // Only a color picked here becomes yours; otherwise you keep following
    // the calendar's own.
    if (f.color.value !== form.getAttribute("data-color")) prefs.color = f.color.value;
    prefs.default_alarm = f.default_alarm.value === "none" ? null : parseInt(f.default_alarm.value, 10);
    postPrefs(id, prefs).then(function (r) {
      if (r.ok) { location.reload(); } else { failed(r); }
    });
  }

  function share(form) {
    fetch(form.action, {
      method: "POST",
      credentials: "same-origin",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams(new FormData(form)).toString()
    }).then(function (r) {
      // Show the sheet again with the new sharing.
      var prefsForm = document.getElementById("cal-prefs");
      if (r.ok && prefsForm) loadPanel("/calendar/settings/" + encodeURIComponent(prefsForm.getAttribute("data-id")));
      else failed({ status: r.status, data: null });
    }, function () { failed({ status: 0 }); });
  }
  // ---- Feeds: secret links and subscriptions (decision 41) -------------------

  function sheetId(sel) {
    var s = panel.querySelector(sel);
    return s ? s.getAttribute("data-id") : null;
  }
  function reloadSheet(id) { loadPanel("/calendar/settings/" + encodeURIComponent(id)); }

  function makeLink(form) {
    var id = sheetId("#cal-links");
    var f = form.elements;
    api("POST", "/api/calendars/" + encodeURIComponent(id) + "/links", { label: f.label.value.trim(), detail: f.detail.value }).then(function (r) {
      if (!r.ok) { failed(r); return; }
      // Shown once: only its hash is kept.
      var box = panel.querySelector(".cal-link-made");
      box.querySelector("[data-link=webcal]").value = r.data.webcal;
      box.querySelector("[data-link=url]").value = r.data.url;
      box.hidden = false;
      form.reset();
      box.querySelector("[data-link=webcal]").select();
    });
  }
  function revokeLink(b) {
    api("POST", "/api/links/" + encodeURIComponent(b.getAttribute("data-id")) + "/revoke", {}).then(function (r) {
      if (r.ok) reloadSheet(sheetId("#cal-links")); else failed(r);
    });
  }
  function copyLink(b) {
    var input = panel.querySelector("[data-link=" + b.getAttribute("data-copy") + "]");
    if (!input) return;
    input.select();
    var done = function () { toast(T.copied); };
    if (navigator.clipboard && window.isSecureContext) {
      navigator.clipboard.writeText(input.value).then(done, function () { /* selected for a manual copy */ });
    } else {
      try { if (document.execCommand("copy")) done(); } catch (err) { /* selected for a manual copy */ }
    }
  }
  function subscribe(form) {
    var f = form.elements;
    var btn = form.querySelector("button[type=submit]");
    btn.disabled = true;
    toast(T.checking_feed);
    api("POST", "/api/subscriptions", { url: f.url.value.trim(), name: f.name.value.trim(), interval: parseInt(f.interval.value, 10) }).then(function (r) {
      btn.disabled = false;
      if (r.ok) location.reload(); else failed(r);
    });
  }
  function subRefresh(b) {
    var id = sheetId("#cal-sub");
    b.disabled = true;
    api("POST", "/api/subscriptions/" + encodeURIComponent(id) + "/refresh", {}).then(function (r) {
      b.disabled = false;
      if (r.ok) { toast(r.data.changed ? T.refreshed : T.up_to_date); refresh(); }
      else failed(r);
      reloadSheet(id);
    });
  }
  function subDelete() {
    var id = sheetId("#cal-sub");
    if (!window.confirm(T.unsubscribe_q)) return;
    api("POST", "/api/subscriptions/" + encodeURIComponent(id) + "/delete", {}).then(function (r) {
      if (r.ok) location.reload(); else failed(r);
    });
  }

  panel.addEventListener("change", function (e) {
    if (e.target.hasAttribute("data-cal-interval")) {
      api("POST", "/api/subscriptions/" + encodeURIComponent(sheetId("#cal-sub")) + "/interval", { minutes: parseInt(e.target.value, 10) }).then(function (r) {
        if (!r.ok) failed(r);
      });
    }
    if (e.target.name === "all_day") {
      panel.querySelectorAll("[data-timed]").forEach(function (el) { el.hidden = e.target.checked; });
    }
    // The end of a repeat: only for the panel's own repeats, and only the
    // field the chosen end needs.
    if (e.target.name === "repeat" || e.target.name === "repeat_end") {
      var g = e.target.form.elements;
      var end = panel.querySelector("[data-repeat-end]");
      if (end) end.hidden = g.repeat.value === "none" || g.repeat.value === "custom";
      g.repeat_until.hidden = g.repeat_end.value !== "until";
      g.repeat_count.hidden = g.repeat_end.value !== "count";
    }
    // Moving the start keeps the end after it.
    if (e.target.name === "start_date") {
      var f = e.target.form.elements;
      if (f.end_date.value < f.start_date.value) f.end_date.value = f.start_date.value;
    }
  });

  // ---- Quick add ------------------------------------------------------------

  var quick = document.getElementById("cal-quick");
  if (quick) {
    quick.addEventListener("submit", function (e) {
      e.preventDefault();
      var input = quick.elements.text;
      var text = input.value.trim();
      if (!text) return;
      api("POST", "/api/quickadd", { text: text }).then(function (r) {
        if (!r.ok) { failed(r); return; }
        var d = r.data;
        if (!d.start || !C.calendars.length) { openNew({ title: d.title }); return; }
        api("POST", "/api/events", { collection: C.calendars[0].id, summary: d.title, start: d.start, end: d.end }).then(function (w) {
          if (w.ok) {
            input.value = "";
            toast(T.msg_added + " " + d.title, w.data.change_id);
            cal.setOption("date", d.start.slice(0, 10));
            refresh();
          } else { failed(w); }
        });
      });
    });
  }

  // ---- Buttons, keys, swipe -------------------------------------------------

  function setView(v) { cal.setOption("view", v); }

  function onClick(e) {
    var b = e.target.closest("[data-cal], [data-view]");
    if (!b) return;
    var v = b.getAttribute("data-view");
    if (v) { setView(v); return; }
    switch (b.getAttribute("data-cal")) {
      case "today": cal.setOption("date", new Date()); break;
      case "prev": cal.prev(); break;
      case "next": cal.next(); break;
      case "new": openNew({}); break;
      case "close": closePanel(); break;
      case "undo": undo(); break;
      case "delete": remove(document.getElementById("ev-form")); break;
      case "list": toggleList(); break;
      case "cal-settings": loadPanel("/calendar/settings/" + encodeURIComponent(b.getAttribute("data-id"))); break;
      case "subscribe": loadPanel("/calendar/subscribe"); break;
      case "revoke-link": revokeLink(b); break;
      case "copy": copyLink(b); break;
      case "sub-refresh": subRefresh(b); break;
      case "sub-delete": subDelete(); break;
    }
  }

  function onKey(e) {
    if (e.ctrlKey || e.metaKey || e.altKey || scopeDialog.open) return;
    if (e.key === "Escape" && !panel.hidden) { closePanel(); return; }
    var tag = (e.target.tagName || "").toLowerCase();
    if (tag === "input" || tag === "textarea" || tag === "select" || e.target.isContentEditable) return;
    switch (e.key) {
      case "t": cal.setOption("date", new Date()); break;
      case "n": if (C.calendars.length) openNew({}); break;
      case "ArrowLeft": case "k": cal.prev(); break;
      case "ArrowRight": case "j": cal.next(); break;
      case "1": case "2": case "3": case "4": setView(VIEWS[+e.key - 1]); break;
      default: return;
    }
    e.preventDefault();
  }

  // Swipe left or right on touch to page; never while dragging an event.
  var sx = null, sy = 0, st = 0;
  grid.addEventListener("pointerdown", function (e) {
    if (e.pointerType !== "touch" || e.target.closest(".ec-event")) { sx = null; return; }
    sx = e.clientX; sy = e.clientY; st = Date.now();
  });
  grid.addEventListener("pointerup", function (e) {
    if (sx === null) return;
    var dx = e.clientX - sx, dy = e.clientY - sy;
    sx = null;
    if (Date.now() - st < 600 && Math.abs(dx) > 60 && Math.abs(dy) < 40) { if (dx < 0) cal.next(); else cal.prev(); }
  });

  // ---- First use: the browser's time zone -----------------------------------

  if (!C.timezone_set) {
    var tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
    if (tz) api("POST", "/api/settings", { timezone: tz }).then(function (r) { if (r.ok) refresh(); });
  }

  return { root: root, onClick: onClick, onKey: onKey };
  }
})();
