import { call, listen, pickFiles, isTauri } from './api.js';
import { prettyKey, codeToName } from './keys.js';

/* ---------- tiny helpers ---------- */

const $ = (sel, root = document) => root.querySelector(sel);

function h(tag, attrs = {}, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k === 'class') el.className = v;
    else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2), v);
    else if (k === 'value') el.value = v;
    else if (k === 'checked') el.checked = v;
    else el.setAttribute(k, v === true ? '' : v);
  }
  for (const kid of kids.flat()) {
    if (kid == null || kid === false) continue;
    el.append(kid.nodeType ? kid : document.createTextNode(String(kid)));
  }
  return el;
}

const ICONS = {
  plus: '<path d="M12 5v14M5 12h14"/>',
  minus: '<path d="M5 12h14"/>',
  play: '<path d="M7 4l13 8-13 8z"/>',
  pause: '<path d="M8 5v14M16 5v14"/>',
  stop: '<rect x="6" y="6" width="12" height="12" rx="1"/>',
  edit: '<path d="M4 20h4L19 9l-4-4L4 16z"/>',
  trash: '<path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13"/>',
  copy: '<rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h8"/>',
  gear: '<circle cx="12" cy="12" r="3"/><path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M4.9 19.1L7 17M17 7l2.1-2.1"/>',
  vol: '<path d="M4 9v6h4l5 4V5L8 9z"/><path d="M16 9a4 4 0 0 1 0 6"/>',
  mute: '<path d="M4 9v6h4l5 4V5L8 9z"/><path d="M17 9l4 6M21 9l-4 6"/>',
  loop: '<path d="M17 2l4 4-4 4"/><path d="M3 11V9a3 3 0 0 1 3-3h15"/><path d="M7 22l-4-4 4-4"/><path d="M21 13v2a3 3 0 0 1-3 3H3"/>',
  reset: '<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5"/>',
  x: '<path d="M6 6l12 12M18 6L6 18"/>',
};

function icon(name) {
  const t = document.createElement('span');
  t.innerHTML = `<svg viewBox="0 0 24 24" aria-hidden="true">${ICONS[name]}</svg>`;
  return t.firstChild;
}

function setIcon(el, name) {
  el.replaceChildren(icon(name));
}

// small icon button; stops the click so it does not select the row
function iconBtn(name, title, fn, extra = '') {
  const b = h('button', {
    class: `icon-btn small ${extra}`.trim(),
    title,
    onclick: (e) => {
      e.stopPropagation();
      fn();
    },
  });
  setIcon(b, name);
  return b;
}

const basename = (p) => String(p).split(/[\\/]/).pop();
const clone = (o) => JSON.parse(JSON.stringify(o));

let toastTimer;
function toast(msg, isError = false, ms = 0) {
  const t = $('#toast');
  t.textContent = String(msg);
  t.classList.toggle('error', isError);
  t.classList.add('show');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove('show'), ms || (isError ? 5000 : 2200));
}

/* ---------- state + saving ---------- */

let cfg = null;
let devices = { outputs: ['Disabled'], inputs: ['Disabled'] };
let lastVolume = 100;

let dirty = false;
let timer = null;

const preset = () => cfg.presets[cfg.selected_preset];

// Edits are sent to Rust a moment after the last change.
function commit() {
  dirty = true;
  clearTimeout(timer);
  timer = setTimeout(flush, 250);
}

async function flush() {
  clearTimeout(timer);
  dirty = false;
  try {
    const result = await call('set_config', { config: cfg });
    if (!dirty) mergeResult(result);
  } catch (e) {
    toast(e, true);
  }
}

// make sure Rust has our latest edits before an action that depends on them
async function sync() {
  if (dirty) await flush();
}

// take errors / counts computed by Rust, redraw only if something changed
function mergeResult(result) {
  let changed = false;
  result.presets.forEach((rp, pi) => {
    const mine = cfg.presets[pi];
    if (!mine) return;
    rp.counters.forEach((rc, ci) => {
      const m = mine.counters[ci];
      if (!m) return;
      if ((m.error || null) !== (rc.error || null) || m.count !== rc.count) {
        m.error = rc.error;
        m.count = rc.count;
        changed = true;
      }
    });
    rp.sounds.forEach((rs, si) => {
      const m = mine.sounds[si];
      if (m && (m.error || null) !== (rs.error || null)) {
        m.error = rs.error;
        changed = true;
      }
    });
  });
  if (changed) render();
}

