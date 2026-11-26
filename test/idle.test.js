const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const app = fs.readFileSync(path.join(__dirname, '..', 'src', 'renderer', 'app.js'), 'utf8');

test('idle work is throttled instead of polling every half second', () => {
  assert.ok(app.includes('setInterval(refreshBrowsingPresence, 120000)'));
  assert.ok(app.includes('setInterval(refreshDiscordRpcStatus, 60 * 1000)'));
  assert.ok(app.includes('setInterval(scheduleTrim, 5 * 60 * 1000)'));
  assert.ok(app.includes('setInterval(triggerRendererMemoryTrim, 5 * 60 * 1000)'));
  assert.ok(!app.includes('setInterval(refreshBrowsingPresence, 30000)'));
  assert.ok(!app.includes('setInterval(refreshDiscordRpcStatus, 5000)'));
  assert.ok(!app.includes('setInterval(scheduleTrim, 12000)'));
});

test('visible idle performs one memory trim after genuine inactivity', () => {
  assert.ok(app.includes('}, 45000);'));
  assert.ok(app.includes('const scheduleVisibleTrim = () => {'));
  assert.ok(app.includes("el.nativeAudio.addEventListener('pause', scheduleVisibleTrim"));
});

test('memory trims only run while hidden', () => {
  assert.ok(app.includes('if (!document.hidden || trimArmed || trimPending || state.isPlaying) return;'));
  assert.ok(app.includes('if (!document.hidden || state.isPlaying || state.isLoadingTrack) return;'));
});
