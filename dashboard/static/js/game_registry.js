(function(global) {
    'use strict';

    var RS = global.RS = global.RS || {};
    RS.games = RS.games || {};

    var views = Object.create(null);
    var APP_ID_PATTERN = /^[a-z][a-z0-9_.-]*$/;

    function register(appId, adapter) {
        if (typeof appId !== 'string' || !APP_ID_PATTERN.test(appId)) {
            throw new TypeError('Game view app_id must use the canonical LRGP grammar');
        }
        if (!adapter || typeof adapter.renderBoard !== 'function' ||
                typeof adapter.bindBoard !== 'function') {
            throw new TypeError('Game view requires renderBoard and bindBoard functions');
        }
        if (views[appId]) {
            throw new Error('Game view already registered: ' + appId);
        }

        views[appId] = Object.freeze({
            appId: appId,
            icon: typeof adapter.icon === 'string' ? adapter.icon : '?',
            themeClass: typeof adapter.themeClass === 'string' ? adapter.themeClass : 'games-theme-unknown',
            boardSelector: typeof adapter.boardSelector === 'string' ? adapter.boardSelector : '',
            actions: Object.freeze(Array.isArray(adapter.actions) ? adapter.actions.slice() : []),
            renderBoard: adapter.renderBoard,
            bindBoard: adapter.bindBoard,
            activeStatusText: typeof adapter.activeStatusText === 'function'
                ? adapter.activeStatusText
                : null,
            detailChips: typeof adapter.detailChips === 'function' ? adapter.detailChips : null,
            renderActiveControls: typeof adapter.renderActiveControls === 'function'
                ? adapter.renderActiveControls
                : null,
            bindControls: typeof adapter.bindControls === 'function' ? adapter.bindControls : null,
            onSessionDelta: typeof adapter.onSessionDelta === 'function' ? adapter.onSessionDelta : null,
            celebrationOptions: typeof adapter.celebrationOptions === 'function'
                ? adapter.celebrationOptions
                : null,
            actionPayload: typeof adapter.actionPayload === 'function' ? adapter.actionPayload : null,
        });
        return views[appId];
    }

    function get(appId) {
        return views[appId] || null;
    }

    function has(appId) {
        return !!views[appId];
    }

    function listIds() {
        return Object.keys(views).sort();
    }

    function supportedManifests(manifests) {
        if (!Array.isArray(manifests)) return [];
        return manifests.filter(function(manifest) {
            return !!(manifest && typeof manifest.app_id === 'string' && has(manifest.app_id));
        });
    }

    RS.games.views = Object.freeze({
        register: register,
        get: get,
        has: has,
        listIds: listIds,
        supportedManifests: supportedManifests,
    });
})(window);
