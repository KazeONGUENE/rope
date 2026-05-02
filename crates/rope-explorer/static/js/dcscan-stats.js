/**
 * DC Explorer — live stats in top bar (price, gas, finality).
 * Fetches /api/v1/stats and updates the first three .top-bar .top-bar-value elements.
 */
(function () {
  function updateTopBar(stats) {
    var values = document.querySelectorAll('.top-bar .top-bar-value');
    if (values[0]) values[0].textContent = (stats && stats.fatPrice) ? stats.fatPrice : '$0.00390';
    if (values[1]) values[1].textContent = (stats && stats.gasPrice) ? stats.gasPrice : '0.001 gwei';
    if (values[2]) values[2].textContent = (stats && stats.finalityTime) ? stats.finalityTime : '<5s';
  }
  fetch('/api/v1/stats')
    .then(function (r) { return r.ok ? r.json() : Promise.reject(r); })
    .then(updateTopBar)
    .catch(function () { updateTopBar(null); });
})();
