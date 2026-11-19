// Bridge to the Rust backend.
// Inside Tauri: window.__TAURI__ (withGlobalTauri = true).
// In a plain browser (python tools/dev_server.py): talks to a mock backend over HTTP.

const T = window.__TAURI__;
export const isTauri = Boolean(T && T.core);

export async function call(cmd, args = {}) {
  if (isTauri) return T.core.invoke(cmd, args);

  const res = await fetch('/api/' + cmd, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(args),
  });
  if (!res.ok) throw new Error(await res.text());
  const text = await res.text();
  return text ? JSON.parse(text) : null;
}

// listen('config-changed', payload => ...)
export function listen(event, cb) {
  if (isTauri) return T.event.listen(event, (e) => cb(e.payload));
  return Promise.resolve(() => {}); // mock mode has no push events
}

// Returns an array of absolute paths (empty if the user cancels).
export async function pickFiles({ multiple = false, filters = [] } = {}) {
  if (isTauri) {
    const r = await T.dialog.open({ multiple, filters });
    if (r == null) return [];
    return Array.isArray(r) ? r : [r];
  }
  const text = window.prompt('Mock mode (no file dialog). Enter path(s), one per line:');
  if (!text) return [];
  return text.split('\n').map((s) => s.trim()).filter(Boolean);
}
