import { call, listen, pickFiles, pickFolder, isTauri } from './api.js';
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
  folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
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
    wave.lastPoll = 0; // look at the level right away, no wait
  } catch (e) {
    toast(e, true);
  }
}

// "Import folder": every sound file in a folder becomes its own sound (named like the file).
// Files are copied to the Soundboard folder. Files already on the board are not added twice.
async function importFolder() {
  const dir = await pickFolder();
  if (!dir) return;
  try {
    const r = await call('import_folder', { folder: dir });
    const p = preset();
    const used = new Set(p.sounds.flatMap((s) => s.files));
    const fresh = [];
    for (const f of r.files) {
      if (used.has(f.path)) continue;
      used.add(f.path);
      fresh.push({ name: f.name, files: [f.path], keys: [], volume: 100, loop: false, fx: 'clean', image: '', error: null });
    }
    if (!fresh.length) {
      toast('Nothing new: these sounds are already on the board.');
      return;
    }
    p.selected_sound = p.sounds.length;
    p.sounds.push(...fresh);
    commit();
    renderSounds();
    const n = fresh.length;
    toast(`Imported ${n} sound${n === 1 ? '' : 's'}${r.skipped ? `, ${r.skipped} skipped` : ''}. Copies are in ${r.folder}`, false, 6000);
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

// sound pictures: Rust reads the file and gives a data URL; we keep it per path
const imageCache = new Map();
function loadImage(path) {
  if (!path) return Promise.resolve(null);
  if (!imageCache.has(path)) imageCache.set(path, call('image_data', { path }).catch(() => null));
  return imageCache.get(path);
}

const IMAGE_FILTER = [{ name: 'Picture', extensions: ['png', 'jpg', 'jpeg', 'webp', 'gif', 'bmp'] }];

/* ---------- dancing bars on the tile cover ---------- */
// Rust splits what the soundboard plays into 5 bands (bass .. treble) and `get_sound_level`
// gives the loudest level of each since the last call. Each white bar follows one band.
// Still icon = rest heights. Bars jump up fast, fall back slowly, then settle into the icon.

const SVG_NS = 'http://www.w3.org/2000/svg';
const WAVE_X = [4, 8, 12, 16, 20];
const WAVE_REST = [2, 6, 9, 4, 1];          // half heights of the still icon
const WAVE_TILT_DB = [0, 3, 6, 9, 12];      // treble has less energy: lift it a little
const WAVE_FLOOR_DB = -56;                  // this level = bar at its smallest
const WAVE_RANGE_DB = 46;                   // ...and this much louder = bar at full height
const WAVE_MIN = 0.6;                       // half height of a silent bar while the sound plays
const WAVE_MAX = 10.5;                      // half height of a full bar (viewBox is 24 high)
const WAVE_HOLD_MS = 250;                   // short silence does not drop the bars to rest yet

const wave = {
  tiles: [],            // { svg, files, live } for the tiles on screen
  target: [0, 0, 0, 0, 0],
  level: [0, 0, 0, 0, 0],
  act: 0,               // 0 = still icon, 1 = dancing
  file: '',
  busy: false,
  lastPoll: 0,
  lastLoud: -1e9,
  started: false,
};

function setWaveBars(svg, halves) {
  svg._bars.forEach((l, i) => {
    l.setAttribute('y1', (12 - halves[i]).toFixed(2));
    l.setAttribute('y2', (12 + halves[i]).toFixed(2));
  });
}

function waveIcon() {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('aria-hidden', 'true');
  svg.classList.add('wave');
  svg._bars = WAVE_X.map((x) => {
    const l = document.createElementNS(SVG_NS, 'line');
    l.setAttribute('x1', x);
    l.setAttribute('x2', x);
    svg.append(l);
    return l;
  });
  setWaveBars(svg, WAVE_REST);
  return svg;
}

function pollWave(now) {
  wave.busy = true;
  wave.lastPoll = now;
  call('get_sound_level').then((l) => {
    wave.file = l.file || '';
    let loud = false;
    l.bands.forEach((v, i) => {
      const db = v > 1e-5 ? 20 * Math.log10(v) : -100;
      const t = Math.max(0, Math.min(1, (db + WAVE_TILT_DB[i] - WAVE_FLOOR_DB) / WAVE_RANGE_DB));
      wave.target[i] = t;
      if (v > 1e-4) loud = true;
    });
    if (loud) wave.lastLoud = performance.now();
  }).catch(() => {
    wave.target.fill(0);
  }).finally(() => {
    wave.busy = false;
  });
}

function waveFrame(now) {
  requestAnimationFrame(waveFrame);
  if (document.hidden || view !== 'home') return;

  const dancing = now - wave.lastLoud < WAVE_HOLD_MS;
  // while a sound plays: ask about 30 times a second; idle: 10 times a second is enough
  if (!wave.busy && now - wave.lastPoll >= (dancing || wave.act > 0.01 ? 33 : 100)) pollWave(now);

  for (let i = 0; i < wave.level.length; i++) {
    const t = wave.target[i];
    wave.level[i] += (t - wave.level[i]) * (t > wave.level[i] ? 0.6 : 0.14);
  }
  wave.act += ((dancing ? 1 : 0) - wave.act) * (dancing ? 0.4 : 0.1);
  if (!dancing && wave.act < 0.01) wave.act = 0;

  const peak = Math.max(...wave.level);
  for (const tile of wave.tiles) {
    const mine = wave.act > 0 && wave.file && tile.files.includes(wave.file);
    if (!mine) {
      if (tile.live) { // back to the still icon
        setWaveBars(tile.svg, WAVE_REST);
        tile.cover.classList.remove('live');
        tile.cover.style.removeProperty('--lvl');
        tile.live = false;
      }
      continue;
    }
    const halves = WAVE_REST.map((rest, i) => {
      const dance = WAVE_MIN + wave.level[i] * (WAVE_MAX - WAVE_MIN);
      return rest + (dance - rest) * wave.act;
    });
    setWaveBars(tile.svg, halves);
    tile.cover.classList.add('live');
    tile.cover.style.setProperty('--lvl', (peak * wave.act).toFixed(3));
    tile.live = true;
  }
}

function startWave() {
  if (wave.started) return;
  wave.started = true;
  requestAnimationFrame(waveFrame);
}

function soundCover(name, onPlay, image = '', files = []) {
  const t = document.createElement('span');
  t.innerHTML = SOLID_PLAY;
  const go = h('span', { class: 'go' }, t.firstChild);
  const svg = waveIcon();
  const cover = h('div', {
    class: 'cover',
    style: `--h:${hueOf(name)}`,
    title: 'Play',
    onclick: (e) => { e.stopPropagation(); onPlay(); },
  }, svg);
  wave.tiles.push({ svg, cover, files, live: false });
  if (image) {
    loadImage(image).then((url) => {
      if (!url) return; // missing file: keep the generated cover
      cover.prepend(h('img', { class: 'pic', src: url, alt: '', draggable: 'false' }));
      cover.classList.add('has-pic');
    });
  }
  cover.append(go);
  return cover;
}

function renderSounds() {
  const p = preset();
  const list = $('#sound-list');
  $('#sound-count').textContent = p.sounds.length || '';
  list.replaceChildren();
  wave.tiles = []; // the covers are drawn again below
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
    const fxName = s.fx && s.fx !== 'clean' ? (FX.find((f) => f[0] === s.fx) || [0, s.fx])[1] : '';
    list.append(
      h('div', {
        class: 'sound' + (i === p.selected_sound ? ' selected' : ''),
        title: s.name || '',
        onclick: () => select(i),
        ondblclick: () => playSound(i),
      },
        h('div', { class: 'art' },
          soundCover(s.name || s.files[0] || 'sound', () => { select(i); playSound(i); }, s.image, s.files),
          h('div', { class: 'acts' },
            iconBtn('edit', 'Edit', () => openSoundDialog(clone(s), (d) => { p.sounds[i] = d; commit(); renderSounds(); })),
            iconBtn('trash', 'Remove', () => {
              p.sounds.splice(i, 1);
              p.selected_sound = Math.max(0, Math.min(p.selected_sound, p.sounds.length - 1));
              commit();
              renderSounds();
            }, 'danger')),
          h('div', { class: 'chips' },
            fxName ? h('span', { class: 'chip', title: 'Sound effect' }, fxName) : null,
            s.loop ? h('span', { class: 'chip', title: 'Loops until stopped' }, icon('loop')) : null,
            h('span', { class: 'chip vol' + (s.error ? ' warn' : '') }, `${s.volume}%`))),
        h('div', { class: 'name' }, s.name || '(no name)'),
        h('div', { class: 'sub' }, keysBox(s.keys), h('span', {}, `${n} file${n === 1 ? '' : 's'}`)),
        s.error ? h('div', { class: 'err', title: s.error }, s.error.replace(/\s+/g, ' ')) : null));
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

  const pic = h('div', { class: 'pic-prev' });
  const paintPic = () => {
    pic.replaceChildren();
    pic.classList.toggle('empty', !d.image);
    if (!d.image) { pic.append(icon('bars')); return; }
    loadImage(d.image).then((url) => {
      if (!d.image) return;
      pic.replaceChildren(url ? h('img', { src: url, alt: '' }) : h('span', { class: 'muted' }, 'Cannot read'));
    });
  };
  paintPic();

  const fxSelect = h('select', { onchange: () => { d.fx = fxSelect.value; } },
    FX.map(([id, name]) => h('option', { value: id }, id === 'clean' ? 'None' : name)));
  fxSelect.value = FX.some((f) => f[0] === d.fx) ? d.fx : 'clean';

  const body = [
    h('div', { class: 'field' }, h('label', {}, 'Name'),
      h('input', { type: 'text', value: d.name, oninput: (e) => { d.name = e.target.value; } })),
    h('div', { class: 'field' }, h('span', { class: 'label' }, 'Sound files (one is picked at random)'),
      fileList,
      h('div', { class: 'inline' },
        h('button', {
          onclick: async () => {
            const picked = await pickFiles({ multiple: true, filters: AUDIO_FILTER });
            if (!picked.length) return;
            try {
              // copies go to the Soundboard folder, the sound keeps the copy
              const r = await call('import_files', { paths: picked });
              for (const f of r.files) if (!d.files.includes(f.path)) d.files.push(f.path);
              if (r.skipped) toast(`${r.skipped} file${r.skipped === 1 ? '' : 's'} skipped (not a sound file or cannot be read).`, true);
            } catch (e) {
              toast(e, true);
            }
            paintFiles();
          },
        }, 'Add files…'))),
    keysField('Keys', () => d.keys, (k) => { d.keys = k; }),
    h('div', { class: 'field' }, h('span', { class: 'label' }, 'Volume'),
      h('div', { class: 'inline' }, vol, volLabel)),
    h('div', { class: 'field' }, h('span', { class: 'label' }, 'Picture'),
      h('div', { class: 'pic-row' }, pic,
        h('button', {
          onclick: async () => {
            const [f] = await pickFiles({ multiple: false, filters: IMAGE_FILTER });
            if (f) { d.image = f; paintPic(); }
          },
        }, 'Choose picture…'),
        h('button', { onclick: () => { d.image = ''; paintPic(); } }, 'Remove'))),
    h('div', { class: 'field' }, h('label', {}, 'Sound effect'), fxSelect),
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
const REPO_URL = 'https://github.com/same-09/RustBoard/';
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
// three tabs: Settings, Soundboard ('home'), Sound Effects ('fx')
const PAGES = { settings: '#settings-page', home: '.split', fx: '#fx-page' };
const TABS = { settings: '#tab-settings', home: '#tab-home', fx: '#tab-fx' };

function showView(v) {
  view = v;
  if (v !== 'settings') stopMeters();
  document.querySelector('.app').classList.toggle('in-settings', v !== 'home');
  for (const [name, sel] of Object.entries(PAGES)) $(sel).hidden = name !== v;
  for (const [name, sel] of Object.entries(TABS)) {
    const t = $(sel);
    t.classList.toggle('active', name === v);
    t.setAttribute('aria-selected', String(name === v));
  }
}

function openView(v) {
  if (v === 'settings') return openSettings();
  if (v === 'fx') return openFx();
  showView('home');
}

/* ---------- mic meters (Settings) ---------- */
// Rust keeps the loudest level since the last call; we ask about 12 times a second while the
// Settings page is open and the meters are on. Bars use a -70..0 dBFS scale, like the gate slider.

const METER_MIN_DB = -70;
const meterPct = (db) => Math.max(0, Math.min(100, ((db - METER_MIN_DB) / -METER_MIN_DB) * 100));
let meterTimer = null;

function stopMeters() {
  clearInterval(meterTimer);
  meterTimer = null;
}

// `m` = { inFill, outFill, inDb, outDb, note }. Bars jump up fast and fall back slowly.
function startMeters(m) {
  stopMeters();
  let shownIn = METER_MIN_DB, shownOut = METER_MIN_DB, busy = false;
  const fall = (shown, db) => (db >= shown ? db : Math.max(db, shown - 3.5));
  const label = (db) => (db <= METER_MIN_DB ? '-∞' : `${Math.round(db)} dB`);

  meterTimer = setInterval(async () => {
    if (busy || view !== 'settings' || document.hidden) return;
    busy = true;
    try {
      const l = await call('get_levels');
      shownIn = fall(shownIn, l.input_db);
      shownOut = fall(shownOut, l.output_db);
      m.inFill.style.width = `${meterPct(shownIn)}%`;
      m.outFill.style.width = `${meterPct(shownOut)}%`;
      m.inDb.textContent = label(shownIn);
      m.outDb.textContent = label(shownOut);
      m.outFill.classList.toggle('shut', cfg.gate_on && l.active && !l.gate_open);
      m.note.textContent = !l.active
        ? 'Mic is off. Pick a Mic input and a Mic output above, or press Go Live.'
        : cfg.gate_on
          ? (l.gate_open ? 'Gate open: your voice goes through.' : 'Gate closed: the mic is muted.')
          : 'Gate is off: the mic goes through as it is.';
    } catch (_) {
      /* the next tick tries again */
    } finally {
      busy = false;
    }
  }, 80);
}

// Listing audio devices is slow on Windows (can take seconds). Show Settings right away
// from the cached list, refresh in the background, and redraw only if the list changed.
let devicesLoaded = false;
async function refreshDevices() {
  const before = JSON.stringify(devices);
  devices = await call('list_devices');
  devicesLoaded = true;
  return JSON.stringify(devices) !== before;
}

async function openSettings() {
  if (devicesLoaded) {
    refreshDevices()
      .then((changed) => { if (changed && view === 'settings') buildSettings(); })
      .catch(() => {});
  } else {
    try {
      await refreshDevices();
    } catch (e) {
      toast(`Cannot list audio devices: ${e}`, true);
    }
  }
  buildSettings();
}

function buildSettings() {
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

  // Noise gate + level meters. Both have an on/off switch; the threshold slider and the
  // threshold line on the IN bar only matter while the gate is on.
  const micGateSection = () => {
    // only the IN bar gets the red threshold line
    const bar = (name, withMark) => {
      const fill = h('i', { class: 'm-fill' });
      const mark = withMark ? h('b', { class: 'm-mark' }) : null;
      const db = h('span', { class: 'muted m-db' }, '-∞');
      return { fill, mark, db, el: h('div', { class: 'meter' }, h('span', { class: 'm-lab' }, name), h('div', { class: 'm-bar' }, fill, mark), db) };
    };
    const inBar = bar('IN', true);
    const outBar = bar('OUT', false);
    const note = h('div', { class: 'd' }, '');
    const meters = h('div', { class: 'set-row col meters' }, inBar.el, outBar.el, note);

    const label = h('span', { class: 'muted db' }, `${cfg.gate_threshold_db} dB`);
    const thr = h('input', {
      type: 'range', min: -70, max: -20, step: 1, value: cfg.gate_threshold_db,
      oninput: () => {
        cfg.gate_threshold_db = Number(thr.value);
        label.textContent = `${thr.value} dB`;
        placeMark();
        commit();
      },
    });
    const placeMark = () => {
      inBar.mark.hidden = !cfg.gate_on;
      inBar.mark.style.left = `${meterPct(cfg.gate_threshold_db)}%`;
    };
    thr.disabled = !cfg.gate_on;
    placeMark();

    const gateSwitch = h('input', {
      type: 'checkbox', checked: cfg.gate_on,
      onchange: (e) => { cfg.gate_on = e.target.checked; thr.disabled = !cfg.gate_on; placeMark(); commit(); },
    });
    const meterSwitch = h('input', {
      type: 'checkbox', checked: cfg.meters_on,
      onchange: (e) => {
        cfg.meters_on = e.target.checked;
        meters.hidden = !cfg.meters_on;
        if (cfg.meters_on) startMeters({ inFill: inBar.fill, outFill: outBar.fill, inDb: inBar.db, outDb: outBar.db, note });
        else stopMeters();
        commit();
      },
    });

    meters.hidden = !cfg.meters_on;
    if (cfg.meters_on) startMeters({ inFill: inBar.fill, outFill: outBar.fill, inDb: inBar.db, outDb: outBar.db, note });
    else stopMeters();

    return section('Noise gate & meters',
      row('Noise gate', 'Mutes your mic between words, so keys and fans do not reach Discord.', gateSwitch),
      row('Gate threshold', 'The gate opens when your voice is louder than this. Raise it if noise gets through.', h('div', { class: 'inline' }, thr, label)),
      row('Level meters', 'IN is your mic. OUT is what goes to Discord. Watch IN while you set the threshold.', meterSwitch),
      meters);
  };

  // Live mix: ducking (sounds get quieter while you speak) and the limiter. Each has a switch.
  const liveMixSection = () => {
    const duckSwitch = h('input', {
      type: 'checkbox', checked: cfg.duck_on,
      onchange: (e) => { cfg.duck_on = e.target.checked; commit(); },
    });
    const limiterSwitch = h('input', {
      type: 'checkbox', checked: cfg.limiter_on,
      onchange: (e) => { cfg.limiter_on = e.target.checked; commit(); },
    });
    return section('Live mix',
      row('Ducking', 'Sounds get quieter while you speak, so your friends hear you. Works on the output that carries your mic (Go Live). Needs the mic on.', duckSwitch),
      row('Limiter', 'Stops loud peaks from crackling in Discord. Applies to sounds and to your mic.', limiterSwitch));
  };

  // Phone remote: the Rustboard Remote Android app talks to this PC over Wi-Fi. Off by default.
  const remoteSection = () => {
    const box = h('div', { class: 'set-row col remote-box' });
    box.hidden = !cfg.remote_on;

    const paint = async () => {
      if (!cfg.remote_on) return;
      await sync(); // Rust must have the switch and the port before we ask
      try {
        const r = await call('remote_info');
        const qr = h('div', { class: 'remote-qr' });
        qr.innerHTML = r.qr; // our own SVG from Rust
        box.replaceChildren(
          h('div', { class: 'remote-wrap' },
            qr,
            h('div', { class: 'remote-txt' },
              h('div', { class: r.running ? 'ok' : 'warn' }, r.running ? 'Running' : 'Not running (see the message, try another port)'),
              h('div', { class: 'd' }, 'Open the Rustboard Remote app on your phone and press Scan QR. Or copy the link and paste it in the app. Phone and PC must use the same Wi-Fi.'),
              h('div', { class: 'remote-url' }, r.url),
              h('div', { class: 'inline' },
                h('button', { onclick: () => copy(r.url) }, icon('copy'), 'Copy link'),
                h('button', {
                  onclick: async () => {
                    try { cfg = await call('remote_new_token'); toast('New link made. The old link stops working.'); paint(); }
                    catch (e) { toast(e, true); }
                  },
                }, 'New link'))),
          ),
          h('div', { class: 'd' }, 'Anyone with this link can play your sounds. Keep it private. If Windows asks about network access, allow Private networks.'));
      } catch (e) {
        box.replaceChildren(h('div', { class: 'd' }, `Cannot read the link: ${e}`));
      }
    };

    const onoff = h('input', {
      type: 'checkbox', checked: cfg.remote_on,
      onchange: (e) => { cfg.remote_on = e.target.checked; box.hidden = !cfg.remote_on; commit(); paint(); },
    });
    const port = h('input', {
      type: 'number', min: 1024, max: 65535, value: cfg.remote_port, class: 'port',
      onchange: () => {
        const v = Math.round(Number(port.value));
        cfg.remote_port = v >= 1024 && v <= 65535 ? v : 8787;
        port.value = cfg.remote_port;
        commit(); paint();
      },
    });
    paint();
    return section('Phone remote',
      row('Phone remote', 'Control the soundboard from the Rustboard Remote Android app, like a Stream Deck.', onoff),
      row('Port', 'Change it only if another app already uses 8787.', port),
      box);
  };

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

    micGateSection(),

    liveMixSection(),

    remoteSection(),

    section('Sound shortcuts',
      shortcut('Pause all', ss.pause),
      shortcut('Continue', ss.continue),
      shortcut('Stop all', ss.stop)),
    h('div', { class: 'credit' },
      'Based on Rustboard by ',
      h('button', { class: 'credit-link', onclick: () => call('open_url', { url: REPO_URL }).catch(() => copy(REPO_URL)) }, 'same-09'),
      ' · MIT + credit'),
  ];

  $('#settings-body').replaceChildren(...body);
  $('#settings-body').scrollTop = 0;
  showView('settings');
}


/* ---------- Sound Effects (mic voice changer) ---------- */
// ids match src-tauri/src/fx.rs. Names and hints are only for the screen.
const FX = [
  ['clean', 'Clean', 'Your normal voice'],
  ['deep', 'Deep', 'Lower, bigger voice'],
  ['chipmunk', 'Chipmunk', 'Fast and squeaky'],
  ['demon', 'Demon', 'Very low, gritty, dark'],
  ['giant', 'Giant', 'Huge voice in a big hall'],
  ['robot', 'Robot', 'Metallic robot'],
  ['alien', 'Alien', 'Warbly sci-fi alien'],
  ['radio', 'Radio', 'Walkie-talkie, old radio'],
  ['megaphone', 'Megaphone', 'Loud, honky megaphone'],
  ['cave', 'Cave', 'Echoing cave'],
  ['8bit', '8-bit', 'Crunchy retro console'],
];

/* ---------- AI Voice: your voice becomes another voice, live ---------- */
// Rust (src-tauri/src/voice_ai.rs) checks the graphics card and keeps the imported voices.
const VOICE_SITE = 'https://voice-models.com/';

function aiVoiceSection() {
  let ai = null;     // { gpu: {name, vram_mb, level}, engine_ready }
  let voices = [];   // [{ id, name, size_mb, has_index }]
  const gpuLine = h('div', { class: 'fx-status' });
  const engineLine = h('div', { class: 'fx-status' });
  const runLine = h('div', { class: 'fx-status' });
  runLine.hidden = true;
  const live_ai = { state: 0, text: '' }; // what the engine does for the live mic
  // the install of the voice engine (Python, PyTorch, models), asked from Rust while it runs
  let inst = { running: false, done: false, step: 0, steps: 6, title: '', line: '', percent: 0, error: '' };
  let instTimer = null;
  let justDone = false; // the install finished while this page was open
  const instFill = h('i', { class: 'm-fill' });
  instFill.style.transition = 'width 400ms ease';
  const instText = h('div', { class: 'fx-status' });
  const instBar = h('div', { class: 'm-bar' }, instFill);
  const instRow = h('div', { class: 'set-row col' }, instBar, instText);
  instRow.hidden = true;
  const instDesc = h('div', { class: 'd' });
  const instBtns = h('div', { class: 'ctl' });
  const list = h('div', {});
  const strong = () => !!ai && ai.gpu.level === 'strong';

  const gpuText = () => {
    if (!ai) return 'Checking your graphics card…';
    const g = ai.gpu;
    if (g.level === 'none') return 'No graphics card found. AI Voice will not run well on this PC.';
    const mem = g.vram_mb ? ` · ${Math.round(g.vram_mb / 1024)} GB` : '';
    return g.level === 'strong'
      ? `${g.name}${mem} · Strong enough ✓`
      : `${g.name}${mem} · Not strong enough. AI Voice needs an NVIDIA card with 6 GB or more.`;
  };

  const paint = () => {
    gpuLine.textContent = gpuText();
    gpuLine.classList.toggle('warn', !!ai && !strong());
    if (cfg.ai_voice_on && ai && !strong()) {
      gpuLine.textContent += ' It is on anyway: expect lag and dropped frames.';
    }
    engineLine.hidden = !ai;
    engineLine.classList.toggle('warn', !!ai && !ai.engine_ready);
    engineLine.textContent = !ai ? '' : ai.engine_ready
      ? (ai.engine_own ? 'Voice engine: installed by Rustboard ✓' : `Voice engine: engine folder found ✓ (${ai.engine_dir})`)
      : `Voice engine: ${ai.engine_error}`;
    paintInstall();
    runLine.hidden = !(cfg.ai_voice_on && live_ai.state);
    runLine.classList.toggle('warn', live_ai.state === 3);
    runLine.textContent = live_ai.state === 1 ? live_ai.text
      : live_ai.state === 2 ? `Your voice is being changed live ✓ (${live_ai.text})`
        : live_ai.state === 3 ? `Voice engine problem: ${live_ai.text} Your mic is silent until this works.` : '';
    sw.checked = !!cfg.ai_voice_on;
    list.replaceChildren(...(voices.length
      ? voices.map((v) => {
        const sel = cfg.ai_voice === v.id;
        return h('div', { class: 'set-row' },
          h('div', { class: 'txt' },
            h('div', { class: 't' }, v.name),
            h('div', { class: 'd' }, `${v.size_mb} MB${v.has_index ? ' · with index' : ''}`)),
          h('div', { class: 'ctl' },
            h('button', { class: sel ? 'primary' : '', onclick: () => { cfg.ai_voice = sel ? '' : v.id; commit(); paint(); } }, sel ? 'Selected' : 'Use'),
            h('button', { onclick: () => removeVoice(v) }, 'Remove')));
      })
      : [h('div', { class: 'set-row' }, h('div', { class: 'fx-status' }, 'No voices yet. Get one on voice-models.com, then press Import voice.'))]));
    paintTest();
  };

  const paintInstall = () => {
    const ready = (!!ai && ai.engine_ready) || justDone; // just installed = ready, even before the status refresh
    const own = (!!ai && ai.engine_own) || justDone;
    const cuda = !!ai && ai.gpu.vram_mb > 0; // nvidia-smi found a card
    instRow.hidden = !(inst.running || inst.error); // nothing to show once it is installed
    instFill.style.width = `${inst.running ? inst.percent : 0}%`;
    instText.classList.toggle('warn', !!inst.error && inst.error !== 'Cancelled.');
    instText.textContent = inst.running
      ? `${inst.percent}% — ${inst.line || inst.title}`
      : inst.error ? (inst.error === 'Cancelled.' ? 'Cancelled. Press Install AI voice to continue where it stopped.' : `Install failed: ${inst.error}`)
        : '';
    instDesc.textContent = ready && own
      ? 'Installed by Rustboard in Documents/RustBoard/AI. Remove it if you want the space back (several GB). Your voices stay.'
      : `Rustboard installs the engine for you: its own Python, PyTorch and the base models, inside Documents/RustBoard/AI. About ${cuda ? '3.5' : '0.8'} GB to download, 5 to 15 minutes. No separate engine download and no .bat file needed.`;
    const btns = [];
    if (inst.running) {
      btns.push(h('button', { onclick: () => call('ai_install_cancel').catch(() => {}) }, 'Cancel'));
    } else if (ready && own) {
      btns.push(h('button', { onclick: removeEngine }, 'Remove engine'));
    } else {
      btns.push(h('button', { class: 'primary', onclick: startInstall }, ready ? 'Install AI voice instead' : 'Install AI voice'));
    }
    instBtns.replaceChildren(...btns);
  };

  const pollInstall = async () => {
    try {
      const was = inst.running;
      inst = await call('ai_install_status');
      if (was && !inst.running) {
        clearInterval(instTimer); instTimer = null;
        if (inst.done && cfg.ai_engine_dir) { cfg.ai_engine_dir = ''; commit(); await sync(); } // use the engine we just installed
        try { ai = await call('ai_status'); } catch (_) { /* keep the old status */ }
        justDone = !!inst.done;
        if (inst.done) toast('The AI voice engine is installed.');
      }
    } catch (_) { /* next tick */ }
    paint();
    if (!instRow.isConnected && instTimer) { clearInterval(instTimer); instTimer = null; }
  };

  const watchInstall = () => {
    if (!instTimer) instTimer = setInterval(pollInstall, 800);
  };

  const startInstall = async () => {
    try {
      await call('ai_install');
      inst = { running: true, done: false, step: 0, steps: 6, title: 'Starting', line: '', percent: 0, error: '' };
      justDone = false;
      watchInstall();
      paint();
    } catch (e) { toast(String(e), true); }
  };

  const removeEngine = () => openDialog({
    title: 'Remove the AI voice engine',
    body: [
      h('p', {}, 'This deletes the engine Rustboard installed (Python, PyTorch and the base models, several GB).'),
      h('p', {}, 'Your imported voices stay. You can install the engine again at any time.'),
    ],
    buttons: [{ label: 'Cancel' }, {
      label: 'Remove', primary: true,
      onclick: async () => {
        try {
          await call('ai_remove_engine');
          inst = { running: false, done: false, step: 0, steps: 6, title: '', line: '', percent: 0, error: '' };
          justDone = false;
          ai = await call('ai_status');
        } catch (e) { toast(String(e), true); }
        paint();
      },
    }],
  });

  const turnOn = () => { cfg.ai_voice_on = true; commit(); paint(); };

  const sw = h('input', {
    type: 'checkbox', checked: !!cfg.ai_voice_on,
    onchange: (e) => {
      if (!e.target.checked) { cfg.ai_voice_on = false; commit(); paint(); return; }
      if (strong()) return turnOn();
      e.target.checked = false; // not yet: ask first
      openDialog({
        title: 'Your graphics card may be too weak',
        body: [
          h('p', {}, ai && ai.gpu.name ? `Your card: ${ai.gpu.name}${ai.gpu.vram_mb ? ` (${Math.round(ai.gpu.vram_mb / 1024)} GB)` : ''}.` : 'No graphics card was found.'),
          h('p', {}, 'AI Voice works best on an NVIDIA card with 6 GB or more. On this PC it can:'),
          h('p', {}, '• make your voice stutter or cut out on Discord'),
          h('p', {}, '• lower the FPS of your game'),
          h('p', {}, '• make the PC hot and loud'),
          h('p', {}, 'Turn it on anyway?'),
        ],
        buttons: [{ label: 'Cancel' }, { label: 'Turn on anyway', primary: true, onclick: turnOn }],
      });
    },
  });

  const removeVoice = (v) => openDialog({
    title: 'Remove voice',
    body: h('p', {}, `Remove "${v.name}" from Rustboard? The file you downloaded stays where it is.`),
    buttons: [{ label: 'Cancel' }, {
      label: 'Remove', primary: true,
      onclick: async () => {
        try {
          await call('voice_remove', { id: v.id });
          voices = voices.filter((x) => x.id !== v.id);
          if (cfg.ai_voice === v.id) { cfg.ai_voice = ''; commit(); }
          paint();
        } catch (e) { toast(String(e), true); }
      },
    }],
  });

  const importVoice = async () => {
    const [f] = await pickFiles({ multiple: false, filters: [{ name: 'Voice model', extensions: ['zip', 'pth', 'onnx'] }] });
    if (!f) return;
    try {
      const v = await call('voice_import', { path: f });
      voices = await call('voice_list');
      cfg.ai_voice = v.id; commit();
      toast(`Imported "${v.name}".`);
      paint();
    } catch (e) { toast(String(e), true); }
  };

  const pickEngine = async () => {
    const dir = await pickFolder();
    if (!dir) return;
    cfg.ai_engine_dir = dir; commit();
    try { ai = await call('ai_status'); } catch (_) { /* keep the old status */ }
    paint();
  };

  const openSite = () => call('open_url', { url: VOICE_SITE })
    .catch(() => navigator.clipboard.writeText(VOICE_SITE).then(() => toast('Link copied.'), () => toast(VOICE_SITE)));

  // ---- Test voice: record a few seconds, hear yourself, hear the new voice, see the speed ----
  const TEST_SECONDS = 4;
  let testing = false;
  let testStep = '';
  const testHint = h('div', { class: 'd' });
  const testResult = h('div', { class: 'fx-status' });
  const clipBtns = h('div', { class: 'ctl' });
  const resRow = h('div', { class: 'set-row' }, h('div', { class: 'stack' }, testResult), clipBtns);
  resRow.hidden = true;
  const testBtn = h('button', { class: 'primary', onclick: () => runTest() }, 'Test voice');
  // "Cancel" shows only while the test runs (listening or changing the voice)
  const cancelBtn = h('button', { onclick: () => cancelTest() }, 'Cancel');
  cancelBtn.hidden = true;
  const haveVoice = () => voices.some((v) => v.id === cfg.ai_voice);

  const paintTest = () => {
    testBtn.disabled = testing || !haveVoice() || live;
    cancelBtn.hidden = !testing;
    testHint.textContent = testing ? testStep
      : live ? 'Stop Go Live first. The test plays on your Primary output and would reach Discord.'
        : !haveVoice() ? 'Pick a voice with Use first.'
          : `Speak for ${TEST_SECONDS} seconds. Then you hear your own voice and the new voice.`;
  };

  const playClip = (which, label) => h('button', {
    onclick: () => call('ai_test_play', { which }).catch((e) => toast(String(e), true)),
  }, icon('play'), label);

  const showTest = (r) => {
    clipBtns.replaceChildren(playClip('original', 'My voice'), ...(r.converted ? [playClip('converted', 'New voice')] : []));
    const said = {
      fast: 'Fast enough for live ✓',
      tight: 'Tight. Live may lag when your game is busy.',
      slow: 'Too slow for live. Expect lag and dropped frames.',
    };
    testResult.textContent = r.converted
      ? `Changed ${r.recorded_secs.toFixed(1)} s of voice in ${r.convert_secs.toFixed(1)} s (${r.speed.toFixed(2)}× real time). ${said[r.verdict] || ''}`
      : (r.error || 'The voice could not be changed.');
    testResult.classList.toggle('warn', !r.converted || r.verdict === 'slow');
    resRow.hidden = false;
  };

  const cancelTest = async () => {
    if (!testing) return;
    cancelBtn.disabled = true;
    testStep = 'Cancelling…';
    paintTest();
    try { await call('ai_test_cancel'); } catch (_) { /* the test ends by itself */ }
  };

  const runTest = async () => {
    if (testing) return;
    testing = true;
    resRow.hidden = true;
    let left = TEST_SECONDS;
    testStep = `Speak now… ${left}`;
    testBtn.textContent = 'Listening…';
    paintTest();
    const timer = setInterval(() => {
      left -= 1;
      if (left > 0) {
        testStep = `Speak now… ${left}`;
      } else {
        clearInterval(timer);
        testStep = 'Changing your voice…';
        testBtn.textContent = 'Working…';
      }
      paintTest();
    }, 1000);
    try {
      showTest(await call('ai_test', { voice: cfg.ai_voice }));
    } catch (e) {
      if (/cancel/i.test(String(e))) {
        toast('Test cancelled.'); // nothing to show: the clips are not kept
      } else {
        clipBtns.replaceChildren();
        testResult.textContent = String(e);
        testResult.classList.add('warn');
        resRow.hidden = false;
      }
    } finally {
      clearInterval(timer);
      testing = false;
      cancelBtn.disabled = false;
      testBtn.textContent = 'Test voice';
      paintTest();
    }
  };

  paint();
  call('ai_status').then((r) => { ai = r; paint(); }).catch(() => { ai = { gpu: { name: '', vram_mb: 0, level: 'none' }, engine_ready: false, engine_dir: '', engine_error: 'Could not check.' }; paint(); });
  call('voice_list').then((r) => { voices = r; paint(); }).catch(() => {});
  call('ai_install_status').then((r) => { inst = r; if (r.running) watchInstall(); paint(); }).catch(() => {});
  // while this page is open: ask the engine how it is doing (loading, live, failed)
  const poll = setInterval(async () => {
    if (!runLine.isConnected) { clearInterval(poll); return; }
    try {
      const l = await call('get_levels');
      if (l.ai_state !== live_ai.state || l.ai_text !== live_ai.text) {
        live_ai.state = l.ai_state; live_ai.text = l.ai_text; paint();
      }
    } catch (_) { /* next tick */ }
  }, 1000);

  return h('section', { class: 'set-section' },
    h('h4', {}, 'AI Voice'),
    h('div', { class: 'set-card' },
      h('div', { class: 'set-row' },
        h('div', { class: 'txt' },
          h('div', { class: 't' }, 'Change into another voice'),
          h('div', { class: 'd' }, 'Your voice turns into someone else’s, live. Needs a strong graphics card.')),
        h('div', { class: 'ctl' }, sw)),
      h('div', { class: 'set-row' }, h('div', { class: 'stack' }, gpuLine, engineLine, runLine)),
      h('div', { class: 'set-row' },
        h('div', { class: 'txt' }, h('div', { class: 't' }, 'AI voice engine'), instDesc),
        instBtns),
      instRow,
      h('div', { class: 'set-row' },
        h('div', { class: 'txt' },
          h('div', { class: 't' }, 'Already have an engine folder?'),
          h('div', { class: 'd' }, 'Optional. Pick the folder that contains the "rustboard" folder to use that engine instead. Auto = the engine Rustboard installed, or one in the usual places.')),
        h('div', { class: 'ctl' },
          h('button', { onclick: pickEngine }, 'Choose folder'),
          h('button', { onclick: () => { cfg.ai_engine_dir = ''; commit(); call('ai_status').then((r) => { ai = r; paint(); }); } }, 'Auto'))),
      list,
      h('div', { class: 'set-row' },
        h('div', { class: 'txt' }, h('div', { class: 't' }, 'Test voice'), testHint),
        h('div', { class: 'ctl' }, cancelBtn, testBtn)),
      resRow,
      h('div', { class: 'set-row' },
        h('div', { class: 'txt' }, h('div', { class: 'd' }, 'Download a voice from voice-models.com, then import the .zip here.')),
        h('div', { class: 'ctl' },
          h('button', { onclick: openSite }, 'voice-models.com'),
          h('button', { class: 'primary', onclick: importVoice }, 'Import voice'))),
      h('div', { class: 'set-row' },
        h('div', { class: 'fx-status' }, 'Only use a voice you have the right to use. Do not pretend to be a real person without their permission.'))));
}

let repaintFx = null; // redraw the Sound Effects page (a hotkey changed the effect)

function openFx() {
  const status = h('div', { class: 'fx-status' });
  const grid = h('div', { class: 'fx-grid' });

  const micReady = () => live || (cfg.mic_input !== 'Disabled' && cfg.mic_output !== 'Disabled');
  const paintStatus = () => {
    const warn = cfg.fx_on && !micReady();
    status.classList.toggle('warn', warn);
    status.textContent = !cfg.fx_on
      ? 'Effects are off: your mic goes through as it is.'
      : warn
        ? 'The mic is not running. Press Go Live, or pick a Mic input and a Mic output in Settings.'
        : cfg.fx_preset === 'clean'
          ? 'Pick an effect below.'
          : `Effect on: ${FX.find((f) => f[0] === cfg.fx_preset)[1]}. Everyone who hears your mic hears it.`;
  };

  // optional hotkey under each effect card: Bind / Clear (press it once = on, again = off)
  const keyRow = (id) => {
    const box = h('span', {});
    const paint = () => box.replaceChildren(keysBox((cfg.fx_keys && cfg.fx_keys[id]) || []));
    paint();
    return h('div', { class: 'fx-key' },
      box,
      h('button', {
        onclick: () => bindKeys((k) => {
          cfg.fx_keys = cfg.fx_keys || {};
          if (k.length) cfg.fx_keys[id] = k; else delete cfg.fx_keys[id];
          paint(); commit();
        }),
      }, 'Bind'),
      h('button', {
        onclick: () => { if (cfg.fx_keys) delete cfg.fx_keys[id]; paint(); commit(); },
      }, 'Clear'));
  };

  const paintGrid = () => {
    grid.classList.toggle('off', !cfg.fx_on);
    grid.replaceChildren(...FX.map(([id, name, desc]) => h('div', { class: 'fx-cell' },
      h('button', {
        class: `fx-card${cfg.fx_preset === id ? ' on' : ''}`,
        role: 'radio',
        'aria-checked': String(cfg.fx_preset === id),
        onclick: () => {
          cfg.fx_preset = id;
          if (id !== 'clean' && !cfg.fx_on) { cfg.fx_on = true; sw.checked = true; }
          commit();
          paintGrid();
          paintStatus();
        },
      }, h('span', { class: 't' }, name), h('span', { class: 'd' }, desc)),
      keyRow(id))));
  };

  const sw = h('input', {
    type: 'checkbox', checked: cfg.fx_on,
    onchange: (e) => { cfg.fx_on = e.target.checked; commit(); paintGrid(); paintStatus(); },
  });

  paintGrid();
  paintStatus();
  repaintFx = () => { sw.checked = cfg.fx_on; paintGrid(); paintStatus(); };

  $('#fx-body').replaceChildren(
    aiVoiceSection(),
    h('section', { class: 'set-section' },
      h('h4', {}, 'Mic effects'),
      h('div', { class: 'set-card' },
        h('div', { class: 'set-row' },
          h('div', { class: 'txt' },
            h('div', { class: 't' }, 'Sound effects'),
            h('div', { class: 'd' }, 'Change your voice live. It works on the mic that goes to Discord.')),
          h('div', { class: 'ctl' }, sw)),
        h('div', { class: 'set-row' }, status))),
    h('section', { class: 'set-section' },
      h('h4', {}, 'Effect'),
      h('div', { class: 'fx-status', style: 'margin: -4px 2px 8px' },
        'Effects for single sounds: Soundboard > edit a sound > Sound effect. Optional hotkey under each effect: press once = on, press again = off.'),
      h('div', { class: 'set-card', role: 'radiogroup', 'aria-label': 'Effect' }, grid)));
  $('#fx-body').scrollTop = 0;
  showView('fx');
}

/* ---------- render + wiring ---------- */

function render() {
  renderHeader();
  renderSounds();
}

function wire() {
  setIcon($('#preset-chev'), 'chev');
  for (const t of document.querySelectorAll('#tabs .rail-tab')) setIcon($('.ico', t), t.dataset.icon);
  setIcon($('#live-ico'), 'mic');
  setIcon($('#add-ico'), 'plus');
  setIcon($('#folder-ico'), 'folder');
  setIcon($('#sound-open'), 'folder');
  setIcon($('#sound-stop'), 'stop');
  setIcon($('#sound-pause'), 'pause');
  setIcon($('#play-ico'), 'play');

  $('#preset-btn').addEventListener('click', openPresetMenu);
  $('#open-home').addEventListener('click', () => showView('home'));
  for (const t of document.querySelectorAll('#tabs .rail-tab')) {
    t.addEventListener('click', () => { if (t.dataset.view !== view) openView(t.dataset.view); });
  }
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && view !== 'home' && !dlg.open && !bindDlg.open) showView('home');
  });
  $('#go-live').addEventListener('click', toggleLive);

  $('#sound-folder').addEventListener('click', importFolder);
  $('#sound-open').addEventListener('click', () => call('open_soundboard_folder').catch((e) => toast(e, true)));

  $('#sound-add').addEventListener('click', () => {
    const p = preset();
    const fresh = { name: '', files: [], keys: [], volume: 100, loop: false, fx: 'clean', image: '', error: null };
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

/* ---------- window controls (frameless window) ---------- */

const WIN_SVG = {
  max: '<svg viewBox="0 0 14 14"><rect x="2" y="2" width="10" height="10" rx="2"/></svg>',
  restore: '<svg viewBox="0 0 14 14"><rect x="2" y="4.25" width="7.75" height="7.75" rx="1.75"/><path d="M4.5 2H10.25C11.22 2 12 2.78 12 3.75V9.5"/></svg>',
};

function initWindowControls() {
  const box = $('#winctl');
  const win = isTauri ? window.__TAURI__.window?.getCurrentWindow?.() : null;
  if (!box || !win) return; // browser mock mode: no custom controls
  box.hidden = false;
  const max = $('#win-max');
  const sync = async () => {
    const m = await win.isMaximized().catch(() => false);
    max.innerHTML = m ? WIN_SVG.restore : WIN_SVG.max;
    max.setAttribute('aria-label', m ? 'Restore' : 'Maximize');
  };
  $('#win-min').onclick = () => win.minimize();
  max.onclick = () => win.toggleMaximize();
  $('#win-close').onclick = () => win.close();
  win.onResized(sync);
  sync();
}

async function init() {
  initWindowControls();
  wire();
  cfg = await call('get_state');
  lastVolume = cfg.volume || 100;
  render();
  renderLive();
  showView('home');
  startWave();
  refreshDevices().catch(() => {}); // warm the device list so Settings opens fast

  // hotkeys change the selection in Rust, then tell us
  listen('config-changed', (c) => { cfg = c; render(); if (view === 'fx' && repaintFx) repaintFx(); });
  listen('audio-error', (msg) => toast(msg, true));

  checkForUpdate();
}

init().catch((e) => toast(`Cannot reach backend: ${e}`, true));
