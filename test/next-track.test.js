const test = require('node:test');
const assert = require('node:assert/strict');

require('../src/renderer/next-track.js');

const { shouldAutoplayAtEnd } = globalThis.RiffNextTrack;

const playlistAtLastTrack = {
  queueLength: 2,
  queueIndex: 1,
  atEnd: true,
  isRepeat: true,
  isShuffle: false,
  smartShuffle: false,
  isRadioQueue: false,
  autoplayEnabled: true
};

test('repeat playlist at last track does not fall into random autoplay', () => {
  assert.equal(shouldAutoplayAtEnd(playlistAtLastTrack), false);
});

test('autoplay still starts at the end when repeat is off', () => {
  assert.equal(shouldAutoplayAtEnd({ ...playlistAtLastTrack, isRepeat: false }), true);
});

test('autoplay still starts at the end with repeat on when shuffle is active', () => {
  assert.equal(shouldAutoplayAtEnd({ ...playlistAtLastTrack, isShuffle: true }), false);
});
