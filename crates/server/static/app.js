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

  // Clear the flash box when a new request starts, so old errors do not linger.
  document.addEventListener("htmx:beforeRequest", function () {
    var f = document.getElementById("flash");
    if (f) f.innerHTML = "";
  });
})();
