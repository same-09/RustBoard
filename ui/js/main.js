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
  chev: '<path d="M6 9l6 6 6-6"/>',
  check: '<path d="M5 12.5l4.5 4.5L19 7"/>',
  bars: '<path d="M4 10v4M8 6v12M12 3v18M16 8v8M20 11v2"/>',
  mic: '<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0 0 14 0M12 18v3"/>',
};

// a solid triangle for the cover hover (the stroke play icon is too thin at 16px)
const SOLID_PLAY = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 5.5v13a1 1 0 0 0 1.5.86l11-6.5a1 1 0 0 0 0-1.72l-11-6.5A1 1 0 0 0 8 5.5z"/></svg>';

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

// take errors computed by Rust, redraw only if something changed
function mergeResult(result) {
  let changed = false;
  result.presets.forEach((rp, pi) => {
    const mine = cfg.presets[pi];
    if (!mine) return;
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

function emptyState(title, text, label, fn) {
  return h('div', { class: 'empty' },
    h('div', { class: 'cover big', style: '--h:250' }, icon('bars')),
    h('div', { class: 't' }, title),
    h('div', { class: 'd' }, text),
    h('button', { class: 'primary', onclick: fn }, icon('plus'), label));
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

const hueOf = (name) => [...String(name)].reduce((a, ch) => (a * 31 + ch.charCodeAt(0)) >>> 0, 7) % 360;

// the selected sound tints the backdrop, like the cover does in Liner
function setAmbient(sound) {
  const hue = sound ? hueOf(sound.name || (sound.files[0] || 'sound')) : 232;
  const root = document.documentElement;
  if (root.style.getPropertyValue('--hue') === String(hue)) return;
  root.style.setProperty('--hue', hue);
  const amb = $('#ambient');
  amb.classList.remove('swap');
  void amb.offsetWidth;
  amb.classList.add('swap');
}

function soundCover(name, onPlay) {
  const t = document.createElement('span');
  t.innerHTML = SOLID_PLAY;
  const go = h('span', { class: 'go' }, t.firstChild);
  return h('div', {
    class: 'cover',
    style: `--h:${hueOf(name)}`,
    title: 'Play',
    onclick: (e) => { e.stopPropagation(); onPlay(); },
  }, icon('bars'), go);
}

function renderSounds() {
  const p = preset();
  const list = $('#sound-list');
  $('#sound-count').textContent = p.sounds.length || '';
  list.replaceChildren();
  setAmbient(p.sounds[p.selected_sound]);
  if (!p.sounds.length) {
    list.append(emptyState('No sounds yet', 'Add a sound file and give it a key. Press the key anywhere and it plays.', 'Add sound', () => $('#sound-add').click()));
    return;
  }
  const select = (i) => {
    p.selected_sound = i;
    commit();
    list.querySelectorAll('.sound').forEach((el, idx) => el.classList.toggle('selected', idx === i));
    setAmbient(p.sounds[i]);
  };
  p.sounds.forEach((s, i) => {
    const n = s.files.length;
    list.append(
      h('div', {
        class: 'sound' + (i === p.selected_sound ? ' selected' : ''),
        onclick: () => select(i),
        ondblclick: () => playSound(i),
      },
        soundCover(s.name || s.files[0] || 'sound', () => { select(i); playSound(i); }),
        h('div', { class: 'main' },
          h('div', { class: 'name' }, s.name || '(no name)'),
          h('div', { class: 'sub' }, keysBox(s.keys), h('span', {}, `${n} file${n === 1 ? '' : 's'}`)),
          s.error ? h('div', { class: 'err' }, s.error) : null),
        s.loop ? h('span', { class: 'tag', title: 'Loops until stopped' }, icon('loop')) : null,
        h('span', { class: 'vol-pct' + (s.error ? ' warn' : '') }, `${s.volume}%`),
        h('div', { class: 'acts' },
          iconBtn('edit', 'Edit', () => openSoundDialog(clone(s), (d) => { p.sounds[i] = d; commit(); renderSounds(); })),
          iconBtn('trash', 'Remove', () => {
            p.sounds.splice(i, 1);
            p.selected_sound = Math.max(0, Math.min(p.selected_sound, p.sounds.length - 1));
            commit();
            renderSounds();
          }, 'danger'))));
  });
}

const switchRow = (title, desc, input) =>
  h('div', { class: 'switch-row' },
    h('div', { class: 'txt' }, h('div', { class: 't' }, title), desc ? h('div', { class: 'd' }, desc) : null),
    input);

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
    switchRow('Loop', 'Play again and again until you stop it',
      h('input', { type: 'checkbox', checked: d.loop, onchange: (e) => { d.loop = e.target.checked; } })),
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
  $('#preset-name').textContent = preset().name || '(no name)';

  $('#master').value = cfg.volume;
  paintRange($('#master'));
  setIcon($('#mute'), cfg.volume === 0 ? 'mute' : 'vol');
  $('#mute').title = cfg.volume === 0 ? 'Unmute' : 'Mute';
}

// slider fill follows the value
function paintRange(r) {
  const min = Number(r.min) || 0;
  const max = Number(r.max) || 100;
  r.style.setProperty('--p', `${((r.value - min) / (max - min || 1)) * 100}%`);
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
  const fresh = { name: 'New Preset', keys: [], sounds: [], selected_sound: 0 };
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
    body: h('p', {}, `Delete "${preset().name}" with its sounds?`),
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

/* ---------- preset menu ---------- */

let menuEl = null;

function closeMenu() {
  if (!menuEl) return;
  menuEl.remove();
  menuEl = null;
  document.removeEventListener('pointerdown', onMenuOutside, true);
  window.removeEventListener('keydown', onMenuKey, true);
  window.removeEventListener('blur', closeMenu);
}
function onMenuOutside(e) {
  if (menuEl && !menuEl.contains(e.target) && !e.target.closest('#preset-btn')) closeMenu();
}
function onMenuKey(e) {
  if (e.key === 'Escape') { e.stopPropagation(); closeMenu(); }
}

function openPresetMenu() {
  if (menuEl) { closeMenu(); return; }
  const btn = $('#preset-btn');

  const pick = (i) => h('button', {
    class: 'menu-item',
    onclick: () => { closeMenu(); if (i !== cfg.selected_preset) selectPreset(i); },
  },
    h('span', { class: 'slot' }, i === cfg.selected_preset ? icon('check') : null),
    h('span', { class: 'grow' }, cfg.presets[i].name || '(no name)'),
    cfg.presets[i].keys.length ? h('span', { class: 'side' }, cfg.presets[i].keys.map(prettyKey).join(' + ')) : null);

  const act = (ic, label, fn, extra = '') => h('button', {
    class: 'menu-item ' + extra,
    onclick: () => { closeMenu(); fn(); },
  }, h('span', { class: 'slot' }, icon(ic)), h('span', { class: 'grow' }, label));

  menuEl = h('div', { class: 'menu', role: 'menu' },
    cfg.presets.map((_, i) => pick(i)),
    h('div', { class: 'menu-sep' }),
    act('plus', 'New preset', addPreset),
    act('copy', 'Duplicate', clonePreset),
    act('edit', 'Rename and keys', () => {
      const i = cfg.selected_preset;
      openPresetDialog(clone(cfg.presets[i]), (d) => { cfg.presets[i] = d; commit(); render(); }, false);
    }),
    act('trash', 'Delete', deletePreset, 'danger'));

  document.body.append(menuEl);
  const r = btn.getBoundingClientRect();
  menuEl.style.left = `${Math.max(8, Math.min(r.left, window.innerWidth - menuEl.offsetWidth - 8))}px`;
  menuEl.style.top = `${r.bottom + 6}px`;
  menuEl.style.maxHeight = `${window.innerHeight - r.bottom - 14}px`;

  document.addEventListener('pointerdown', onMenuOutside, true);
  window.addEventListener('keydown', onMenuKey, true);
  window.addEventListener('blur', closeMenu);
}

/* ---------- dialogs ---------- */

const dlg = $('#dlg');

function openDialog({ title, body, buttons, wide = false }) {
  dlg.classList.toggle('wide', wide);
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
  $('#dlg-body').scrollTop = 0;
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
  $('#live-label').textContent = live ? 'Live' : 'Go Live';
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

let view = 'home';
function showView(v) {
  view = v;
  const st = v === 'settings';
  document.querySelector('.app').classList.toggle('in-settings', st);
  $('.split').hidden = st;
  $('#settings-page').hidden = !st;
  $('#open-settings').classList.toggle('active', st);
}

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

  const section = (label, ...rows) =>
    h('section', { class: 'set-section' }, h('h4', {}, label), h('div', { class: 'set-card' }, rows));

  const row = (title, desc, control) =>
    h('div', { class: 'set-row' },
      h('div', { class: 'txt' }, h('div', { class: 't' }, title), desc ? h('div', { class: 'd' }, desc) : null),
      control ? h('div', { class: 'ctl' }, control) : null);

  const device = (title, selKey, names, volKey) =>
    h('div', { class: 'set-row col' },
      h('div', { class: 'txt' }, h('div', { class: 't' }, title)),
      h('div', { class: 'dev-row' }, select(selKey, names), range(volKey)));

  const shortcut = (label, sc) => {
    const box = h('span', {});
    const paint = () => box.replaceChildren(keysBox(sc.keys));
    paint();
    return row(label, null, [
      box,
      h('button', { onclick: () => bindKeys((k) => { sc.keys = k; paint(); commit(); }) }, 'Bind'),
      h('button', { onclick: () => { sc.keys = []; paint(); commit(); } }, 'Clear'),
    ]);
  };

  const ss = cfg.sound_shortcuts;

  const cable = devices.cable;
  const liveChoices = [AUTO_CABLE, ...devices.outputs.filter((n) => n !== 'Disabled' && n !== 'Default Output Device')];

  const copy = (text) => navigator.clipboard.writeText(text).then(() => toast('Copied.'), () => toast(text));

  const body = [
    section('Discord',
      cable
        ? row('Virtual cable', cable, h('span', { class: 'ok' }, 'Ready'))
        : row('Virtual cable', 'Install VB-CABLE (free), restart Windows, then open Settings again.',
          h('button', {
            onclick: () => call('open_url', { url: CABLE_URL }).catch(() => copy(CABLE_URL)),
          }, 'Get VB-CABLE')),
      cable
        ? row('Discord input device',
          `In Discord open Voice & Video and pick "${discordMic(cable)}". Turn Noise Suppression off.`,
          h('button', { onclick: () => copy(discordMic(cable)) }, icon('copy'), 'Copy name'))
        : null,
      row('Go Live output', 'Where your sounds and mic are sent.', select('live_output', liveChoices))),

    section('Sound output',
      row('Master volume', null, range('volume')),
      device('Primary output', 'primary_output', devices.outputs, 'primary_device_volume'),
      device('Secondary output', 'secondary_output', devices.outputs, 'secondary_device_volume'),
      row('Overlap sounds', 'Do not stop the old sound when a new one starts.',
        h('input', { type: 'checkbox', checked: cfg.overlap, onchange: (e) => { cfg.overlap = e.target.checked; commit(); } }))),

    section('Microphone passthrough',
      row('Mic input', 'Go Live uses this mic, or the default mic when it is Disabled.', select('mic_input', devices.inputs)),
      row('Mic output', 'Send the mic to a device, for example a virtual cable. Go Live ignores this.', select('mic_output', devices.outputs)),
      row('Mic volume', null, range('mic_volume'))),

    section('Sound shortcuts',
      shortcut('Pause all', ss.pause),
      shortcut('Continue', ss.continue),
      shortcut('Stop all', ss.stop)),
  ];

  $('#settings-body').replaceChildren(...body);
  $('#settings-body').scrollTop = 0;
  showView('settings');
}

/* ---------- render + wiring ---------- */

function render() {
  renderHeader();
  renderSounds();
}

function wire() {
  setIcon($('#preset-chev'), 'chev');
  setIcon($('#open-settings'), 'gear');
  setIcon($('#live-ico'), 'mic');
  setIcon($('#add-ico'), 'plus');
  setIcon($('#sound-stop'), 'stop');
  setIcon($('#sound-pause'), 'pause');
  setIcon($('#play-ico'), 'play');

  $('#preset-btn').addEventListener('click', openPresetMenu);
  $('#open-settings').addEventListener('click', () => (view === 'settings' ? showView('home') : openSettings()));
  $('#open-home').addEventListener('click', () => showView('home'));
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && view === 'settings' && !dlg.open && !bindDlg.open) showView('home');
  });
  $('#go-live').addEventListener('click', toggleLive);

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

  // slider fill: paint on input, and for every slider a dialog adds later
  document.addEventListener('input', (e) => {
    if (e.target.matches && e.target.matches('input[type="range"]')) paintRange(e.target);
  }, true);
  new MutationObserver((list) => {
    for (const m of list) {
      for (const n of m.addedNodes) {
        if (n.nodeType !== 1) continue;
        (n.matches('input[type="range"]') ? [n] : [...n.querySelectorAll('input[type="range"]')]).forEach(paintRange);
      }
    }
  }).observe(document.body, { childList: true, subtree: true });

}

/* ---------- new version notice (installed app only) ---------- */
// No signing key: we ask GitHub for the newest Release. If it is newer than this build, the bar
// offers Update: Rust downloads the setup (SHA-256 checked), starts it and quits. If the Release
// has no setup file, the button opens the Release page instead.

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

    const text = $('#update-text');
    const btn = $('#update-btn');
    const later = $('#update-later');
    const asset = (rel.assets || []).find((x) => /-setup\.exe$/i.test(x.name));
    text.textContent = `New version ${rel.tag_name} is available (you have ${current}).`;
    later.onclick = () => { $('#update-bar').hidden = true; };

    if (!asset) {
      btn.textContent = 'Download';
      btn.onclick = () => call('open_url', { url: rel.html_url }).catch((e) => toast(e, true));
    } else {
      btn.textContent = 'Update';
      btn.onclick = async () => {
        btn.disabled = later.disabled = true;
        text.textContent = 'Starting download…';
        const un1 = await listen('update-progress', ({ done, total }) => {
          const mb = (n) => (n / 1048576).toFixed(1);
          text.textContent = total ? `Downloading ${rel.tag_name}… ${Math.round((done / total) * 100)}% (${mb(done)} / ${mb(total)} MB)` : `Downloading ${rel.tag_name}… ${mb(done)} MB`;
        });
        const un2 = await listen('update-error', (msg) => {
          un1(); un2();
          text.textContent = String(msg);
          btn.textContent = 'Try again';
          btn.disabled = later.disabled = false;
        });
        try {
          await call('install_update', { url: asset.browser_download_url, digest: asset.digest || null });
        } catch (e) {
          un1(); un2();
          text.textContent = String(e);
          btn.disabled = later.disabled = false;
        }
      };
    }
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

  // hotkeys change the selection in Rust, then tell us
  listen('config-changed', (c) => { cfg = c; render(); });
  listen('audio-error', (msg) => toast(msg, true));

  checkForUpdate();
}

init().catch((e) => toast(`Cannot reach backend: ${e}`, true));