/* ---------- counters ---------- */

// same rules as the Rust side: {0} = number, {{ and }} = braces
function formatCount(fmt, n) {
  let out = '';
  for (let i = 0; i < fmt.length; i++) {
    const c = fmt[i];
    if (c === '{') {
      if (fmt[i + 1] === '{') { out += '{'; i++; }
      else if (fmt.slice(i, i + 3) === '{0}') { out += n; i += 2; }
      else return null;
    } else if (c === '}') {
      if (fmt[i + 1] === '}') { out += '}'; i++; }
      else return null;
    } else out += c;
  }
  return out;
}

function bump(c, dir) {
  c.count += dir * c.increment;
  commit();
  renderCounters();
}

function renderCounters() {
  const p = preset();
  const list = $('#counter-list');
  list.replaceChildren();
  if (!p.counters.length) {
    list.append(h('div', { class: 'empty' }, 'No counters yet. Press + to add one.'));
    return;
  }
  p.counters.forEach((c, i) => {
    const shown = formatCount(c.format, c.count);
    list.append(
      h('div', {
        class: 'row counter' + (i === p.selected_counter ? ' selected' : ''),
        onclick: () => { p.selected_counter = i; commit(); renderCounters(); },
      },
        h('div', { class: 'main' },
          h('div', { class: 'name' }, c.name || '(no name)'),
          h('div', { class: 'sub' }, `${c.file ? basename(c.file) : 'no file'} · step ${c.increment}`),
          c.error ? h('div', { class: 'err' }, c.error) : null),
        h('div', { class: 'value' }, shown == null ? c.count : shown),
        h('div', { class: 'acts' },
          iconBtn('minus', 'Decrement', () => bump(c, -1)),
          iconBtn('plus', 'Increment', () => bump(c, +1)),
          iconBtn('reset', 'Reset to 0', () => { c.count = 0; commit(); renderCounters(); }),
          iconBtn('edit', 'Edit', () => openCounterDialog(clone(c), (d) => { p.counters[i] = d; commit(); renderCounters(); })),
          iconBtn('trash', 'Remove', () => {
            p.counters.splice(i, 1);
            p.selected_counter = Math.max(0, Math.min(p.selected_counter, p.counters.length - 1));
            commit();
            renderCounters();
          }, 'danger'))));
  });
}

function openCounterDialog(d, onSave) {
  const file = h('input', { type: 'text', value: d.file, placeholder: 'C:\\stream\\deaths.txt', oninput: () => { d.file = file.value; } });
  const body = [
    h('div', { class: 'field' }, h('label', {}, 'Name'),
      h('input', { type: 'text', value: d.name, oninput: (e) => { d.name = e.target.value; } })),
    h('div', { class: 'field' }, h('label', {}, 'Text file (shown on stream)'),
      h('div', { class: 'inline' }, file,
        h('button', {
          onclick: async () => {
            const [f] = await pickFiles({ filters: [{ name: 'Text', extensions: ['txt'] }] });
            if (f) { d.file = f; file.value = f; }
          },
        }, 'Browse')),
      h('span', { class: 'hint' }, 'You can type a new path. The file is created when it does not exist.')),
    h('div', { class: 'field' }, h('label', {}, 'Format'),
      h('input', { type: 'text', value: d.format, oninput: (e) => { d.format = e.target.value; } }),
      h('span', { class: 'hint' }, '{0} is the number. Example: "Deaths: {0}".')),
    h('div', { class: 'field' }, h('label', {}, 'Increment'),
      h('input', { type: 'number', value: d.increment, oninput: (e) => { d.increment = parseInt(e.target.value, 10) || 0; } })),
    h('div', { class: 'field' }, h('label', {}, 'Count'),
      h('input', { type: 'number', value: d.count, oninput: (e) => { d.count = parseInt(e.target.value, 10) || 0; } })),
  ];
  openDialog({
    title: d.name ? 'Edit counter' : 'New counter',
    body,
    buttons: [
      { label: 'Cancel' },
      { label: 'Save', primary: true, onclick: () => { if (!d.name.trim()) d.name = 'Counter'; d.error = null; onSave(d); } },
    ],
  });
}

/* ---------- sounds ---------- */

