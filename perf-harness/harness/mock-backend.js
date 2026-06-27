// Injected before the app bundle: a fake Tauri IPC backend so the real built
// frontend runs in a real WebKit browser (MiniBrowser via WebKitWebDriver) at
// scale. Holds an in-memory vault of `?n=` files. Kept fast (microtask) so this
// measures FRONTEND rendering cost, not backend latency.
(function () {
  var qs = new URLSearchParams(location.search);
  var N = parseInt(qs.get('n') || '1000', 10);
  var BIG = parseInt(qs.get('big') || '0', 10); // lines in README, to test large-file editing
  var NEST = qs.get('nest') === '1'; // put all notes under one dir, to test expanding a big folder
  var HIST = parseInt(qs.get('hist') || '0', 10); // synthetic history events, to test the track at scale
  function bigBody(prefix) {
    if (!BIG) return '# ' + prefix + ' vault\n\nSeeded for perf testing.\n';
    var out = '# ' + prefix + ' (large file, ' + BIG + ' lines)\n\n';
    for (var j = 0; j < BIG; j++) out += '- line ' + j + ' with some **bold** and `code` and [a link](http://x/' + j + ')\n';
    return out;
  }
  function mk(prefix) {
    var c = { 'README.md': bigBody(prefix) };
    var dir = NEST ? 'allnotes/' : '';
    for (var i = 0; i < N; i++) c[dir + 'note-' + String(i).padStart(5, '0') + '.md'] = '# ' + prefix + ' note ' + i + '\n\n- one\n- two\n\nBody **text** ' + i + '.\n';
    return c;
  }
  function histEvents() {
    var nowSec = Math.floor(Date.now() / 1000);
    if (!HIST) return [{ id: 'r', ts: nowSec - 3600, lamport: 1, kind: 'create', path: 'README.md' }];
    // Recent events spread over the last ~HIST minutes so they fall in the view.
    var ev = [];
    var kinds = ['create', 'edit', 'rename', 'delete'];
    for (var i = 0; i < HIST; i++) ev.push({ id: 'r' + i, ts: nowSec - (HIST - i) * 60, lamport: i, kind: kinds[i % 4], path: 'note-' + String(i % N).padStart(5, '0') + '.md' });
    return ev;
  }
  // Two vaults so we can exercise switching (the "opening another vault froze" path).
  var VAULTS = {
    v1: { info: { id: 'v1', path: '/home/me/massive', vault_id: 'vid1', enabled: false, listening_ticket: null }, content: mk('Massive') },
    v2: { info: { id: 'v2', path: '/home/me/second', vault_id: 'vid2', enabled: false, listening_ticket: null }, content: mk('Second') },
  };
  function files(id) {
    return Object.keys(VAULTS[id].content).map(function (p) {
      return { path: p, file_id: p, is_dir: false, merge_class: 'text' };
    });
  }
  var H = {
    list_vaults: function () { return [VAULTS.v1.info, VAULTS.v2.info]; },
    get_identity: function () { return 'ssh-ed25519 AAAAExampleKeyMaterial me@host'; },
    get_status: function (a) { return { id: a.id, vault_id: VAULTS[a.id].info.vault_id, rows: N, files: N, head: 'h', listening_ticket: null, peers: [], last_ts: 1700000000 }; },
    list_files: function (a) { return files(a.id); },
    read_file: function (a) { var c = VAULTS[a.id].content; return c[a.path] != null ? c[a.path] : ''; },
    write_file: function (a) { VAULTS[a.id].content[a.path] = a.content; return null; },
    delete_file: function (a) { delete VAULTS[a.id].content[a.path]; return null; },
    rename_file: function (a) { var c = VAULTS[a.id].content; c[a['new']] = c[a.old]; delete c[a.old]; return null; },
    history: function () { return histEvents(); },
    read_file_at: function (a) { var c = VAULTS[a.id].content; return { exists: true, content: c[a.path] != null ? c[a.path] : '' }; },
    restore_file_at: function () { return null; },
    rescan: function () { return null; },
    remove_vault: function () { return null; },
    add_local_folder: function (a) { return VAULTS.v1.info; },
    clone_remote: function (a) { return VAULTS.v1.info; },
    set_allow_connections: function () { return 'tkt'; },
    sync_now: function () { return null; },
    'plugin:dialog|open': function () { return '/home/me/massive'; },
  };
  // Mutations are slowed to mimic the real O(N) materialize, so the harness can
  // catch UI races (e.g. delete read-modify-write) that an instant mock hides.
  var SLOW = { write_file: 60, delete_file: 60, rename_file: 60 };
  window.__TAURI_INTERNALS__ = {
    invoke: function (cmd, args) {
      return new Promise(function (resolve) {
        var h = H[cmd];
        setTimeout(function () { resolve(h ? h(args || {}) : null); }, SLOW[cmd] || 0);
      });
    },
    transformCallback: function (cb) {
      var id = Math.floor(Math.random() * 1e9);
      window['_cb_' + id] = cb;
      return id;
    },
    metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main' } },
  };
})();
