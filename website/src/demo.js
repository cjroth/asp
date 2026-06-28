import './main.js'   // nav toggle (CSS is linked in <head> to avoid FOUC)

// ----- status / log color maps (from the original component) -----
const META = {
  insync:  { c: '#2aa869', l: 'In sync' },
  syncing: { c: '#18b6c9', l: 'Syncing' },
  offline: { c: '#e5484d', l: 'Offline' },
  catchup: { c: '#f5a623', l: 'Catch-up' },
}
const LDOT = { commit: '#6b4ef0', recv: '#18b6c9', sync: '#2aa869', clone: '#f5a623', offline: '#e5484d' }

const rid = () => Math.random().toString(16).slice(2, 6)

function seedFiles() {
  return [
    { name: 'plan.md',  body: '# Q3 plan\n\nShip ASP v1 across every device.\n\n- [x] deterministic fold\n- [ ] write the docs\n- [ ] launch the demo' },
    { name: 'notes.md', body: '# Notes\n\nSync is automatic — no commit, no push.\nEdits converge in ~1s, peer to peer.' },
    { name: 'graph.md', body: '# Knowledge graph\n\nEntities and links build themselves\nfrom the vault for fast recall.' },
  ]
}

function initialState() {
  return {
    nodes: [
      { id: 'n1', name: 'MacBook Pro', role: 'laptop', status: 'insync', rows: 48, file: 0, files: seedFiles(), log: [
        { k: 'commit', t: 'commit plan.md · +3 −0 · #a1f3' },
        { k: 'recv',   t: 'integrate ← iPhone · 2 rows' },
        { k: 'sync',   t: 'in sync · 48 rows · 3 nodes' },
      ] },
      { id: 'n2', name: 'iPhone', role: 'phone', status: 'insync', rows: 48, file: 0, files: seedFiles(), log: [
        { k: 'recv', t: 'integrate ← MacBook Pro · 3 rows' },
        { k: 'sync', t: 'in sync · 48 rows · 3 nodes' },
      ] },
      { id: 'n3', name: 'claude-agent', role: 'agent', status: 'syncing', rows: 46, file: 0, files: seedFiles(), log: [
        { k: 'clone', t: 'clone ← MacBook Pro · handshake ok' },
        { k: 'recv',  t: 'integrate ← MacBook Pro · 6 rows' },
      ] },
    ],
    sel: 'n1',
    counter: 4,
  }
}

// ----- component -----
class SyncDemo {
  constructor(root) {
    this.root = root
    this.state = initialState()
    this._t = this._t2 = this._t3 = null
    this._docKey = null       // tracks which file is in the textarea, to avoid clobbering the caret
    this.buildShell()
    this.render()
  }

  setState(updater) {
    const patch = typeof updater === 'function' ? updater(this.state) : updater
    this.state = { ...this.state, ...patch }
    this.render()
  }

  selected() {
    return this.state.nodes.find((n) => n.id === this.state.sel) || this.state.nodes[0]
  }

  // ----- actions (ported 1:1 from the design component) -----
  selectNode(id) { this.setState({ sel: id }) }

  selectFile(i) {
    this.setState((s) => ({ nodes: s.nodes.map((n) => (n.id === s.sel ? { ...n, file: i } : n)) }))
  }

  edit(text) {
    this.setState((s) => ({
      nodes: s.nodes.map((n) => {
        if (n.id === s.sel)
          return { ...n, status: n.status === 'offline' ? 'offline' : 'syncing', files: n.files.map((f, i) => (i === n.file ? { ...f, body: text } : f)) }
        return n.status === 'offline' ? n : { ...n, status: 'syncing' }
      }),
    }))
    clearTimeout(this._t)
    this._t = setTimeout(() => this.commit(), 850)
  }

  commit() {
    this.setState((s) => {
      const act = s.nodes.find((n) => n.id === s.sel)
      if (!act) return {}
      const fname = act.files[act.file].name
      const id = rid()
      const nodes = s.nodes.map((n) => {
        if (n.status === 'offline') return n
        if (n.id === s.sel)
          return { ...n, status: 'insync', rows: n.rows + 1, log: [{ k: 'commit', t: `commit ${fname} · +1 −0 · #${id}` }, ...n.log].slice(0, 7) }
        return { ...n, status: 'insync', rows: n.rows + 1, log: [{ k: 'recv', t: `integrate ← ${act.name} · 1 row` }, ...n.log].slice(0, 7) }
      })
      return { nodes }
    })
  }