async function playSound(i) {
  try {
    await sync();
    await call('play_sound', { preset: cfg.selected_preset, sound: i });
  } catch (e) {
    toast(e, true);
  }
}

function keysBox(keys) {
  return h('span', { class: 'keys' },
    keys.length ? keys.map((k) => h('span', { class: 'kbd' }, prettyKey(k))) : h('span', { class: 'muted' }, 'no keys'));
}

function renderSounds() {
  const p = preset();
  const list = $('#sound-list');
  list.replaceChildren();
  if (!p.sounds.length) {
    list.append(h('div', { class: 'empty' }, 'No sounds yet. Press + to add one.'));
    return;
  }
  p.sounds.forEach((s, i) => {
    list.append(
      h('div', {
        class: 'row' + (i === p.selected_sound ? ' selected' : ''),
        onclick: () => { p.selected_sound = i; commit(); renderSounds(); },
        ondblclick: () => playSound(i),
      },
        h('div', { class: 'main' },
          h('div', { class: 'name' }, s.name || '(no name)'),
          h('div', { class: 'sub' }, keysBox(s.keys), ' ', h('span', { class: 'muted' }, `${s.files.length} file${s.files.length === 1 ? '' : 's'}`)),
          s.error ? h('div', { class: 'err' }, s.error) : null),
        s.loop ? h('span', { class: 'badge', title: 'Loop' }, 'loop') : null,
        h('span', { class: 'badge' + (s.error ? ' warn' : '') }, `${s.volume}%`),
        h('div', { class: 'acts' },
          iconBtn('play', 'Play', () => playSound(i)),
          iconBtn('edit', 'Edit', () => openSoundDialog(clone(s), (d) => { p.sounds[i] = d; commit(); renderSounds(); })),
          iconBtn('trash', 'Remove', () => {
            p.sounds.splice(i, 1);
            p.selected_sound = Math.max(0, Math.min(p.selected_sound, p.sounds.length - 1));
            commit();
            renderSounds();
          }, 'danger'))));
  });
}

function keysField(label, getKeys, setKeys) {
  const box = h('span', { class: 'keys' });
  const paint = () => box.replaceChildren(keysBox(getKeys()));
  paint();
  return h('div', { class: 'field' },
    h('span', { class: 'label' }, label),
    h('div', { class: 'inline' },
      box,
      h('span', { class: 'grow' }),
      h('button', { onclick: () => bindKeys((k) => { setKeys(k); paint(); }) }, 'Bind'),
      h('button', { onclick: () => { setKeys([]); paint(); } }, 'Clear')));
}

const AUDIO_FILTER = [{ name: 'Audio', extensions: ['mp3', 'wav', 'ogg', 'flac', 'm4a', 'aac', 'aiff', 'aif'] }];

function openSoundDialog(d, onSave) {
  const fileList = h('div', { class: 'file-list' });
  const paintFiles = () => {
    fileList.replaceChildren();
    d.files.forEach((f, idx) => {
      fileList.append(h('div', { class: 'file-item' },
        h('span', { title: f }, basename(f)),
        iconBtn('x', 'Remove file', () => { d.files.splice(idx, 1); paintFiles(); })));
    });
    if (!d.files.length) fileList.append(h('div', { class: 'muted' }, 'No files yet'));
  };
  paintFiles();

  const volLabel = h('span', { class: 'muted' }, `${d.volume}%`);
  const vol = h('input', {
    type: 'range', min: 0, max: 100, value: d.volume,
    oninput: () => { d.volume = Number(vol.value); volLabel.textContent = `${d.volume}%`; },
  });

  const body = [
    h('div', { class: 'field' }, h('label', {}, 'Name'),
      h('input', { type: 'text', value: d.name, oninput: (e) => { d.name = e.target.value; } })),
    h('div', { class: 'field' }, h('span', { class: 'label' }, 'Sound files (one is picked at random)'),
      fileList,
      h('div', { class: 'inline' },
        h('button', {
          onclick: async () => {
            const picked = await pickFiles({ multiple: true, filters: AUDIO_FILTER });
            for (const f of picked) if (!d.files.includes(f)) d.files.push(f);
            paintFiles();
          },
        }, 'Add files…'))),
    keysField('Keys', () => d.keys, (k) => { d.keys = k; }),
    h('div', { class: 'field' }, h('span', { class: 'label' }, 'Volume'),
      h('div', { class: 'inline' }, vol, volLabel)),
    h('label', { class: 'check' },
      h('input', { type: 'checkbox', checked: d.loop, onchange: (e) => { d.loop = e.target.checked; } }),
      'Loop'),
  ];
  openDialog({
    title: d.name ? 'Edit sound' : 'New sound',
    body,
    buttons: [
      { label: 'Cancel' },
      {
        label: 'Save', primary: true,
        onclick: () => {
          if (!d.name.trim()) d.name = d.files.length ? basename(d.files[0]).replace(/\.[^.]+$/, '') : 'Sound';
          d.error = null;
          onSave(d);
        },
      },
    ],
  });
}

