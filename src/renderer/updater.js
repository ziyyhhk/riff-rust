// Riff Rust enhanced auto-updater (additive).
// Injects its own modal DOM at runtime; styles live in updater.css.
// Does not touch the existing update sheet in app.js/index.html.
// Bridge: window.electronAPI (see electron-shim.js). Rust side:
//   get_app_version() -> "0.1.24"
//   check_for_updates() -> { available: false } today (stub, no feed wired),
//     so we also accept richer shapes and fall back to the GitHub releases
//     API exactly like app.js's checkDirectFromRenderer does.
//   update_action(action) accepts free-form strings; app.js uses 'now'/'ignore'.
//   installUpdateNow() is a stub in the port; installUpdateQuiet() maps to
//     install_update_if_newer, the real silent installer.
(() => {
  'use strict';

  const api = window.electronAPI;
  if (!api || typeof api.checkForUpdates !== 'function' || typeof api.getAppVersion !== 'function') return;

  const APP_NAME = 'Riff Rust';
  const CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000; // ~6h
  const INITIAL_DELAY_MS = 7000; // let boot + the existing quiet-install pass settle
  const RELEASE_API = 'https://api.github.com/repos/ziyyhhk/riff-rust/releases/latest';
  const RELEASE_PAGE = 'https://github.com/ziyyhhk/riff-rust/releases/latest';

  const KEY_SKIPPED = 'riff_skipped_update_version';
  const KEY_LEGACY_IGNORED = 'riff_ignored_update_ver'; // existing sheet's "Later" key
  const KEY_SETTINGS = 'riff_update_settings';
  const KEY_HISTORY = 'riff_update_history';

  // Short, honest changelog bullets for recent releases (from docs/releases).
  const CHANGELOG = {
    '0.1.24': [
      'Removed the automatic silent update that could replace the app while you were reading.',
      'The changelog now shows only the newest release.',
      'Updates happen only when you press Update Now.'
    ],
    '0.1.22': [
      'Smart Shuffle v2: its own sparkle button in the player, mini player and playlist header.',
      'Smart shuffle never repeats the same artist back to back and spreads the last few artists apart.',
      'Playlists honour smart shuffle: Play starts on a random track and every next track is picked randomly.',
      'Hover a track for 15 seconds of the best part, then the preview cache is deleted a few minutes later.',
      'Storage settings: auto-clear the cache (default 7 days) and offline library (default 30 days), or set Never.',
      'Startup tool download progress now starts at 0% instead of sitting at 100%.',
      'The updater has a full UI with changelog, update settings, history, and a Skip version button.'
    ],
    '0.1.21': [
      'Long lists (library, playlists, favorites, history) now render only the visible rows, so big libraries scroll smoothly.',
      'New "Add local" button in playlist headers imports local files with tags and cover art, with duplicate protection.',
      'Add files / Add folder in the local view now return proper tracks and populate the library.',
      'Stream loading fixes: forced re-resolve support and no stale caching of short-lived HLS URLs.',
      'Synced upstream Riff v0.7.6 fixes while keeping gapless, lucky button, queue dedup and universal import.'
    ],
    '0.1.20': [
      'Universal import: any file ffmpeg can read is converted to mp3 on import, keeping title and artist tags.',
      'Headset disconnect pause: playback pauses when the audio device drops instead of switching to speakers.',
      'Gapless handoff for consecutive local cached tracks.',
      'I\'m Feeling Lucky: one-tap queue built from favorites, history and daily mixes.',
      'New scalloped R emblem across the window, tray, about and welcome screens.',
      'Album track lists load, lyrics matching is more reliable, and the queue is deduplicated.'
    ],
    '0.1.19': [
      'Custom data directory override now persists and takes effect on the next launch.',
      'Plugins actually load and run when enabled.',
      'Saving default settings writes the file and tells you where it went.',
      'Offline library shows real titles and artists read from file tags.',
      'Steadier playback: readable stream errors, better retries, instant starts for cached tracks.',
      'Charts fall back to the cached chart instead of showing an empty success state.'
    ]
  };

  // ------------------------------------------------------------- storage

  function lsGet(key) {
    try { return localStorage.getItem(key); } catch (_) { return null; }
  }
  function lsSet(key, value) {
    try { localStorage.setItem(key, value); } catch (_) {}
  }
  function readJSON(key, fallback) {
    try {
      const raw = lsGet(key);
      if (!raw) return fallback;
      const parsed = JSON.parse(raw);
      return parsed == null ? fallback : parsed;
    } catch (_) { return fallback; }
  }

  function loadSettings() {
    const s = readJSON(KEY_SETTINGS, {});
    return {
      autoCheck: s.autoCheck !== false,   // default on
      autoInstall: s.autoInstall === true // default off
    };
  }
  function saveSettings() { lsSet(KEY_SETTINGS, JSON.stringify(settings)); }

  function loadHistory() {
    const h = readJSON(KEY_HISTORY, []);
    return Array.isArray(h) ? h : [];
  }
  function recordInstall(toVersion) {
    const history = loadHistory();
    history.unshift({ version: toVersion, from: currentVersion, at: new Date().toISOString() });
    lsSet(KEY_HISTORY, JSON.stringify(history.slice(0, 25)));
  }

  // ------------------------------------------------------------- versions

  function normVer(v) { return String(v == null ? '' : v).trim().replace(/^v/i, ''); }

  function isNewer(latest, current) {
    const a = normVer(latest).split('.').map((n) => parseInt(n, 10) || 0);
    const b = normVer(current).split('.').map((n) => parseInt(n, 10) || 0);
    for (let i = 0; i < Math.max(a.length, b.length); i++) {
      const x = a[i] || 0;
      const y = b[i] || 0;
      if (x > y) return true;
      if (x < y) return false;
    }
    return false;
  }

  function isSkipped(version) {
    const v = normVer(version);
    return !!v && (normVer(lsGet(KEY_SKIPPED)) === v || normVer(lsGet(KEY_LEGACY_IGNORED)) === v);
  }

  // ------------------------------------------------------------- state

  let currentVersion = '';
  let latestInfo = null;      // { version, releaseNotes, publishedAt, htmlUrl }
  let settings = loadSettings();
  let isOpen = false;
  let checking = false;
  let installing = false;
  let autoTimer = null;
  let statusTimer = null;
  let lastFocused = null;
  let ui = null;              // built lazily on first open

  // ------------------------------------------------------------- DOM helpers

  function el(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text != null) node.textContent = text;
    return node;
  }
  function icon(name) {
    const span = el('span', 'material-symbols-rounded riff-updx-icon', name);
    span.setAttribute('aria-hidden', 'true');
    return span;
  }

  // ------------------------------------------------------------- changelog

  function notesToBullets(raw) {
    // Minimal markdown cleanup for feed-provided notes; keep it plain text.
    const lines = String(raw || '')
      .replace(/<[^>]+>/g, '')
      .split(/\r?\n/)
      .map((l) => l.replace(/^#{1,6}\s+/, '').replace(/^\s*[-*•]\s+/, '').replace(/\*\*([^*]+)\*\*/g, '$1').replace(/`([^`]+)`/g, '$1').trim())
      .filter((l) => l && !/^!\[/.test(l));
    return lines.slice(0, 8);
  }

  function renderChangelog() {
    const box = ui.changelog;
    box.textContent = '';
    if (!latestInfo) {
      box.appendChild(el('p', 'riff-updx-empty', 'No update information yet.'));
      return;
    }
    const latest = normVer(latestInfo.version);

    const addSection = (version, bullets) => {
      const section = el('section', 'riff-updx-release');
      section.appendChild(el('h3', 'riff-updx-release-title', APP_NAME + ' v' + version));
      const ul = el('ul', 'riff-updx-bullets');
      bullets.forEach((b) => ul.appendChild(el('li', null, b)));
      section.appendChild(ul);
      box.appendChild(section);
    };

    // Latest: bundled map first, then feed notes, then an honest generic line.
    const latestBullets = CHANGELOG[latest]
      || (latestInfo.releaseNotes ? notesToBullets(latestInfo.releaseNotes) : [])
      || [];
    addSection(latest, latestBullets.length ? latestBullets : ['Bug fixes and performance improvements.']);
  }

  // ------------------------------------------------------------- settings tab

  function makeToggle(labelText, descText, checked, onChange) {
    const row = el('label', 'riff-updx-toggle-row');
    const textWrap = el('span', 'riff-updx-toggle-text');
    textWrap.appendChild(el('span', 'riff-updx-toggle-label', labelText));
    textWrap.appendChild(el('span', 'riff-updx-toggle-desc', descText));
    const input = el('input', 'riff-updx-switch');
    input.type = 'checkbox';
    input.checked = checked;
    const track = el('span', 'riff-updx-switch-track');
    track.appendChild(el('span', 'riff-updx-switch-thumb'));
    input.addEventListener('change', () => onChange(input.checked));
    row.appendChild(textWrap);
    row.appendChild(input);
    row.appendChild(track);
    return row;
  }

  function renderSettings() {
    const box = ui.settings;
    box.textContent = '';
    box.appendChild(makeToggle(
      'Check automatically',
      'Look for a new release on launch and about every 6 hours.',
      settings.autoCheck,
      (on) => {
        settings.autoCheck = on;
        saveSettings();
        scheduleAutoCheck();
      }
    ));
    box.appendChild(el('p', 'riff-updx-settings-note',
      'Skipped versions stay hidden until a newer release appears. Preferences are stored on this device.'));
  }

  // ------------------------------------------------------------- history tab

  function renderHistory() {
    const box = ui.history;
    box.textContent = '';
    const history = loadHistory();
    if (!history.length) {
      box.appendChild(el('p', 'riff-updx-empty', 'No updates installed yet. Installed updates will show up here.'));
      return;
    }
    const ul = el('ul', 'riff-updx-history-list');
    history.forEach((entry) => {
      const li = el('li', 'riff-updx-history-item');
      li.appendChild(icon('history'));
      const meta = el('span', 'riff-updx-history-meta');
      const version = normVer(entry && entry.version);
      meta.appendChild(el('span', 'riff-updx-history-version', 'Updated to v' + (version || '?')));
      let sub = '';
      if (entry && entry.from) sub = 'from v' + normVer(entry.from);
      if (entry && entry.at) {
        const d = new Date(entry.at);
        if (!isNaN(d.getTime())) sub += (sub ? ' · ' : '') + d.toLocaleDateString() + ' ' + d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
      }
      if (sub) meta.appendChild(el('span', 'riff-updx-history-sub', sub));
      li.appendChild(meta);
      ul.appendChild(li);
    });
    box.appendChild(ul);
  }

  // ------------------------------------------------------------- modal build

  function buildUI() {
    const root = el('div', 'riff-updx-backdrop');
    root.id = 'riff-updx-root';

    const card = el('div', 'riff-updx-card');
    card.setAttribute('role', 'dialog');
    card.setAttribute('aria-modal', 'true');
    card.setAttribute('aria-labelledby', 'riff-updx-title');
    card.tabIndex = -1;

    // --- left sidebar ---
    const side = el('aside', 'riff-updx-side');
    const brand = el('div', 'riff-updx-brand');
    const logoWrap = el('div', 'riff-updx-logo');
    const logo = el('img', 'riff-updx-logo-img');
    logo.src = '../../assets/riff-emblem.png';
    logo.alt = '';
    logo.draggable = false;
    logo.addEventListener('error', () => { logo.style.display = 'none'; });
    logoWrap.appendChild(el('span', 'riff-updx-logo-fallback', 'R'));
    logoWrap.appendChild(logo);
    brand.appendChild(logoWrap);
    brand.appendChild(el('span', 'riff-updx-brand-name', APP_NAME));
    side.appendChild(brand);

    const nav = el('nav', 'riff-updx-nav');
    nav.setAttribute('aria-label', 'Updater sections');
    const tabs = [
      { id: 'changelog', label: 'Changelog', icon: 'new_releases' },
      { id: 'settings', label: 'Update Settings', icon: 'settings' },
      { id: 'history', label: 'Update History', icon: 'history' }
    ];
    const navButtons = {};
    tabs.forEach((tab) => {
      const btn = el('button', 'riff-updx-nav-btn');
      btn.type = 'button';
      btn.dataset.tab = tab.id;
      btn.appendChild(icon(tab.icon));
      btn.appendChild(el('span', null, tab.label));
      btn.addEventListener('click', () => selectTab(tab.id));
      nav.appendChild(btn);
      navButtons[tab.id] = btn;
    });
    side.appendChild(nav);
    const sideVersion = el('div', 'riff-updx-side-version');
    side.appendChild(sideVersion);

    // --- main panel ---
    const main = el('div', 'riff-updx-main');

    const header = el('header', 'riff-updx-header');
    const refreshBtn = el('button', 'riff-updx-refresh');
    refreshBtn.type = 'button';
    refreshBtn.title = 'Check again';
    refreshBtn.setAttribute('aria-label', 'Check for updates again');
    refreshBtn.appendChild(icon('refresh'));
    refreshBtn.addEventListener('click', () => checkForUpdate(true));
    const headText = el('div', 'riff-updx-head-text');
    const title = el('h2', 'riff-updx-title', 'New update available!');
    title.id = 'riff-updx-title';
    const subtitle = el('p', 'riff-updx-subtitle', 'There is a new version of ' + APP_NAME + ' available.');
    headText.appendChild(title);
    headText.appendChild(subtitle);
    const closeBtn = el('button', 'riff-updx-close');
    closeBtn.type = 'button';
    closeBtn.setAttribute('aria-label', 'Close');
    closeBtn.appendChild(icon('close'));
    closeBtn.addEventListener('click', close);
    header.appendChild(refreshBtn);
    header.appendChild(headText);
    header.appendChild(closeBtn);
    main.appendChild(header);

    const content = el('div', 'riff-updx-content');
    const changelog = el('div', 'riff-updx-pane riff-updx-scroll');
    const settingsPane = el('div', 'riff-updx-pane riff-updx-scroll');
    const historyPane = el('div', 'riff-updx-pane riff-updx-scroll');
    settingsPane.hidden = true;
    historyPane.hidden = true;
    content.appendChild(changelog);
    content.appendChild(settingsPane);
    content.appendChild(historyPane);
    main.appendChild(content);

    // --- footer ---
    const footer = el('footer', 'riff-updx-footer');
    const versions = el('div', 'riff-updx-versions');
    const curLabel = el('span', 'riff-updx-ver-current');
    const sep = el('span', 'riff-updx-ver-sep', '|');
    const latLabel = el('span', 'riff-updx-ver-latest');
    versions.appendChild(curLabel);
    versions.appendChild(sep);
    versions.appendChild(latLabel);
    const actions = el('div', 'riff-updx-actions');
    const skipBtn = el('button', 'riff-updx-btn riff-updx-btn-text', 'Skip version');
    skipBtn.type = 'button';
    const cancelBtn = el('button', 'riff-updx-btn riff-updx-btn-tonal', 'Cancel');
    cancelBtn.type = 'button';
    const updateBtn = el('button', 'riff-updx-btn riff-updx-btn-filled', 'Update Now');
    updateBtn.type = 'button';
    skipBtn.addEventListener('click', skipVersion);
    cancelBtn.addEventListener('click', close);
    updateBtn.addEventListener('click', updateNow);
    actions.appendChild(skipBtn);
    actions.appendChild(cancelBtn);
    actions.appendChild(updateBtn);
    footer.appendChild(versions);
    footer.appendChild(actions);
    main.appendChild(footer);

    card.appendChild(side);
    card.appendChild(main);
    root.appendChild(card);

    // Backdrop click closes (but not clicks inside the card).
    root.addEventListener('mousedown', (e) => { if (e.target === root) close(); });

    ui = {
      root, card, navButtons, sideVersion, refreshBtn, subtitle,
      changelog, settings: settingsPane, history: historyPane,
      curLabel, latLabel, updateBtn, skipBtn, cancelBtn, closeBtn
    };
    document.body.appendChild(root);
  }

  function selectTab(id) {
    if (!ui) return;
    Object.keys(ui.navButtons).forEach((key) => {
      const active = key === id;
      ui.navButtons[key].classList.toggle('riff-updx-active', active);
      ui.navButtons[key].setAttribute('aria-current', active ? 'page' : 'false');
    });
    ui.changelog.hidden = id !== 'changelog';
    ui.settings.hidden = id !== 'settings';
    ui.history.hidden = id !== 'history';
    if (id === 'settings') renderSettings();
    if (id === 'history') renderHistory();
    if (id === 'changelog') renderChangelog();
  }

  // ------------------------------------------------------------- open / close

  function open() {
    if (!latestInfo) return;
    if (!ui) buildUI();
    ui.curLabel.textContent = 'Current Version: v' + normVer(currentVersion);
    ui.latLabel.textContent = 'Latest Version: v' + normVer(latestInfo.version);
    ui.sideVersion.textContent = 'v' + normVer(currentVersion);
    ui.updateBtn.disabled = false;
    ui.updateBtn.textContent = 'Update Now';
    installing = false;
    if (statusTimer) { clearTimeout(statusTimer); statusTimer = null; }
    ui.subtitle.textContent = 'There is a new version of ' + APP_NAME + ' available.';
    selectTab('changelog');
    lastFocused = document.activeElement;
    ui.root.classList.add('riff-updx-open');
    isOpen = true;
    ui.card.focus();
  }

  function close() {
    if (!ui || !isOpen) return;
    isOpen = false;
    ui.root.classList.remove('riff-updx-open');
    if (lastFocused && typeof lastFocused.focus === 'function') {
      try { lastFocused.focus(); } catch (_) {}
    }
  }

  function flashStatus(message) {
    if (!ui) return;
    ui.subtitle.textContent = message;
    if (statusTimer) clearTimeout(statusTimer);
    statusTimer = setTimeout(() => {
      if (ui) ui.subtitle.textContent = 'There is a new version of ' + APP_NAME + ' available.';
    }, 4000);
  }

  document.addEventListener('keydown', (e) => {
    if (isOpen && (e.key === 'Escape' || e.key === 'Esc')) {
      e.stopPropagation();
      close();
    }
  });

  // ------------------------------------------------------------- actions

  function skipVersion() {
    if (!latestInfo) return;
    lsSet(KEY_SKIPPED, normVer(latestInfo.version));
    try { if (typeof api.updateAction === 'function') api.updateAction('ignore'); } catch (_) {}
    close();
  }

  function updateNow() {
    if (!latestInfo || installing) return;
    installing = true;
    recordInstall(normVer(latestInfo.version));
    ui.updateBtn.disabled = true;
    ui.updateBtn.textContent = 'Installing\u2026';

    // Existing install flow, same calls the built-in sheet makes.
    try {
      if (typeof api.installUpdateNow === 'function') {
        Promise.resolve(api.installUpdateNow()).catch(() => {});
      } else if (typeof api.updateAction === 'function') {
        api.updateAction('now');
      }
    } catch (_) {}

    // In the Rust port install_update_now is a stub; installUpdateQuiet maps
    // to install_update_if_newer, the real download + silent install + restart.
    if (typeof api.installUpdateQuiet === 'function') {
      Promise.resolve()
        .then(() => api.installUpdateQuiet())
        .then((result) => {
          if (typeof result === 'string' && /up to date/i.test(result)) {
            installing = false;
            ui.updateBtn.disabled = false;
            ui.updateBtn.textContent = 'Update Now';
            flashStatus('You are already on the latest version.');
          }
          // Otherwise the app quits and relaunches into the new build.
        })
        .catch(() => {
          installing = false;
          if (ui) {
            ui.updateBtn.disabled = false;
            ui.updateBtn.textContent = 'Update Now';
            flashStatus('Update failed. You can retry or download it from GitHub.');
          }
        });
    }
  }

  // ------------------------------------------------------------- check flow

  function extractRelease(r) {
    if (!r || typeof r !== 'object') return null;
    const version = normVer(r.version || r.latestVersion || r.latest || r.tag_name || '');
    if (!version) return null;
    if (r.available === false) return null;
    return {
      version,
      releaseNotes: r.releaseNotes || r.body || '',
      publishedAt: r.publishedAt || r.published_at || '',
      htmlUrl: r.htmlUrl || r.html_url || RELEASE_PAGE
    };
  }

  async function fetchReleaseFromGitHub() {
    if (typeof navigator !== 'undefined' && navigator.onLine === false) return null;
    const res = await fetch(RELEASE_API, { headers: { 'User-Agent': 'Riff-Player' } });
    if (!res.ok) return null;
    const json = await res.json();
    const version = normVer(json && json.tag_name);
    if (!version) return null;
    return {
      version,
      releaseNotes: (json && json.body) || '',
      publishedAt: (json && json.published_at) || '',
      htmlUrl: (json && json.html_url) || RELEASE_PAGE
    };
  }

  async function checkForUpdate(manual) {
    if (checking) return;
    checking = true;
    if (ui) ui.refreshBtn.classList.add('riff-updx-spinning');
    try {
      let release = null;
      try {
        release = extractRelease(await api.checkForUpdates());
      } catch (_) {}
      // The port's check_for_updates is a stub ({ available: false }); use the
      // same direct GitHub fallback the built-in updater uses.
      if (!release) {
        try { release = await fetchReleaseFromGitHub(); } catch (_) {}
      }

      if (release && isNewer(release.version, currentVersion) && !isSkipped(release.version)) {
        latestInfo = release;
        open();
      } else if (manual && ui) {
        flashStatus('You are using the latest version of ' + APP_NAME + '.');
      }
    } finally {
      checking = false;
      if (ui) ui.refreshBtn.classList.remove('riff-updx-spinning');
    }
  }

  function scheduleAutoCheck() {
    if (autoTimer) { clearInterval(autoTimer); autoTimer = null; }
    if (settings.autoCheck) {
      autoTimer = setInterval(() => { checkForUpdate(false); }, CHECK_INTERVAL_MS);
    }
  }

  // ------------------------------------------------------------- boot

  function boot() {
    Promise.resolve()
      .then(() => api.getAppVersion())
      .then((v) => { currentVersion = normVer(v) || '0.0.0'; })
      .catch(() => { currentVersion = '0.0.0'; })
      .then(() => {
        setTimeout(() => { checkForUpdate(false); }, INITIAL_DELAY_MS);
        scheduleAutoCheck();
      });
  }

  // Small manual hook so the modal can be inspected without waiting for a
  // scheduled check: window.riffUpdater.check() / .open() / .close().
  window.riffUpdater = {
    check: () => checkForUpdate(true),
    open: () => { if (latestInfo) open(); },
    close
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', boot, { once: true });
  } else {
    boot();
  }
})();
