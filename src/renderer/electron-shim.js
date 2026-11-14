// Riff Rust bridge: replaces Electron's preload (window.electronAPI) with
// Tauri IPC + the embedded localhost API. Loaded before app.js.
// Boot payload (token, port, storage snapshot) is injected by the Rust side
// via initialization_script at page load.
(() => {
  const boot = window.__RIFF_BOOT__ || { token: '', port: 0, storage: {} };
  const tauri = window.__TAURI__ || {};
  const invoke = tauri.core && typeof tauri.core.invoke === 'function'
    ? tauri.core.invoke.bind(tauri.core)
    : () => Promise.resolve(null);
  const eventApi = tauri.event || {};

  const ua = navigator.userAgent || '';
  const isWin = ua.includes('Windows');
  const isMac = /Mac|iPhone|iPad|iPod/i.test(ua);
  const isLinux = !isWin && !isMac;
  const platform = isWin ? 'win32' : (isMac ? 'darwin' : 'linux');

  // The dialog plugin only ships a confirm/alert polyfill; call its commands
  // directly through the Tauri internals (as the npm package would).
  const internals = window.__TAURI_INTERNALS__ || {};
  const rawInvoke = typeof internals.invoke === 'function' ? internals.invoke.bind(internals) : invoke;

  // Registered listeners for on* APIs (Riff responds to app-driven events;
  // most events never fire in v0.1 — see PORTING.md).
  const listeners = new Map();
  function onEvent(name, callback) {
    listeners.set(name, (listeners.get(name) || []).concat(callback));
    if (eventApi.listen) {
      eventApi.listen(name, (e) => {
        const payload = e && 'payload' in e ? e.payload : e;
        (listeners.get(name) || []).forEach((cb) => {
          try { cb(payload); } catch (_) {}
        });
      });
    }
  }

  function fireEvent(name, payload) {
    (listeners.get(name) || []).forEach((cb) => {
      try { cb(payload); } catch (_) {}
    });
  }

  async function pickFiles({ multiple, directory, filters, title }) {
    try {
      const args = { multiple: !!multiple, directory: !!directory };
      if (filters) args.filters = filters;
      if (title) args.title = title;
      const result = await rawInvoke('plugin:dialog|open', args);
      if (!result) return null;
      // Desktop returns a bare path string (single pick) or a string array (multi)
      return Array.isArray(result) ? result : [result];
    } catch (e) {
      console.warn('[riff-rust] dialog failed:', e);
      return null;
    }
  }

  window.electronAPI = {
    // --- boot ---
    serverToken: boot.token || '',
    isWindows: isWin,
    platform,
    getServerPort: () => Promise.resolve(boot.port || 0),

    // --- window chrome ---
    minimizeWindow: () => invoke('window_minimize'),
    maximizeWindow: () => invoke('window_maximize'),
    closeWindow: () => invoke('window_close'),
    setWindowButtonsVisible: (visible) => invoke('set_window_buttons_visible', { visible }),
    setZoomFactor: (factor) => invoke('set_zoom_factor', { factor }),
    getZoomFactor: () => Promise.resolve(1.0),
    clearWebFrameCache: () => Promise.resolve(true),

    // --- gpu / proxy ---
    getGpuCompositing: () => invoke('get_gpu_compositing'),
    setAppProxy: (proxyUrl) => invoke('set_app_proxy', { proxyUrl }),

    // --- file dialogs (driven by the dialog plugin's JS API) ---
    pickMedia: async () => {
      const files = await pickFiles({ filters: [{ name: 'Media', extensions: ['png', 'jpg', 'jpeg', 'webp', 'gif', 'mp4', 'webm'] }] });
      return files ? 'file://' + encodeURI(files[0]).replace(/\\/g, '/') : null;
    },
    importAudioFiles: async () => {
      const files = await pickFiles({ multiple: true, filters: [{ name: 'Audio', extensions: ['mp3', 'wav', 'flac', 'opus', 'ogg', 'oga', 'm4a', 'aac', 'webm', 'weba'] }] });
      if (!files || files.length === 0) return [];
      return invoke('import_audio_files', { paths: files });
    },
    pickLocalFiles: async () => {
      const files = await pickFiles({ multiple: true, filters: [{ name: 'Audio', extensions: ['mp3', 'wav', 'flac', 'opus', 'ogg', 'oga', 'm4a', 'aac', 'webm', 'weba'] }] });
      return files || [];
    },
    pickLocalFolder: async () => {
      const files = await pickFiles({ directory: true, multiple: false });
      return files ? files[0] : null;
    },
    rescanLocalMetadata: (paths) => invoke('rescan_local_metadata', { paths: paths || null }),
    pickDataDir: async () => {
      const files = await pickFiles({ directory: true });
      return files ? files[0] : null;
    },
    pickDownloadDir: async () => {
      const files = await pickFiles({ directory: true });
      return files ? files[0] : null;
    },

    // --- visibility / lifecycle ---
    onWindowVisibility: (callback) => onEvent('window-visibility', callback),
    onMediaCommand: (callback) => onEvent('media-command', callback),
    onMediaControl: (callback) => onEvent('media-control', callback),
    mediaKeysActive: () => invoke('media_keys_active'),
    setMediaKeys: (v) => invoke('set_media_keys', { v }),

    // --- mini player ---
    toggleMiniWindow: () => invoke('toggle_mini_window'),
    setMiniLyrics: (enabled) => invoke('set_mini_lyrics', { enabled }),

    // --- discord rich presence ---
    discordRpcUpdate: (trackInfo) => invoke('discord_rpc_update', { trackInfo }),
    discordRpcClear: () => invoke('discord_rpc_clear'),
    discordRpcSetEnabled: (enabled) => invoke('discord_rpc_set_enabled', { enabled }),
    discordRpcGetEnabled: () => invoke('discord_rpc_get_enabled'),
    discordRpcGetConnected: () => invoke('discord_rpc_get_connected'),
    discordRpcReconnect: () => invoke('discord_rpc_reconnect'),

    // --- cache ---
    getCacheSize: () => invoke('get_cache_size'),
    clearCache: () => invoke('clear_cache'),

    // --- data dir ---
    getDataDir: () => invoke('get_data_dir'),
    setDataDir: (dir, move) => invoke('set_data_dir', { dir, moveFiles: !!move }),
    getDownloadDir: () => invoke('get_download_dir'),
    setDownloadDir: (dir) => invoke('set_download_dir', { dir }),
    resetDownloadDir: () => invoke('set_download_dir', { reset: true }),

    // --- app ---
    restartApp: () => invoke('restart_app'),
    getAppVersion: () => invoke('get_app_version'),
    checkForUpdates: () => invoke('check_for_updates'),
    updateAction: (action) => invoke('update_action', { action }),
    installUpdateNow: () => { invoke('check_for_updates'); return invoke('install_update_now'); },
    onUpdateAvailable: (cb) => onEvent('update-available', cb),
    onUpdateDownloaded: (cb) => onEvent('update-downloaded', cb),
    onUpdateProgress: (cb) => onEvent('update-progress', cb),
    onUpdateReady: (cb) => onEvent('update-ready', cb),
    onUpdateError: (cb) => onEvent('update-error', cb),
    onUpdateNotAvailable: (cb) => onEvent('update-not-available', cb),
    setTrayOnClose: (enabled) => invoke('set_tray_on_close', { enabled }),
    getMultiInstance: () => invoke('get_multi_instance'),
    setMultiInstance: (v) => invoke('set_multi_instance', { v }),
    getGlobalShortcuts: () => invoke('get_global_shortcuts'),
    setGlobalShortcuts: (v) => invoke('set_global_shortcuts', { v }),

    // --- launcher ---
    getLauncherInfo: () => invoke('get_launcher_info'),
    installLauncher: () => invoke('install_launcher'),

    // --- welcome ---
    welcomeFinish: (detail) => invoke('welcome_finish', { detail }),
    onWelcomeResult: (cb) => onEvent('welcome-result', cb),

    // --- themes ---
    listThemes: () => invoke('list_themes'),
    importThemes: async () => {
      const files = await pickFiles({ multiple: true, filters: [{ name: 'Themes', extensions: ['css'] }] });
      if (!files || files.length === 0) return [];
      return invoke('import_themes', { paths: files });
    },
    setThemeEnabled: (id, on) => invoke('set_theme_enabled', { id, on }),
    disableAllThemes: () => invoke('disable_all_themes'),
    removeTheme: (id) => invoke('remove_theme', { id }),

    // --- plugins ---
    listPlugins: () => invoke('list_plugins'),
    importPlugins: async () => {
      const files = await pickFiles({ multiple: true, filters: [{ name: 'Plugins', extensions: ['js'] }] });
      if (!files || files.length === 0) return 0;
      return invoke('import_plugins', { paths: files });
    },
    setPluginEnabled: (file, on) => invoke('set_plugin_enabled', { file, on }),
    disableAllPlugins: () => invoke('disable_all_plugins'),
    removePlugin: (file) => invoke('remove_plugin', { file }),
    openPluginsFolder: () => invoke('open_plugins_folder'),

    // --- css editor ---
    getAppCss: () => invoke('get_app_css'),

    // --- misc ---
    openExternal: (u) => invoke('open_external', { u }),
    logError: (level, message, stack) => invoke('log_error', { level, message, stack: stack || null }),
    openLogsFolder: () => invoke('open_logs_folder'),
    getDiagnostics: () => invoke('get_diagnostics'),
    getAudioOutputInfo: () => invoke('get_audio_output_info'),

    // --- google / youtube account ---
    googleLogin: () => invoke('google_login'),
    googleLogout: () => invoke('google_logout'),
    googleAuthStatus: () => invoke('google_auth_status'),
    googleAccountInfo: () => invoke('google_account_info'),
    googleImportLibrary: (opts) => invoke('google_import_library', { opts: opts || {} }),
    youtubePlaybackLogin: () => invoke('youtube_playback_login'),
    youtubePlaybackLogout: () => invoke('youtube_playback_logout'),
    youtubePlaybackStatus: () => invoke('youtube_playback_status'),
    onYoutubePlaybackAuth: (callback) => onEvent('youtube-playback-auth', callback),

    // --- playlists / account ---
    savePlaylistsJson: (payload) => invoke('save_playlists_json', { payload }),
    showPlaylistContextMenu: (playlistId, options) => invoke('show_playlist_context_menu', { playlistId, options }),
    accountExport: (payload) => invoke('account_export', { payload }),
    accountImport: () => invoke('account_import'),

    // --- storage (synchronous, served from the boot snapshot) ---
    storageInitSync: () => boot.storage || {},
    storageSaveSync: (data) => { invoke('storage_save_batch', { data }); },
    storageSaveBatch: (data) => { invoke('storage_save_batch', { data }); },

    // --- web cache ---
    readCacheFile: () => invoke('read_cache_file'),
    writeCacheFile: (data) => invoke('write_cache_file', { data }),
  };
})();