  toggleOffline() {
    const cur = this.selected()
    const reconnecting = cur && cur.status === 'offline'
    this.setState((s) => ({
      nodes: s.nodes.map((n) => {
        if (n.id !== s.sel) return n
        if (n.status === 'offline') return { ...n, status: 'catchup', log: [{ k: 'recv', t: 'anti-entropy · catching up…' }, ...n.log].slice(0, 7) }
        return { ...n, status: 'offline', log: [{ k: 'offline', t: 'went offline · edits will queue' }, ...n.log].slice(0, 7) }
      }),
    }))
    if (reconnecting) {
      clearTimeout(this._t3)
      this._t3 = setTimeout(() => {
        this.setState((s) => ({
          nodes: s.nodes.map((n) => (n.id === s.sel ? { ...n, status: 'insync', rows: n.rows + 4, log: [{ k: 'sync', t: 'caught up · +4 rows · in sync' }, ...n.log].slice(0, 7) } : n)),
        }))
      }, 1300)
    }
  }

  addNode() {
    this.setState((s) => {
      const base = s.nodes.find((n) => n.id === s.sel) || s.nodes[0]
      const id = 'n' + s.counter
      const pool = [['Linux box', 'device'], ['iPad', 'tablet'], ['work laptop', 'laptop'], ['vault-hub', 'hub'], ['codex-agent', 'agent']]
      const pair = pool[(s.counter - 4) % pool.length]
      const node = { id, name: pair[0], role: pair[1], status: 'syncing', rows: Math.max(0, base.rows - 6), file: 0, files: base.files.map((f) => ({ ...f })), log: [{ k: 'clone', t: `clone ← ${base.name} · handshake ok` }] }
      return { nodes: [...s.nodes, node], counter: s.counter + 1, sel: id }
    })
    clearTimeout(this._t2)
    this._t2 = setTimeout(() => {
      this.setState((s) => {
        const peer = s.nodes.find((x) => x.id !== s.sel)
        const target = peer ? peer.rows : 0
        return { nodes: s.nodes.map((n) => (n.id === s.sel ? { ...n, status: 'insync', rows: target, log: [{ k: 'sync', t: `caught up · in sync · ${s.nodes.length} nodes` }, ...n.log].slice(0, 7) } : n)) }
      })
    }, 1300)
  }

  reset() {
    clearTimeout(this._t); clearTimeout(this._t2); clearTimeout(this._t3)
    this._docKey = null
    this.setState(initialState())
  }