/* ---------- presets ---------- */

function renderHeader() {
  const sel = $('#preset-select');
  sel.replaceChildren(...cfg.presets.map((p, i) => h('option', { value: String(i) }, p.name || '(no name)')));
  sel.value = String(cfg.selected_preset);

  document.querySelectorAll('#enable-seg button').forEach((b) => {
    b.classList.toggle('on', b.dataset.v === cfg.enable);
  });
  $('#counters-panel').classList.toggle('off', cfg.enable === 'Sounds');
  $('#sounds-panel').classList.toggle('off', cfg.enable === 'Counters');

  $('#master').value = cfg.volume;
  setIcon($('#mute'), cfg.volume === 0 ? 'mute' : 'vol');
  $('#mute').title = cfg.volume === 0 ? 'Unmute' : 'Mute';
}

function openPresetDialog(d, onSave, isNew) {
  const body = [
    h('div', { class: 'field' }, h('label', {}, 'Name'),
      h('input', { type: 'text', value: d.name, oninput: (e) => { d.name = e.target.value; } })),
    keysField('Keys to switch to this preset', () => d.keys, (k) => { d.keys = k; }),
  ];
  openDialog({
    title: isNew ? 'New preset' : 'Edit preset',
    body,
    buttons: [
      { label: 'Cancel' },
      { label: 'Save', primary: true, onclick: () => { if (!d.name.trim()) d.name = 'Preset'; onSave(d); } },
    ],
  });
}

async function selectPreset(index) {
  try {
    await sync();
    cfg = await call('select_preset', { index });
    render();
  } catch (e) {
    toast(e, true);
  }
}

function addPreset() {
  const fresh = { name: 'New Preset', keys: [], counters: [], sounds: [], selected_counter: 0, selected_sound: 0 };
  openPresetDialog(fresh, (d) => {
    cfg.presets.push(d);
    cfg.selected_preset = cfg.presets.length - 1;
    commit();
    render();
  }, true);
}

function clonePreset() {
  const copy = clone(preset());
  copy.name = `${copy.name} copy`;
  copy.keys = []; // a second preset on the same keys would fight with the first
  cfg.presets.push(copy);
  cfg.selected_preset = cfg.presets.length - 1;
  commit();
  render();
}

function deletePreset() {
  if (cfg.presets.length <= 1) {
    toast('You need at least one preset.', true);
    return;
  }
  openDialog({
    title: 'Delete preset',
    body: h('p', {}, `Delete "${preset().name}" with its counters and sounds?`),
    buttons: [
      { label: 'Cancel' },
      {
        label: 'Delete', primary: true,
        onclick: () => {
          cfg.presets.splice(cfg.selected_preset, 1);
          cfg.selected_preset = Math.max(0, cfg.selected_preset - 1);
          commit();
          render();
        },
      },
    ],
  });
}

/* ---------- dialogs ---------- */

const dlg = $('#dlg');

function openDialog({ title, body, buttons }) {
  $('#dlg-title').textContent = title;
  $('#dlg-body').replaceChildren(...[].concat(body));
  const foot = $('#dlg-foot');
  foot.replaceChildren();
  for (const b of buttons) {
    foot.append(h('button', {
      class: b.primary ? 'primary' : '',
      onclick: async () => {
        if (b.onclick) {
          const keep = await b.onclick();
          if (keep === false) return;
        }
        dlg.close();
      },
    }, b.label));
  }
  if (!dlg.open) dlg.showModal();
}

/* ---------- key binding ---------- */

const bindDlg = $('#bind');

