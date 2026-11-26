// Pure progress math for the song scrubber, kept testable.
(function (global) {
  'use strict';

  function clampProgress(currentTime, duration) {
    if (!Number.isFinite(currentTime) || !Number.isFinite(duration) || duration <= 0) return 0;
    return Math.max(0, Math.min(1, currentTime / duration));
  }

  global.RiffScrubber = { clampProgress: clampProgress };
})(typeof globalThis !== 'undefined' ? globalThis : window);
