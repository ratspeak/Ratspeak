#!/usr/bin/env node
// Conversation-list timestamps keep current-year dates compact while retaining
// the year when it is needed to disambiguate older history.

'use strict';

var fs = require('fs');
var path = require('path');

var source = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'state.js'), 'utf8');
var start = source.indexOf('var _use12Hour =');
var end = source.indexOf('function _formatClockMinute', start);
if (start === -1 || end === -1) throw new Error('conversation timestamp formatter not found');

var RealDate = Date;
var nowMs = new RealDate(2026, 7, 6, 12, 0, 0).getTime();
class FixedDate extends RealDate {
    constructor() {
        var args = Array.prototype.slice.call(arguments);
        super(...(args.length ? args : [nowMs]));
    }
    static now() { return nowMs; }
}

var formatter = new Function(
    'Date',
    'Intl',
    source.slice(start, end) + '\nreturn { formatConvTime: formatConvTime, dmy: _dateOrderDMY };'
)(FixedDate, Intl);

function epoch(year, month, day) {
    return new RealDate(year, month - 1, day, 12, 0, 0).getTime() / 1000;
}

function expect(label, actual, expected) {
    if (actual !== expected) {
        throw new Error(label + ': expected ' + JSON.stringify(expected) + ', got ' + JSON.stringify(actual));
    }
    process.stdout.write('  ok  ' + label + ' -> ' + JSON.stringify(actual) + '\n');
}

var currentYearDate = formatter.dmy ? '26/07' : '07/26';
var priorYearDate = formatter.dmy ? '26/07/2025' : '07/26/2025';

expect('current-year history omits redundant year', formatter.formatConvTime(epoch(2026, 7, 26)), currentYearDate);
expect('older history retains year', formatter.formatConvTime(epoch(2025, 7, 26)), priorYearDate);
expect('yesterday stays conversational', formatter.formatConvTime(epoch(2026, 8, 5)), 'yesterday');
expect('one-week history stays compact', formatter.formatConvTime(epoch(2026, 7, 30)), '7d');

process.stdout.write('Conversation timestamp tests passed\n');