// Rust sends "keys-bound" after the user releases keys (global hook).
// In the browser mock mode we read the keys from the page instead.
function bindKeys(callback) {
  const live = $('#bind-live');
  live.replaceChildren();
  const held = [];
  let done = false;
  let unlisten = null;

  const onDown = (e) => {
    e.preventDefault();
    const n = codeToName(e.code);
    if (!held.includes(n)) {
      held.push(n);
      live.replaceChildren(...held.map((k) => h('span', { class: 'kbd' }, prettyKey(k))));
    }
  };
  // Backup: the window itself also sees the key release (works even if the global hook is silent).
  const onUp = (e) => {
    e.preventDefault();
    finish([...held]);
  };
  function cleanup() {
    window.removeEventListener('keydown', onDown, true);
    window.removeEventListener('keyup', onUp, true);
    if (unlisten) unlisten();
  }
  function finish(keys) {
    if (done) return;
    done = true;
    cleanup();
    if (isTauri) call('cancel_bind').catch(() => {}); // stop the Rust side waiting
    if (bindDlg.open) bindDlg.close();
    if (keys && keys.length) callback(keys);
  }

  window.addEventListener('keydown', onDown, true);
  window.addEventListener('keyup', onUp, true);
  bindDlg.addEventListener('close', () => {
    if (!done) {
      done = true;
      cleanup();
      call('cancel_bind').catch(() => {});
    }
  }, { once: true });

  call('begin_bind').catch((e) => toast(e, true));
  listen('keys-bound', (keys) => finish(keys)).then((u) => {
    unlisten = u;
    if (done) u();
  });
  bindDlg.showModal();
}

$('#bind-cancel').addEventListener('click', () => bindDlg.close());

/* ---------- Go Live (sounds + mic -> virtual cable -> Discord) ---------- */

const AUTO_CABLE = 'Auto (VB-Cable)';
const CABLE_URL = 'https://vb-audio.com/Cable/';
let live = false;

function renderLive() {
  const b = $('#go-live');
  b.classList.toggle('on', live);
  b.textContent = live ? 'LIVE' : 'Go Live';
  b.title = live ? 'Live: Discord hears your sounds and your mic. Click to stop.'
    : 'Send your sounds and your mic to Discord through the virtual cable (VB-CABLE)';
}

// "CABLE Input (VB-Audio Virtual Cable)" is what we play to; Discord must listen to the matching "CABLE Output ..."
function discordMic(output) {
  return /\bInput\b/.test(output) ? output.replace(/\bInput\b/, 'Output') : `the matching input of ${output}`;
}

async function toggleLive() {
  const btn = $('#go-live');
  btn.disabled = true;
  try {
    const st = await call('set_live', { on: !live });
    live = st.on;
    renderLive();
    if (!st.on) {
      toast('Live stopped.');
    } else if (!st.headphones) {
      toast('LIVE, but your headphones (Primary output) did not open, so you cannot hear the sounds. Pick another Primary output in Settings, then press Go Live twice.', true, 9000);
    } else if (st.sounds && st.mic) {
      toast(`LIVE: sounds + mic go to ${st.output}. In Discord set Input Device to "${discordMic(st.output)}".`, false, 7000);
    } else if (st.sounds) {
      toast('LIVE, but your mic did not start, so Discord only hears the sounds. Check Settings > Microphone input.', true, 8000);
    } else {
      toast(`LIVE: only your mic goes to ${st.output}; sounds could not open it.`, true, 8000);
    }
  } catch (e) {
    toast(e, true, 8000);
  } finally {
    btn.disabled = false;
  }
}

/* ---------- settings ---------- */

