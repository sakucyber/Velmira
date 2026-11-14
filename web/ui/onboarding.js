import { el, store, THEMES, FONT_SIZE, normalizeFontSize, applyTheme, applyFontSize } from '../util/core.js';
import { shelfPreferences, saveShelfPreferences } from './library-tools.js';
import { localStore } from '../util/local-store.js';

const KEY = 'velmira.onboarding.v1';

async function completed() {
  try { if (localStorage.getItem(KEY) === '1') return true; } catch { /* use IndexedDB below */ }
  return (await localStore.meta(KEY).catch(() => false)) === true;
}

async function remember() {
  let saved = false;
  try { localStorage.setItem(KEY, '1'); saved = true; } catch { /* use IndexedDB below */ }
  try { await localStore.meta(KEY, true); saved = true; } catch { /* settings remain usable */ }
  return saved;
}

/** One optional screen before the local library; all preferences stay editable. */
export async function showOnboarding({ force = false } = {}) {
  if (!force && await completed()) return { changed: false };
  if (!document.querySelector('link[data-onboarding-style]')) {
    const style = el('link'); style.rel = 'stylesheet'; style.href = '/styles/onboarding.css'; style.dataset.onboardingStyle = 'true'; document.head.append(style);
  }
  const original = store.get(), shelf = shelfPreferences();
  let theme = THEMES.some(item => item.id === original.theme) ? original.theme : 'day';
  let fontSize = normalizeFontSize(original.fontSize), density = shelf.density === 'compact' ? 'compact' : 'standard';
  const previousFocus = document.activeElement, app = document.getElementById('app');
  const overlay = el('div', 'onboarding-overlay');
  const dialog = el('section', 'onboarding'); dialog.setAttribute('role', 'dialog'); dialog.setAttribute('aria-modal', 'true'); dialog.setAttribute('aria-labelledby', 'onboarding-title');
  dialog.tabIndex = -1;
  const wordmark = el('div', 'onboarding-brand', 'Velmira');
  const title = el('h1', null, force ? '让阅读更合心意' : '你的书，随时读'); title.id = 'onboarding-title';
  dialog.append(wordmark, title, el('p', 'onboarding-intro', '选一个舒服的阅读方式，就可以导入手机里的书。以后随时能在设置中调整。'));
  const choices = el('div', 'onboarding-themes'); choices.setAttribute('role', 'group'); choices.setAttribute('aria-label', '选择阅读主题');
  const preview = el('div', 'onboarding-preview'); preview.setAttribute('aria-label', '阅读效果预览');
  preview.append(el('strong', null, '翻开新的一页'), el('p', null, '窗边的光落在书页上，故事从这里开始。'));
  const updatePreview = () => {
    preview.dataset.theme = theme; preview.style.fontSize = fontSize + 'px';
    for (const button of choices.children) button.setAttribute('aria-pressed', String(button.dataset.theme === theme));
  };
  for (const item of THEMES) {
    const button = el('button', 'onboarding-theme', item.name); button.type = 'button'; button.dataset.theme = item.id;
    button.setAttribute('aria-label', item.name + '主题');
    const swatch = el('i'); swatch.style.background = item.swatch; button.prepend(swatch);
    button.addEventListener('click', () => { theme = item.id; updatePreview(); }); choices.append(button);
  }
  dialog.append(el('h2', null, '阅读主题'), choices);
  const font = el('label', 'onboarding-font'), fontText = el('span', null, '文字大小'), fontValue = el('output', null, fontSize + ' px');
  const range = el('input'); range.type = 'range'; range.min = FONT_SIZE.min; range.max = FONT_SIZE.max; range.step = 1; range.value = fontSize;
  range.setAttribute('aria-label', '首次阅读字号');
  range.addEventListener('input', () => { fontSize = normalizeFontSize(range.value); fontValue.textContent = fontSize + ' px'; updatePreview(); });
  font.append(fontText, fontValue, range); dialog.append(font, preview);
  const densityLabel = el('label', 'onboarding-density', '书架显示'), densitySelect = el('select'); densitySelect.setAttribute('aria-label', '首次书架密度');
  for (const [value, label] of [['standard', '舒展'], ['compact', '紧凑']]) { const option = el('option', null, label); option.value = value; densitySelect.append(option); }
  densitySelect.value = density; densitySelect.addEventListener('change', () => { density = densitySelect.value; }); densityLabel.append(densitySelect); dialog.append(densityLabel);
  const actions = el('div', 'onboarding-actions'), start = el('button', 'btn btn-primary', '开始阅读'), skip = el('button', 'btn', force ? '保留原设置' : '跳过，直接进入');
  start.type = skip.type = 'button'; actions.append(start, skip); dialog.append(actions, el('p', 'onboarding-note', '书籍和阅读记录保存在本机，断网也能使用。'));
  const status = el('p', 'onboarding-status'); status.setAttribute('role', 'status'); dialog.append(status);
  overlay.append(dialog); document.body.append(overlay); if (app) app.inert = true;
  updatePreview(); start.focus();
  return new Promise(resolve => {
    let closing = false;
    const finish = async changed => {
      if (closing) return; closing = true; start.disabled = skip.disabled = true;
      if (changed) {
        store.set({ theme, fontSize }); saveShelfPreferences({ ...shelf, density }); applyTheme(theme); applyFontSize(fontSize);
      }
      await remember();
      overlay.remove(); if (app) app.inert = false; document.removeEventListener('keydown', onKey);
      if (previousFocus?.isConnected) previousFocus.focus(); resolve({ changed });
    };
    const onKey = event => {
      if (event.key === 'Escape') { event.preventDefault(); finish(false); }
      if (event.key !== 'Tab') return;
      const items = [...dialog.querySelectorAll('button,input,select')].filter(item => !item.disabled);
      const first = items[0], last = items.at(-1);
      if (event.shiftKey && (document.activeElement === first || !dialog.contains(document.activeElement))) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && (document.activeElement === last || !dialog.contains(document.activeElement))) { event.preventDefault(); first.focus(); }
    };
    start.addEventListener('click', () => finish(true)); skip.addEventListener('click', () => finish(false)); document.addEventListener('keydown', onKey);
  });
}
