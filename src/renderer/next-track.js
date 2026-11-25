// Small pure decision helpers for next-track routing. Kept separate from app.js
// so the playlist repeat behavior can be tested without a DOM.
(function (global) {
  'use strict';

  function shouldAutoplayAtEnd(options) {
    const o = options || {};
    const atEnd = typeof o.atEnd === 'boolean' ? o.atEnd : (o.queueIndex + 1 >= (o.queueLength || 0));
    return !o.isRepeat && !o.isRadioQueue && !o.isShuffle && !o.smartShuffle && atEnd && Boolean(o.autoplayEnabled);
  }

  global.RiffNextTrack = { shouldAutoplayAtEnd: shouldAutoplayAtEnd };
})(typeof globalThis !== 'undefined' ? globalThis : window);