async function openSettings() {
  try {
    devices = await call('list_devices');
  } catch (e) {
    toast(`Cannot list audio devices: ${e}`, true);
  }

  const select = (key, names) => {
    const s = h('select', { onchange: () => { cfg[key] = s.value; commit(); } },
      names.map((n) => h('option', { value: n }, n)));
    s.value = cfg[key];
    if (s.value !== cfg[key]) { // saved device is not plugged in now
      s.append(h('option', { value: cfg[key] }, `${cfg[key]} (not found)`));
      s.value = cfg[key];
    }
    return s;
  };

  const range = (key) => {
    const label = h('span', { class: 'muted' }, `${cfg[key]}%`);
    const r = h('input', {
      type: 'range', min: 0, max: 100, value: cfg[key],
      oninput: () => {
        cfg[key] = Number(r.value);
        label.textContent = `${r.value}%`;
        if (key === 'volume') renderHeader();
        commit();
      },
    });
    return h('div', { class: 'inline' }, r, label);
  };

  const dev = (label, selKey, names, volKey) =>
    h('div', { class: 'field' }, h('label', {}, label),
      h('div', { class: 'dev-row' }, select(selKey, names), range(volKey)));

  const shortcut = (label, sc) => {
    const box = h('span', {});
    const paint = () => box.replaceChildren(keysBox(sc.keys));
    paint();
    return h('div', { class: 'sc-row' },
      h('span', {}, label),
      box,
      h('button', { onclick: () => bindKeys((k) => { sc.keys = k; paint(); commit(); }) }, 'Bind'),
      h('button', { onclick: () => { sc.keys = []; paint(); commit(); } }, 'Clear'));
  };

  const cs = cfg.counter_shortcuts;
  const ss = cfg.sound_shortcuts;

  const cable = devices.cable;
  const liveChoices = [AUTO_CABLE, ...devices.outputs.filter((n) => n !== 'Disabled' && n !== 'Default Output Device')];

  const body = [
    h('div', { class: 'sect' }, 'Discord (VB-Cable)'),
    h('div', { class: `cable-status ${cable ? 'ok' : 'bad'}` },
      cable ? `Virtual cable found: ${cable}` : 'No virtual cable found.'),
    cable
      ? h('div', { class: 'hint' },
        `Press Go Live: your sounds and your mic both go to the cable. In Discord open Settings > Voice & Video, `
        + `set Input Device to "${discordMic(cable)}" and turn off Noise Suppression.`)
      : h('div', { class: 'hint' },
        `Install VB-CABLE (free) from ${CABLE_URL}, restart Windows, then open Settings again. `,
        h('button', {
          onclick: () => call('open_url', { url: CABLE_URL }).catch(
            () => navigator.clipboard.writeText(CABLE_URL).then(() => toast('Link copied.'), () => toast(CABLE_URL))),
        }, 'Open page')),
    h('div', { class: 'field' }, h('label', {}, 'Go Live output'), select('live_output', liveChoices)),

    h('div', { class: 'sect' }, 'Sound output'),
    h('div', { class: 'field' }, h('label', {}, 'Master volume'), range('volume')),
    dev('Primary output', 'primary_output', devices.outputs, 'primary_device_volume'),
    dev('Secondary output', 'secondary_output', devices.outputs, 'secondary_device_volume'),
    h('label', { class: 'check' },
      h('input', { type: 'checkbox', checked: cfg.overlap, onchange: (e) => { cfg.overlap = e.target.checked; commit(); } }),
      'Overlap sounds (do not stop the old sound when a new one starts)'),

    h('div', { class: 'sect' }, 'Microphone passthrough'),
    h('div', { class: 'hint' }, 'Sends your mic to an output device (for example a virtual cable) so it travels with the soundboard. Go Live uses the Mic input below (the default mic if it is Disabled) and ignores Mic output.'),
    h('div', { class: 'field' }, h('label', {}, 'Mic input'), select('mic_input', devices.inputs)),
    h('div', { class: 'field' }, h('label', {}, 'Mic output'), select('mic_output', devices.outputs)),
    h('div', { class: 'field' }, h('label', {}, 'Mic volume'), range('mic_volume')),

    h('div', { class: 'sect' }, 'Counter shortcuts'),
    shortcut('Next counter', cs.next),
    shortcut('Previous counter', cs.previous),
    shortcut('Increment', cs.increment),
    shortcut('Decrement', cs.decrement),
    shortcut('Reset', cs.reset),

    h('div', { class: 'sect' }, 'Sound shortcuts'),
    shortcut('Pause all', ss.pause),
    shortcut('Continue', ss.continue),
    shortcut('Stop all', ss.stop),
  ];

  openDialog({ title: 'Settings', body, buttons: [{ label: 'Close', primary: true }] });
}

/* ---------- render + wiring ---------- */

function render() {
  renderHeader();
  renderCounters();
  renderSounds();
}

