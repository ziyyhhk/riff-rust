const test = require('node:test');
const assert = require('node:assert/strict');

require('../src/renderer/scrubber.js');

const { clampProgress } = globalThis.RiffScrubber;

test('progress is clamped to the song timeline', () => {
  assert.equal(clampProgress(15, 60), 0.25);
  assert.equal(clampProgress(999, 60), 1);
  assert.equal(clampProgress(-5, 60), 0);
  assert.equal(clampProgress(15, 0), 0);
  assert.equal(clampProgress(15, Number.NaN), 0);
  assert.equal(clampProgress(Number.NaN, 60), 0);
});
