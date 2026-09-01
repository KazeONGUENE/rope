/**
 * DC Explorer - live top-bar stats (price, gas, finality) from /api/v1/stats.
 *
 * 2026-08-14 refresh loop + non-destructive failure:
 *   - Poll /api/v1/stats every 15 s (was: fired once on page load only,
 *     so the top-bar `DC FAT Price` / `Gas` values on any open tab were
 *     frozen at whatever they were when the tab was first opened).
 *   - Never overwrite a good value with an em-dash on a transient
 *     failure. Only paint the em-dash if we have NEVER succeeded on
 *     this page. Previously a single failed tick painted "-" and the
 *     top-bar looked broken until the next successful tick.
 *
 * 2026-08-11 rewrite:
 *   - Prefer ID-based targets (`#topbar-fat-price`, `#topbar-fat-change`,
 *     `#topbar-gas`) so a page can wire only the fields it wants without
 *     depending on the position of `.top-bar-value` in the DOM. Falls back
 *     to the historical positional selector for pages that haven't been
 *     migrated yet.
 *   - On fetch failure, display an honest en-dash ("-") instead of the
 *     stale 2026-Q1 defaults ($0.00390 / 0.001 gwei) - those defaults were
 *     misleading whenever /api/v1/stats was briefly unreachable and made
 *     every screenshot of a degraded moment look like real live data.
 *   - Render 24 h change with the right color; keep the rest untouched.
 *   - Never throws; a broken JSON response degrades to plain hyphens.
 */
(function () {
  var PLACEHOLDER = '-';
  var REFRESH_MS = 15000;
  var hasEverSucceeded = false;

  function setById(id, text, style) {
    var el = document.getElementById(id);
    if (!el) return false;
    el.textContent = text;
    if (style) {
      for (var k in style) if (Object.prototype.hasOwnProperty.call(style, k)) {
        el.style[k] = style[k];
      }
    }
    return true;
  }

  function updateTopBar(stats) {
    var fatPrice = (stats && stats.fatPrice) || PLACEHOLDER;
    var gas = (stats && stats.gasPrice) || PLACEHOLDER;
    var finality = (stats && stats.finalityTime) || '<5s';

    // 24h change formatting (optional; only if the endpoint returns it).
    var changeText = '';
    var changeColor = '';
    var rawChange = stats && stats.fatPriceChange24h;
    if (rawChange != null && rawChange !== '') {
      var s = String(rawChange).trim();
      // Accept "-2.94%", "+1.23%", "0.5", 0.5, etc.
      var num = parseFloat(s.replace(/[^0-9.\-+]/g, ''));
      if (!isNaN(num)) {
        var sign = num > 0 ? '+' : '';
        changeText = sign + num.toFixed(2) + '%';
        changeColor = num > 0 ? '#22c55e' : (num < 0 ? '#ef4444' : '');
      }
    }

    // Preferred: ID-based targets (address page + any page opted in).
    var wiredById =
      setById('topbar-fat-price', fatPrice) &&
      setById('topbar-gas', gas);
    setById('topbar-fat-change', changeText, changeColor ? { color: changeColor } : null);

    if (wiredById) return;

    // Fallback: legacy positional selector (pages not yet migrated).
    var values = document.querySelectorAll('.top-bar .top-bar-value');
    if (values[0]) values[0].textContent = fatPrice;
    if (values[1]) values[1].textContent = gas;
    if (values[2]) values[2].textContent = finality;
  }

  function tick() {
    fetch('/api/v1/stats', { cache: 'no-store' })
      .then(function (r) { return r.ok ? r.json() : Promise.reject(r); })
      .then(function (stats) {
        hasEverSucceeded = true;
        updateTopBar(stats);
      })
      .catch(function () {
        // Non-destructive: only paint placeholders on the very first
        // load (before any success). Subsequent transient failures
        // keep the last-known-good values so the top-bar does not
        // flicker to hyphens for a full 15 s interval on a single
        // failed tick.
        if (!hasEverSucceeded) updateTopBar(null);
      });
  }

  tick();
  setInterval(tick, REFRESH_MS);
})();