function wire() {
  setIcon($('#preset-add'), 'plus');
  setIcon($('#preset-clone'), 'copy');
  setIcon($('#preset-edit'), 'edit');
  setIcon($('#preset-del'), 'trash');
  setIcon($('#open-settings'), 'gear');
  setIcon($('#counter-add'), 'plus');
  setIcon($('#sound-add'), 'plus');
  setIcon($('#sound-stop'), 'stop');
  setIcon($('#sound-pause'), 'pause');
  setIcon($('#sound-play'), 'play');

  $('#preset-select').addEventListener('change', (e) => selectPreset(Number(e.target.value)));
  $('#preset-add').addEventListener('click', addPreset);
  $('#preset-clone').addEventListener('click', clonePreset);
  $('#preset-edit').addEventListener('click', () => {
    const i = cfg.selected_preset;
    openPresetDialog(clone(cfg.presets[i]), (d) => { cfg.presets[i] = d; commit(); render(); }, false);
  });
  $('#preset-del').addEventListener('click', deletePreset);
  $('#open-settings').addEventListener('click', openSettings);
  $('#go-live').addEventListener('click', toggleLive);

  document.querySelectorAll('#enable-seg button').forEach((b) => {
    b.addEventListener('click', () => { cfg.enable = b.dataset.v; commit(); renderHeader(); });
  });

  $('#counter-add').addEventListener('click', () => {
    const p = preset();
    const fresh = { name: '', file: '', format: '{0}', increment: 1, count: 0, error: null };
    openCounterDialog(fresh, (d) => { p.counters.push(d); p.selected_counter = p.counters.length - 1; commit(); renderCounters(); });
  });

  $('#sound-add').addEventListener('click', () => {
    const p = preset();
    const fresh = { name: '', files: [], keys: [], volume: 100, loop: false, error: null };
    openSoundDialog(fresh, (d) => { p.sounds.push(d); p.selected_sound = p.sounds.length - 1; commit(); renderSounds(); });
  });

  $('#sound-play').addEventListener('click', () => {
    const p = preset();
    if (p.sounds.length) playSound(p.selected_sound);
  });
  $('#sound-pause').addEventListener('click', () => call('audio_pause').catch((e) => toast(e, true)));
  $('#sound-stop').addEventListener('click', () => call('audio_stop').catch((e) => toast(e, true)));

  $('#master').addEventListener('input', (e) => {
    cfg.volume = Number(e.target.value);
    setIcon($('#mute'), cfg.volume === 0 ? 'mute' : 'vol');
    commit();
  });
  $('#mute').addEventListener('click', () => {
    if (cfg.volume > 0) { lastVolume = cfg.volume; cfg.volume = 0; }
    else cfg.volume = lastVolume || 100;
    renderHeader();
    commit();
  });
}

/* ---------- new version notice (installed app only) ---------- */
// No auto-install and no signing key: we ask GitHub for the newest Release and, if it is
// newer than this build, show a bar with a Download button (opens the Release page).

const REPO_API = 'https://api.github.com/repos/same-09/RustBoard/releases/latest';

const versionParts = (v) => String(v).replace(/^v/i, '').split(/[.+-]/).map((n) => parseInt(n, 10) || 0);

function isNewer(latest, current) {
  const a = versionParts(latest);
  const b = versionParts(current);
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    const d = (a[i] || 0) - (b[i] || 0);
    if (d !== 0) return d > 0;
  }
  return false;
}

async function checkForUpdate() {
  if (!isTauri) return;
  try {
    const [current, res] = await Promise.all([
      call('app_version'),
      fetch(REPO_API, { headers: { accept: 'application/vnd.github+json' } }),
    ]);
    if (!res.ok) return; // no Release yet, offline or rate limited: stay quiet
    const rel = await res.json();
    if (!rel.tag_name || !isNewer(rel.tag_name, current)) return;

    $('#update-text').textContent = `New version ${rel.tag_name} is available (you have ${current}).`;
    $('#update-btn').onclick = () => call('open_url', { url: rel.html_url }).catch((e) => toast(e, true));
    $('#update-later').onclick = () => { $('#update-bar').hidden = true; };
    $('#update-bar').hidden = false;
  } catch (e) {
    console.warn('update check failed', e);
  }
}

async function init() {
  wire();
  cfg = await call('get_state');
  lastVolume = cfg.volume || 100;
  render();
  renderLive();

  // hotkeys change counters / selection in Rust, then tell us
  listen('config-changed', (c) => { cfg = c; render(); });
  listen('audio-error', (msg) => toast(msg, true));

  checkForUpdate();
}

init().catch((e) => toast(`Cannot reach backend: ${e}`, true));
