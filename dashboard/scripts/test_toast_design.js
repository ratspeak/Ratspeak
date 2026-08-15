#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');

var dashboardRoot = path.join(__dirname, '..');
var toastJs = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'toasts.js'), 'utf8');
var components = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '07-components.css'), 'utf8');
var responsive = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '13-responsive.css'), 'utf8');
var animations = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '12-animations.css'), 'utf8');
var index = fs.readFileSync(path.join(dashboardRoot, 'index.html'), 'utf8');

assert(toastJs.includes("status.className = 'toast-status'"),
    'toasts must carry a compact semantic status mark');
assert(toastJs.includes("status.setAttribute('aria-hidden', 'true')"),
    'the decorative status mark must stay out of the accessibility tree');
assert(toastJs.includes("msgSpan.className = 'toast-message'"),
    'toast copy must use the shared presentation class');
assert(toastJs.includes("toast.setAttribute('role', 'alert')"),
    'error toasts must retain assertive announcement semantics');
assert(toastJs.includes("var toastKey = message + '|' + colorClass"),
    'the visual refresh must preserve duplicate suppression');
assert(toastJs.includes('setTimeout(dismissToast, duration)'),
    'the visual refresh must preserve toast timing');

var toastRule = components.match(/\.toast\s*\{([\s\S]*?)\n\}/);
assert(toastRule && /grid-template-columns:\s*28px minmax\(0, 1fr\) 32px/.test(toastRule[1]),
    'toast layout must give status, message, and dismissal deliberate columns');
assert(toastRule && /background:\s*var\(--surface-popover\)/.test(toastRule[1]),
    'toast cards must use the active theme surface');
assert(!toastRule[1].includes('border-left'),
    'toast cards must not return to the colored left-rail treatment');
assert(!/\.toast\.toast-(?:blue|green|red|orange|purple)[^{]*\{[^}]*linear-gradient/s.test(components),
    'toast variants must not wash the entire card in a status gradient');
assert(/\.toast\.toast-yellow\s*\{[^}]*--toast-tone:\s*var\(--status-warning-fg\)/s.test(components),
    'the existing yellow warning alias must receive the warning presentation');
assert(/\.toast-close:focus-visible\s*\{[^}]*outline:\s*2px solid var\(--focus-ring\)/s.test(components),
    'toast dismissal must expose keyboard focus');
assert(/@media \(max-width:\s*768px\)[\s\S]*?#toast-container[\s\S]*?top:\s*calc\(8px \+ var\(--sat\)\)/.test(responsive),
    'mobile toasts must respect the device safe area');
assert(/@media \(max-width:\s*768px\)[\s\S]*?\.toast\s*\{[^}]*grid-template-columns:\s*28px minmax\(0, 1fr\) 44px/s.test(responsive),
    'the mobile toast grid must reserve the complete dismissal touch target');
assert(/@media \(prefers-reduced-motion:\s*reduce\)[\s\S]*?transition-duration:\s*0\.01ms !important/.test(animations),
    'toast motion must inherit the global reduced-motion contract');
var styleRevision = index.match(/\/static\/style\.css\?v=([^"']+)/);
var toastRevision = index.match(/\/static\/js\/toasts\.js\?v=([^"']+)/);
assert(styleRevision && toastRevision && styleRevision[1] === toastRevision[1],
    'toast presentation assets must share the dashboard build revision');

console.log('toast design contract tests passed');
