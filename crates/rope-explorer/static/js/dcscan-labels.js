/**
 * DCScan Address Labels - client-side label resolution.
 *
 * Fetches /api/v1/labels once, caches in sessionStorage + memory.
 * Provides addrDisplay() for rendering addresses with labels.
 *
 * Hidden addresses (e.g. DC Treasury) NEVER expose the raw hex.
 * The label is the only thing shown - no toggle, no tooltip, no reveal.
 *
 * Non-hidden labeled addresses show the hex by default with a small
 * toggle to switch to the label view (preference persists in localStorage).
 */
(function () {
  'use strict';

  var CACHE_KEY = 'dcscan_labels';
  var CACHE_TTL = 5 * 60 * 1000;
  var PREF_KEY = 'dcscan_label_prefs';

  var _labels = null;
  var _ready = false;
  var _queue = [];

  function getPrefs() {
    try { return JSON.parse(localStorage.getItem(PREF_KEY) || '{}'); }
    catch (e) { return {}; }
  }
  function savePref(addrLower, showLabel) {
    var p = getPrefs();
    p[addrLower] = showLabel;
    try { localStorage.setItem(PREF_KEY, JSON.stringify(p)); } catch (e) {}
  }

  function loadLabels(cb) {
    try {
      var cached = sessionStorage.getItem(CACHE_KEY);
      if (cached) {
        var parsed = JSON.parse(cached);
        if (parsed._ts && Date.now() - parsed._ts < CACHE_TTL) {
          _labels = parsed.labels || {};
          _ready = true;
          if (cb) cb();
          return;
        }
      }
    } catch (e) {}

    fetch('/api/v1/labels')
      .then(function (r) { return r.ok ? r.json() : Promise.reject(r); })
      .then(function (data) {
        _labels = data.labels || {};
        _ready = true;
        try {
          sessionStorage.setItem(CACHE_KEY, JSON.stringify({ labels: _labels, _ts: Date.now() }));
        } catch (e) {}
        if (cb) cb();
        _queue.forEach(function (fn) { fn(); });
        _queue = [];
      })
      .catch(function () {
        _labels = {};
        _ready = true;
        if (cb) cb();
        _queue.forEach(function (fn) { fn(); });
        _queue = [];
      });
  }

  function getTag(addr) {
    if (!addr || !_labels) return null;
    return _labels[addr.toLowerCase()] || null;
  }

  function shortHex(addr, n) {
    n = n || 6;
    if (!addr || addr.length <= n * 2 + 2) return addr || '\u2014';
    return addr.slice(0, n + 2) + '\u2026' + addr.slice(-n);
  }

  function escHtml(s) {
    var d = document.createElement('div');
    d.appendChild(document.createTextNode(s || ''));
    return d.innerHTML;
  }

  /**
   * Build the display HTML for an address.
   *
   * If the address is hidden (fromHidden/toHidden from API, or hidden
   * flag in the label registry), only the label chip is shown. The raw
   * hex is never rendered - not in text, not in href, not in tooltip.
   *
   * @param {string|null} addr  Raw hex address (null for hidden addresses)
   * @param {object} [opts]     Options:
   *   - label:   Label string (from API: fromLabel / toLabel)
   *   - hidden:  true if address is redacted (from API: fromHidden / toHidden)
   *   - short:   Number of hex chars to show (default 6)
   *   - link:    Wrap in <a> to /address/addr (default true; forced false if hidden)
   *   - icon:    Override icon class
   * @returns {string} HTML string
   */
  function addrDisplay(addr, opts) {
    opts = opts || {};
    var tag = addr ? getTag(addr) : null;
    var label = opts.label || (tag && tag.label) || null;
    var hidden = opts.hidden === true || (tag && tag.hidden === true);
    var icon = opts.icon || (tag && tag.icon) || null;
    var shortLen = opts.short || 6;
    var doLink = opts.link !== false && !hidden;

    // ---- HIDDEN: label only, no hex anywhere ----
    if (hidden) {
      if (!label) return '<span class="mono">\u2014</span>';
      var iconH = icon ? '<i class="fas ' + escHtml(icon) + '" style="margin-right:3px;font-size:0.85em;opacity:0.7;"></i>' : '';
      return '<span class="addr-label-tag" data-cat="treasury">' + iconH + escHtml(label) + '</span>';
    }

    if (!addr) return '<span class="mono">\u2014</span>';

    // ---- Non-hidden labeled address: hex default, toggle to label ----
    if (label) {
      var prefs = getPrefs();
      var lower = addr.toLowerCase();
      var showLabel = prefs[lower] === true;
      var uid = 'lbl_' + lower.slice(2, 10) + '_' + Math.random().toString(36).slice(2, 6);

      var iconHtml = icon ? '<i class="fas ' + escHtml(icon) + '" style="margin-right:3px;font-size:0.85em;opacity:0.7;"></i>' : '';
      var labelSpan = '<span class="addr-label-tag">' + iconHtml + escHtml(label) + '</span>';
      var rawSpan = '<span class="mono">' + escHtml(shortHex(addr, shortLen)) + '</span>';

      var rawStyle = showLabel ? ' style="display:none;"' : '';
      var lblStyle = showLabel ? '' : ' style="display:none;"';

      var content = '';
      if (doLink) {
        content = '<a href="/address/' + escHtml(addr) + '" class="addr-display-link">' +
          '<span id="' + uid + '_raw"' + rawStyle + '>' + rawSpan + '</span>' +
          '<span id="' + uid + '_lbl"' + lblStyle + '>' + labelSpan + '</span>' +
          '</a>';
      } else {
        content = '<span id="' + uid + '_raw"' + rawStyle + '>' + rawSpan + '</span>' +
          '<span id="' + uid + '_lbl"' + lblStyle + '>' + labelSpan + '</span>';
      }

      content += ' <button class="addr-toggle-btn" data-uid="' + uid + '" data-addr="' + escHtml(lower) +
        '" title="Toggle label / address" aria-label="Toggle label">' +
        '<i class="fas fa-exchange-alt"></i></button>';
      return content;
    }

    // ---- Unlabeled address: just hex ----
    var hexDisplay = '<span class="mono">' + escHtml(shortHex(addr, shortLen)) + '</span>';
    if (doLink) {
      return '<a href="/address/' + escHtml(addr) + '" class="addr-display-link">' + hexDisplay + '</a>';
    }
    return hexDisplay;
  }

  // Toggle handler (only for non-hidden labeled addresses)
  document.addEventListener('click', function (e) {
    var btn = e.target.closest('.addr-toggle-btn');
    if (!btn) return;
    e.preventDefault();
    e.stopPropagation();

    var uid = btn.getAttribute('data-uid');
    var addrLower = btn.getAttribute('data-addr');

    var rawEl = document.getElementById(uid + '_raw');
    var lblEl = document.getElementById(uid + '_lbl');
    if (!rawEl || !lblEl) return;

    var rawVisible = rawEl.style.display !== 'none';
    rawEl.style.display = rawVisible ? 'none' : '';
    lblEl.style.display = rawVisible ? '' : 'none';

    savePref(addrLower, rawVisible);
  });

  window.DCScanLabels = {
    load: loadLabels,
    ready: function () { return _ready; },
    onReady: function (fn) { if (_ready) fn(); else _queue.push(fn); },
    getTag: getTag,
    addrDisplay: addrDisplay,
    shortHex: shortHex,
    escHtml: escHtml,
    getPrefs: getPrefs,
    savePref: savePref
  };

  loadLabels();
})();
