/* dcscan-pagination.js — small, dependency-free pagination helper.
 *
 * Use:
 *   var pager = DCScanPager.attach({
 *     container: document.querySelector('.pagination'),  // DOM node to render buttons into
 *     pageSize: 25,                                      // items per page
 *     getTotal: function() { return state.totalItems; }, // current total (must be a number)
 *     onPage: function(page) { reload(page); }           // called when user picks a page
 *   });
 *   pager.render(currentPage);
 *
 * Behaviour:
 *   - When total <= pageSize, the container is hidden (display: none)
 *   - Renders prev / numbers / next buttons. Always shows first + last page when many pages
 *   - Click handlers stop event propagation (safe inside cards or rows)
 *   - Disables prev on page 1 and next on last page
 *   - Page numbers cluster around current; uses '…' for gaps
 *
 * Public API:
 *   pager.render(page)   re-render with a given current page (1-based)
 *   pager.go(page)       jump to a page programmatically (calls onPage)
 *   pager.totalPages()   total pages based on current getTotal()
 */
(function (root) {
    'use strict';

    function clamp(n, min, max) { return Math.max(min, Math.min(max, n)); }

    function buildPageList(current, total) {
        // Returns array of page numbers + 'gap' markers, e.g. [1,2,3,'…',8,9,10]
        if (total <= 7) {
            var arr = [];
            for (var i = 1; i <= total; i++) arr.push(i);
            return arr;
        }
        var pages = [1];
        var start = Math.max(2, current - 2);
        var end = Math.min(total - 1, current + 2);
        if (start > 2) pages.push('…');
        for (var p = start; p <= end; p++) pages.push(p);
        if (end < total - 1) pages.push('…');
        pages.push(total);
        return pages;
    }

    function attach(opts) {
        var container = opts.container;
        var pageSize = opts.pageSize || 25;
        var getTotal = opts.getTotal || function () { return 0; };
        var onPage = opts.onPage || function () {};
        var current = 1;

        function totalPages() {
            var t = getTotal() || 0;
            return Math.max(1, Math.ceil(t / pageSize));
        }

        function render(page) {
            current = clamp(page || current || 1, 1, totalPages());
            if (!container) return;

            var total = getTotal() || 0;
            if (total <= pageSize) {
                container.style.display = 'none';
                container.innerHTML = '';
                return;
            }
            container.style.display = '';

            var totalPg = totalPages();
            var html = '';
            var prevDisabled = current <= 1 ? ' disabled' : '';
            var nextDisabled = current >= totalPg ? ' disabled' : '';
            html += '<button type="button" class="page-btn" data-page="' + (current - 1) + '"' + prevDisabled + '><i class="fas fa-angle-left"></i></button>';

            var pages = buildPageList(current, totalPg);
            for (var i = 0; i < pages.length; i++) {
                var p = pages[i];
                if (p === '…') {
                    html += '<span class="page-gap" style="padding: 0.5rem 0.25rem; color: var(--gray-400);">…</span>';
                } else {
                    var active = p === current ? ' active' : '';
                    html += '<button type="button" class="page-btn' + active + '" data-page="' + p + '">' + p + '</button>';
                }
            }
            html += '<button type="button" class="page-btn" data-page="' + (current + 1) + '"' + nextDisabled + '><i class="fas fa-angle-right"></i></button>';
            container.innerHTML = html;

            // Attach handlers
            var btns = container.querySelectorAll('button.page-btn[data-page]');
            for (var j = 0; j < btns.length; j++) {
                btns[j].addEventListener('click', function (ev) {
                    ev.stopPropagation();
                    var p = parseInt(this.getAttribute('data-page'), 10);
                    if (this.disabled || isNaN(p) || p < 1 || p > totalPages()) return;
                    go(p);
                });
            }
        }

        function go(page) {
            page = clamp(page, 1, totalPages());
            current = page;
            try { onPage(page); } catch (e) { console && console.error && console.error(e); }
            render(page);
        }

        return { render: render, go: go, totalPages: totalPages, current: function () { return current; } };
    }

    root.DCScanPager = { attach: attach, buildPageList: buildPageList };
})(typeof window !== 'undefined' ? window : this);