  // ----- static shell built once; lists re-rendered, textarea preserved -----
  buildShell() {
    this.root.className = 'sd-page'
    this.root.innerHTML = `
      <div class="sd-app">
        <div class="sd-top">
          <span class="sd-mark"><span class="sd-tick"></span>ASP</span>
          <span class="sd-subt">Agent Sync Protocol · p2p sync demo</span>
          <div class="f1"></div>
          <span class="sd-stat">nodes<b data-node-count></b></span>
          <span class="sd-stat">log rows<b data-total-rows></b></span>
          <button class="sd-btn2" data-reset>Reset</button>
          <button class="sd-btn" data-add>+ Add node</button>
        </div>
        <div class="sd-map">
          <div class="sd-mesh" data-mesh></div>
          <div class="sd-legend">
            <span><span class="ld" style="background:#2aa869"></span>in sync</span>
            <span><span class="ld" style="background:#18b6c9"></span>syncing</span>
            <span><span class="ld" style="background:#e5484d"></span>offline</span>
            <span><span class="ld" style="background:#f5a623"></span>catch-up</span>
          </div>
        </div>
        <div class="sd-main"><div class="sd-focus">
          <div class="np-head">
            <span class="fw7" style="font-size:19px" data-sel-name></span>
            <span class="np-badge" data-sel-role></span>
            <div class="f1"></div>
            <span class="spill"><span class="sdot" data-status-dot></span><span data-status-label></span></span>
            <button class="np-act" data-offline></button>
          </div>
          <div class="np-body">
            <div class="np-tree">
              <div class="np-treelab">Vault</div>
              <div data-tree></div>
              <div class="ftadd">+ new file</div>
            </div>
            <div class="np-ed">
              <div class="ed-tab"><span class="fdot2"></span><span data-cur-name></span><span class="ed-mut">· editing</span></div>
              <textarea class="ed-area" spellcheck="false" data-editor></textarea>
            </div>
          </div>
          <div class="np-log">
            <div class="np-treelab" style="padding:0 0 9px">Event log</div>
            <div data-log></div>
          </div>
        </div></div>
      </div>`

    const $ = (sel) => this.root.querySelector(sel)
    this.els = {
      nodeCount: $('[data-node-count]'), totalRows: $('[data-total-rows]'),
      mesh: $('[data-mesh]'), selName: $('[data-sel-name]'), selRole: $('[data-sel-role]'),
      statusDot: $('[data-status-dot]'), statusLabel: $('[data-status-label]'),
      offline: $('[data-offline]'), tree: $('[data-tree]'), curName: $('[data-cur-name]'),
      editor: $('[data-editor]'), log: $('[data-log]'),
    }

    $('[data-reset]').addEventListener('click', () => this.reset())
    $('[data-add]').addEventListener('click', () => this.addNode())
    this.els.offline.addEventListener('click', () => this.toggleOffline())
    this.els.editor.addEventListener('input', (e) => this.edit(e.target.value))
  }

  render() {
    const s = this.state
    const sel = this.selected()
    const e = this.els

    e.nodeCount.textContent = s.nodes.length
    e.totalRows.textContent = s.nodes.reduce((a, n) => a + n.rows, 0)

    // mesh
    e.mesh.innerHTML = s.nodes.map((n, i) => {
      const meta = META[n.status]
      const edgeOff = i > 0 && (n.status === 'offline' || s.nodes[i - 1].status === 'offline')
      const edge = i > 0 ? `<div class="edge ${edgeOff ? 'off' : ''}"></div>` : ''
      return `${edge}<div class="mcol">
        <div class="mnode ${n.id === s.sel ? 'on' : ''}" data-node="${n.id}">${n.name.slice(0, 1).toUpperCase()}<span class="mdot" style="background:${meta.c}"></span></div>
        <div class="mlabel">${n.name}<div class="mrole">${n.role}</div></div>
      </div>`
    }).join('')
    e.mesh.querySelectorAll('[data-node]').forEach((el) =>
      el.addEventListener('click', () => this.selectNode(el.getAttribute('data-node'))))

    // head
    const meta = META[sel.status]
    e.selName.textContent = sel.name
    e.selRole.textContent = sel.role
    e.statusDot.style.background = meta.c
    e.statusLabel.textContent = meta.l
    e.statusLabel.style.color = meta.c
    e.offline.textContent = sel.status === 'offline' ? 'Reconnect' : 'Go offline'

    // file tree
    e.tree.innerHTML = sel.files.map((f, i) =>
      `<div class="ftrow ${i === sel.file ? 'on' : ''}" data-file="${i}"><span class="fdot"></span>${f.name}</div>`).join('')
    e.tree.querySelectorAll('[data-file]').forEach((el) =>
      el.addEventListener('click', () => this.selectFile(Number(el.getAttribute('data-file')))))

    // editor — only set value when the open document actually changes (keep caret while typing)
    const cur = sel.files[sel.file]
    e.curName.textContent = cur.name
    const docKey = `${sel.id}:${sel.file}`
    if (docKey !== this._docKey) {
      e.editor.value = cur.body
      this._docKey = docKey
    }

    // log
    e.log.innerHTML = sel.log.map((ev) =>
      `<div class="lg"><span class="lg-dot" style="background:${LDOT[ev.k] || '#9a8fb0'}"></span>${ev.text}</div>`).join('')
  }
}

const mount = document.querySelector('#sync-demo')
if (mount) new SyncDemo(mount)
