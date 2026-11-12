// CalZone: the few client-side bits HTMX does not cover. No inline handlers
// (the CSP forbids them): everything hangs off data- attributes, delegated
// from document so swapped-in fragments work too. Confirmations use HTMX's
// own hx-confirm.
(function () {
  "use strict";

  // <button data-copy="#selector">: copy that element's text.
  document.addEventListener("click", function (e) {
    var btn = e.target.closest("[data-copy]");
    if (!btn) return;
    var src = document.querySelector(btn.getAttribute("data-copy"));
    if (!src || !navigator.clipboard) return;
    navigator.clipboard.writeText(src.textContent.trim()).then(function () {
      var old = btn.textContent;
      btn.textContent = "Copied";
      setTimeout(function () { btn.textContent = old; }, 1200);
    });
  });

  // Select mode on the contacts list (decision 56): Select all / none and
  // a count. Without JavaScript the boxes still work one by one.
  function pickForm(sel) { return document.querySelector(sel); }
  function pickCount() {
    var form = document.getElementById("contact-pick");
    if (!form) return;
    var boxes = form.querySelectorAll('input[name="pick"]');
    var on = form.querySelectorAll('input[name="pick"]:checked').length;
    var out = form.querySelector("[data-pick-count]");
    if (out) out.textContent = on ? on + " / " + boxes.length : "";
    form.querySelectorAll("[data-pick-all], [data-pick-none]").forEach(function (b) { b.hidden = false; });
  }
  document.addEventListener("click", function (e) {
    var all = e.target.closest("[data-pick-all]");
    var none = e.target.closest("[data-pick-none]");
    if (!all && !none) return;
    var form = pickForm((all || none).getAttribute(all ? "data-pick-all" : "data-pick-none"));
    if (!form) return;
    form.querySelectorAll('input[name="pick"]').forEach(function (b) { b.checked = !!all; });
    pickCount();
  });
  document.addEventListener("change", function (e) {
    if (e.target.matches && e.target.matches('input[name="pick"]')) pickCount();
  });
  document.addEventListener("DOMContentLoaded", pickCount);
  document.addEventListener("htmx:afterSettle", pickCount);

  // Clear the flash box when a new request starts, so old errors do not linger.
  document.addEventListener("htmx:beforeRequest", function () {
    var f = document.getElementById("flash");
    if (f) f.innerHTML = "";
  });
})();
