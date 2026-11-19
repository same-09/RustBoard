// Key names follow the Rust `rdev::Key` names: "KeyA", "Num1", "Kp1", "F5", "ControlLeft"...

const SPECIAL = {
  ControlLeft: 'L Ctrl', ControlRight: 'R Ctrl',
  ShiftLeft: 'L Shift', ShiftRight: 'R Shift',
  Alt: 'Alt', AltGr: 'Alt Gr',
  MetaLeft: 'L Win', MetaRight: 'R Win',
  Return: 'Enter', Space: 'Space', Escape: 'Esc', Backspace: 'Backspace', Tab: 'Tab',
  UpArrow: '↑', DownArrow: '↓', LeftArrow: '←', RightArrow: '→',
  PageUp: 'Page Up', PageDown: 'Page Down', Home: 'Home', End: 'End',
  Insert: 'Insert', Delete: 'Delete',
  CapsLock: 'Caps Lock', PrintScreen: 'Print Screen', ScrollLock: 'Scroll Lock',
  NumLock: 'Num Lock', Pause: 'Pause',
  BackQuote: '`', Minus: '-', Equal: '=', LeftBracket: '[', RightBracket: ']',
  SemiColon: ';', Quote: "'", BackSlash: '\\', IntlBackslash: '\\',
  Comma: ',', Dot: '.', Slash: '/',
  KpReturn: 'Num Enter', KpMinus: 'Num -', KpPlus: 'Num +',
  KpMultiply: 'Num *', KpDivide: 'Num /', KpDelete: 'Num .',
};

export function prettyKey(name) {
  if (SPECIAL[name]) return SPECIAL[name];
  let m;
  if ((m = /^Key([A-Z])$/.exec(name))) return m[1];
  if ((m = /^Num(\d)$/.exec(name))) return m[1];
  if ((m = /^Kp(\d)$/.exec(name))) return 'Num ' + m[1];
  return name;
}

// Only used in browser mock mode (in Tauri the Rust hook gives the names).
const CODE_MAP = {
  Enter: 'Return', ArrowUp: 'UpArrow', ArrowDown: 'DownArrow', ArrowLeft: 'LeftArrow', ArrowRight: 'RightArrow',
  AltLeft: 'Alt', AltRight: 'AltGr', Backquote: 'BackQuote', Semicolon: 'SemiColon', Period: 'Dot',
  BracketLeft: 'LeftBracket', BracketRight: 'RightBracket', Backslash: 'BackSlash',
  NumpadAdd: 'KpPlus', NumpadSubtract: 'KpMinus', NumpadMultiply: 'KpMultiply',
  NumpadDivide: 'KpDivide', NumpadEnter: 'KpReturn', NumpadDecimal: 'KpDelete',
};

export function codeToName(code) {
  if (CODE_MAP[code]) return CODE_MAP[code];
  let m;
  if ((m = /^Digit(\d)$/.exec(code))) return 'Num' + m[1];
  if ((m = /^Numpad(\d)$/.exec(code))) return 'Kp' + m[1];
  return code; // KeyA, F5, Space, ControlLeft, ShiftLeft, ...
}
