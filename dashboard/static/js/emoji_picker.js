(function() {
    'use strict';

    var RECENT_KEY = 'ratspeak_recent_emoji';
    var MAX_RECENT = 32;

    function getRecent() {
        try {
            var data = localStorage.getItem(RECENT_KEY);
            return data ? JSON.parse(data) : [];
        } catch (e) { return []; }
    }

    function saveRecent(emoji) {
        var recent = getRecent();
        recent = recent.filter(function(e) { return e !== emoji; });
        recent.unshift(emoji);
        if (recent.length > MAX_RECENT) recent = recent.slice(0, MAX_RECENT);
        try { localStorage.setItem(RECENT_KEY, JSON.stringify(recent)); } catch (e) {}
    }

    function EmojiPicker(opts) {
        this.trigger = opts.trigger;
        this.anchor = opts.anchor || null;
        this.container = opts.container || document.body;
        this.onSelect = opts.onSelect || function() {};
        this.position = opts.position || 'top';
        this.el = null;
        this._open = false;
        this._searchTerm = '';
        this._activeCategory = 'recent';
        this._boundClose = this._onDocClick.bind(this);
        this._boundKeydown = this._onKeydown.bind(this);

        if (this.trigger) {
            this.trigger.addEventListener('click', this.toggle.bind(this));
        }
    }

    EmojiPicker.prototype.toggle = function() {
        if (this._open) this.close(); else this.open();
    };

    EmojiPicker.prototype.open = function() {
        if (this._open) return;
        this._open = true;
        this._build();
        document.addEventListener('mousedown', this._boundClose);
        document.addEventListener('keydown', this._boundKeydown);
    };

    EmojiPicker.prototype.close = function() {
        if (!this._open) return;
        this._open = false;
        if (this.el && this.el.parentNode) {
            this.el.parentNode.removeChild(this.el);
        }
        this.el = null;
        document.removeEventListener('mousedown', this._boundClose);
        document.removeEventListener('keydown', this._boundKeydown);
    };

    EmojiPicker.prototype.destroy = function() {
        this.close();
        if (this.trigger) {
            this.trigger.removeEventListener('click', this.toggle.bind(this));
        }
    };

    EmojiPicker.prototype._onDocClick = function(e) {
        if (this.el && !this.el.contains(e.target) && e.target !== this.trigger) {
            this.close();
        }
    };

    EmojiPicker.prototype._onKeydown = function(e) {
        if (e.key === 'Escape') {
            e.preventDefault();
            this.close();
        }
    };

    EmojiPicker.prototype._build = function() {
        if (this.el) {
            if (this.el.parentNode) this.el.parentNode.removeChild(this.el);
        }

        var picker = document.createElement('div');
        picker.className = 'emoji-picker';
        this.el = picker;

        var search = document.createElement('input');
        search.type = 'text';
        search.className = 'emoji-picker-search';
        search.placeholder = 'Search emoji...';
        search.setAttribute('autocomplete', 'off');
        disableAutoCorrect(search);
        picker.appendChild(search);

        var cats = document.createElement('div');
        cats.className = 'emoji-picker-categories';
        var self = this;

        if (typeof EMOJI_DATA === 'undefined') {
            picker.innerHTML = '<div class="emoji-picker-load-error">Emoji data not loaded</div>';
            document.body.appendChild(picker);
            return;
        }

        EMOJI_DATA.categories.forEach(function(cat) {
            var btn = document.createElement('button');
            btn.className = 'emoji-picker-category-btn' + (cat.id === self._activeCategory ? ' active' : '');
            btn.textContent = cat.icon;
            btn.title = cat.name;
            btn.setAttribute('data-cat', cat.id);
            btn.addEventListener('click', function(e) {
                e.preventDefault();
                e.stopPropagation();
                self._activeCategory = cat.id;
                self._searchTerm = '';
                search.value = '';
                self._renderGrid();
                cats.querySelectorAll('.emoji-picker-category-btn').forEach(function(b) {
                    b.classList.toggle('active', b.getAttribute('data-cat') === cat.id);
                });
            });
            cats.appendChild(btn);
        });
        picker.appendChild(cats);

        var grid = document.createElement('div');
        grid.className = 'emoji-picker-grid';
        picker.appendChild(grid);
        this._grid = grid;

        var searchTimer = null;
        search.addEventListener('input', function() {
            if (searchTimer) clearTimeout(searchTimer);
            searchTimer = setTimeout(function() {
                self._searchTerm = search.value.trim().toLowerCase();
                self._renderGrid();
            }, 150);
        });

        // Appended to body so fixed positioning avoids `overflow: hidden` clipping.
        document.body.appendChild(picker);

        this._positionPicker();
        this._renderGrid();
        if (!isMobile()) search.focus();
    };

    EmojiPicker.prototype._positionPicker = function() {
        if (!this.el) return;
        var anchor = this.anchor || this.trigger || this.container;
        if (!anchor) return;
        var rect = anchor.getBoundingClientRect();
        var safe = safeAreaInsets();
        var minTop = safe.top + 8;
        var minLeft = safe.left + 8;
        var maxRight = window.innerWidth - safe.right - 8;
        var maxBottom = window.innerHeight - safe.bottom - 8;
        var pickerHeight = 320;
        var pickerWidth = Math.min(360, maxRight - minLeft);

        var top = rect.top - pickerHeight - 4;
        var left = rect.left;

        if (top < minTop) {
            top = rect.bottom + 4;
        }

        if (left + pickerWidth > maxRight) {
            left = maxRight - pickerWidth;
        }
        if (left < minLeft) left = minLeft;

        if (top + pickerHeight > maxBottom) {
            top = maxBottom - pickerHeight;
        }
        if (top < minTop) top = minTop;

        this.el.style.left = left + 'px';
        this.el.style.top = top + 'px';
        this.el.style.width = pickerWidth + 'px';
    };

    EmojiPicker.prototype._renderGrid = function() {
        var grid = this._grid;
        if (!grid) return;
        grid.innerHTML = '';

        var self = this;
        var emojis = [];

        if (this._searchTerm) {
            var q = this._searchTerm;
            Object.keys(EMOJI_DATA.emojis).forEach(function(catId) {
                EMOJI_DATA.emojis[catId].forEach(function(em) {
                    if (em.n.indexOf(q) !== -1 || em.k.indexOf(q) !== -1 || em.e === q) {
                        emojis.push(em);
                    }
                });
            });
        } else if (this._activeCategory === 'recent') {
            var recent = getRecent();
            recent.forEach(function(e) {
                emojis.push({ e: e, n: '', k: '' });
            });
            if (emojis.length === 0) {
                grid.innerHTML = '<div class="emoji-picker-empty">No recent emoji</div>';
                return;
            }
        } else {
            emojis = EMOJI_DATA.emojis[this._activeCategory] || [];
        }

        if (emojis.length === 0 && this._searchTerm) {
            grid.innerHTML = '<div class="emoji-picker-empty">No results</div>';
            return;
        }

        emojis.forEach(function(em) {
            var btn = document.createElement('button');
            btn.className = 'emoji-picker-item';
            var display = em.e;
            btn.textContent = display;
            btn.title = em.n || display;
            var touchSelected = false;
            function selectEmoji(e) {
                e.preventDefault();
                e.stopPropagation();
                saveRecent(em.e);
                self.onSelect(display);
            }
            btn.addEventListener('mousedown', function(e) { e.preventDefault(); });
            btn.addEventListener('touchstart', function(e) { e.preventDefault(); }, { passive: false });
            btn.addEventListener('touchend', function(e) {
                var t = (e.changedTouches && e.changedTouches[0]) || null;
                if (t) {
                    var hit = document.elementFromPoint(t.clientX, t.clientY);
                    if (hit !== btn && !btn.contains(hit)) return;
                }
                touchSelected = true;
                setTimeout(function() { touchSelected = false; }, 500);
                selectEmoji(e);
            }, { passive: false });
            btn.addEventListener('click', function(e) {
                if (touchSelected) {
                    e.preventDefault();
                    e.stopPropagation();
                    return;
                }
                selectEmoji(e);
            });
            grid.appendChild(btn);
        });
    };

    window.EmojiPicker = EmojiPicker;

})();
