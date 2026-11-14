/* Pure mapping checks for the synchronized reader.  These run without a
 * browser and protect code-point (rather than UTF-16) offsets and half-open
 * panel ranges used by manual, auto and speech modes. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

let source = fs.readFileSync(new URL('../web/ui/reading-sync.js', import.meta.url), 'utf8');
source = source.replace("import { el, apiGet, clamp, toast } from '../util/core.js';", 'const clamp=(n,a,b)=>n<a?a:n>b?b:n; const el=()=>{}; const apiGet=async()=>({}); const toast=()=>{};');
source = source.replace("const CSS_HREF = new URL('../styles/reading-sync.css', import.meta.url).href;", "const CSS_HREF = '';" );
source = source.replace(/export (async function|function)/g, '$1');
source += '\nthis.__sync={chapterCodepointOffsets,positionForCodepoint,normalizeReadingMap,panelRange};';
const context = vm.createContext({ URL, document: {}, window: {}, setTimeout, clearTimeout });
vm.runInContext(source, context);
const actual = (value) => JSON.parse(JSON.stringify(value));
const { chapterCodepointOffsets, positionForCodepoint, normalizeReadingMap, panelRange } = context.__sync;

const paragraphs = ['甲😀乙', '丙丁'];
assert.deepEqual(actual(chapterCodepointOffsets(paragraphs)), { offsets: [0, 4], length: 6 });
assert.deepEqual(actual(positionForCodepoint(3, paragraphs)), { para: 0, offset: 3 });
assert.deepEqual(actual(positionForCodepoint(4, paragraphs)), { para: 1, offset: 0 });
assert.deepEqual(actual(panelRange({ start: 1, end: 4 }, paragraphs)), { start: 1, end: 4 });
assert.deepEqual(actual(panelRange({ start: 4, end: 4 }, paragraphs)), { start: 4, end: 5 });
const map = normalizeReadingMap({ panels: [{ chapter_id: 2, start_offset: 4, end_offset: 8, url: '/panel.png' }] });
assert.equal(map[0].chapter, 2); assert.equal(map[0].start, 4); assert.equal(map[0].end, 8); assert.equal(map[0].url, '/panel.png');
console.log('reading_sync mapping: PASS');

// Exercise the same production controller with a small DOM and speech stub.
// This verifies behavior that static syntax checks cannot establish: a map
// arriving after mount, manual active-panel positioning, speech boundaries,
// pause/resume and a mode switch preserving the current character cursor.
class Element {
  constructor(tag) { this.tagName = tag.toUpperCase(); this.children = []; this.dataset = {}; this.style = {}; this.handlers = {}; this.classes = new Set(); this.className = ''; this.textContent = ''; this.hidden = false; this.offsetTop = 0; this.offsetHeight = 200; this.clientHeight = 300; this.scrollHeight = 900; this.scrollTop = 0; }
  append(...nodes) { for (const node of nodes) { node.parentElement = this; node.offsetTop = this.children.length * 250; this.children.push(node); } }
  appendChild(node) { this.append(node); return node; }
  prepend(...nodes) { this.children.unshift(...nodes); for (const node of nodes) node.parentElement = this; }
  replaceChildren(...nodes) { this.children = []; this.append(...nodes); }
  setAttribute(name, value) { this[name] = value; }
  addEventListener(name, fn) { (this.handlers[name] ||= []).push(fn); }
  async fire(name) { for (const fn of this.handlers[name] || []) await fn({}); await flush(); }
  remove() { if (this.parentElement) this.parentElement.children = this.parentElement.children.filter(x => x !== this); }
  scrollIntoView() { throw new Error('must scroll pane rather than ancestor page'); }
  get classList() { return { toggle: (name, value) => { if (value ?? !this.classes.has(name)) this.classes.add(name); else this.classes.delete(name); }, add: name => this.classes.add(name), remove: name => this.classes.delete(name) }; }
}
const flush = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
const nodes = [];
const makeElement = (tag, cls, text) => { const node = new Element(tag); node.className = cls || ''; if (text !== undefined) node.textContent = String(text); nodes.push(node); return node; };
const documentStub = { head: makeElement('head'), querySelector: () => null, createElement: tag => makeElement(tag) };
let response = { panels: [] };
const spoken = [];
const speechStub = { paused: false, cancel() { this.cancelled = true; }, speak(u) { spoken.push(u); this.paused = false; }, pause() { this.paused = true; }, resume() { this.paused = false; } };
let timerId = 0; const timers = new Map();
const testContext = vm.createContext({
  URL, document: documentStub, window: { speechSynthesis: speechStub }, speechSynthesis: speechStub,
  SpeechSynthesisUtterance: class { constructor(text) { this.text = text; } },
  __el: makeElement, __api: async () => response,
  setTimeout(fn) { timers.set(++timerId, fn); return timerId; }, clearTimeout(id) { timers.delete(id); },
});
let controllerSource = source.replace('const el=()=>{}; const apiGet=async()=>({});', 'const el=__el; const apiGet=__api;');
controllerSource += '\nthis.__mount=mountReadingSync;';
vm.runInContext(controllerSource, testContext);
const main = makeElement('main'); const foot = makeElement('footer');
const chapters = [['甲😀乙。', '丙丁。'], ['新章。']];
let readingState = { chapter: 0, para: 0, offset: 0, chapterCount: 2 };
let controller;
let navigationCalls = 0; let navigationGate = null;
const navigate = async position => {
  navigationCalls++;
  if (navigationGate) await navigationGate;
  readingState = { ...readingState, ...position }; controller?.update(readingState);
};
controller = await testContext.__mount({ bookId: 'test', main, foot, pager: {}, getState: () => readingState, getParagraphs: () => chapters[readingState.chapter], navigate, onTextProgress: navigate });
await flush();
assert.ok(controller, 'controls mount before comics exist');
response = { panels: [{ chapter: 0, start: 0, end: 5, url: '/one.png' }, { chapter: 0, start: 5, end: 8, url: '/two.png' }] };
await controller.refresh(); await flush();
let panels = nodes.filter(x => x.className.includes('reading-sync-panel') && x.parentElement?.children.includes(x));
assert.equal(panels.length, 2, 'late map becomes visible');
await navigate({ chapter: 0, para: 1, offset: 0 });
assert.ok(panels[1].classes.has('is-active'));
assert.ok(main.children[0].scrollTop > 0, 'manual reading scrolls its own pane rather than the document');
const button = text => nodes.find(x => x.tagName === 'BUTTON' && x.textContent === text);
await navigate({ chapter: 0, para: 0, offset: 0 });
await button('语音朗读').fire('click');
assert.equal(spoken.at(-1).text, '甲😀乙。');
spoken.at(-1).onboundary({ charIndex: 3 }); await flush();
assert.equal(readingState.offset, 2, 'speech UTF-16 boundary is converted to codepoints');
await button('暂停').fire('click'); assert.equal(speechStub.paused, true);
const speedControl = nodes.find(x => x.tagName === 'SELECT');
speedControl.value = '1.5'; await speedControl.fire('change');
assert.equal(speechStub.paused, true, 'changing speed while paused keeps speech paused');
assert.equal(spoken.at(-1).text, '乙。', 'changing speed resumes from the current codepoint instead of rereading the sentence');
await button('继续').fire('click'); assert.equal(speechStub.paused, false);
spoken.at(-1).onerror({ error: 'synthesis-failed' }); await flush();
assert.equal(controller.mode(), 'manual', 'speech errors release the reading mode');
await button('语音朗读').fire('click');
const cancelledUtterance = spoken.at(-1);
await button('自动播放').fire('click'); await flush();
assert.equal(controller.mode(), 'auto'); assert.ok(readingState.para > 0 || readingState.offset >= 2, 'mode switching retains reading position');
const callsAfterStart = navigationCalls;
await button('自动播放').fire('click'); assert.equal(navigationCalls, callsAfterStart, 'repeated play clicks do not start concurrent loops');
await button('暂停').fire('click');
assert.equal(timers.size, 0, 'pausing cancels the next automatic step');
cancelledUtterance.onend(); await flush();
assert.equal(controller.mode(), 'auto', 'cancelled speech completion cannot switch the active mode');
await button('手动').fire('click');
delete testContext.window.speechSynthesis;
await button('语音朗读').fire('click');
assert.equal(controller.mode(), 'manual', 'unsupported speech falls back to manual reading');
testContext.window.speechSynthesis = speechStub;

// A paused or replaced loop must not schedule a stale follow-up after an
// in-flight navigation resolves.
let releaseNavigation;
navigationGate = new Promise(resolve => { releaseNavigation = resolve; });
await button('自动播放').fire('click'); await flush();
await button('暂停').fire('click');
releaseNavigation(); navigationGate = null; await flush();
assert.equal(timers.size, 0, 'a paused in-flight step does not schedule another step');
await button('手动').fire('click');
controller.destroy(); assert.equal(main.children.length, 0); assert.equal(foot.children.length, 0);
console.log('reading_sync controller: PASS');

// Browser scroll events arrive after a programmatic jump.  The visible line
// can begin before the spoken character, so that event must retain the exact
// logical anchor until the user actually scrolls to another position.
const pagerTimers = new Map(); let pagerTimerId = 0;
const viewport = { scrollTop: 40, clientHeight: 200, scrollHeight: 1200, addEventListener() {} };
const column = { classList: { remove() {} } };
let pagerSource = fs.readFileSync(new URL('../web/ui/paging.js', import.meta.url), 'utf8')
  .replace("import { clamp } from '../util/core.js';", 'const clamp=(n,a,b)=>n<a?a:n>b?b:n;')
  .replace("import { resolveImage } from '../util/local-media.js';", '')
  .replace(/export /g, '') + '\nthis.__Pager=Pager;';
const pagerContext = vm.createContext({
  setTimeout(fn) { pagerTimers.set(++pagerTimerId, fn); return pagerTimerId; },
  clearTimeout(id) { pagerTimers.delete(id); },
});
vm.runInContext(pagerSource, pagerContext);
const changes = [];
const realPager = new pagerContext.__Pager(viewport, column, { onChange: info => changes.push(actual(info)) });
realPager.paragraphs = ['甲乙丙丁戊己。']; realPager.chunks = [{ start: 0, end: 0 }]; realPager.vertical = true;
realPager._scrollToPara = () => {};
realPager._restoreOffset = () => { viewport.scrollTop = 40; };
realPager.locatePara = () => 0; realPager.locateOffset = () => 0;
await realPager.gotoPara(0, 4);
assert.equal(changes.at(-1).offset, 4);
realPager._onScroll();
for (const [id, fn] of pagerTimers) { pagerTimers.delete(id); fn(); }
assert.equal(changes.at(-1).offset, 4, 'native programmatic scroll keeps an exact character anchor');
viewport.scrollTop = 80; realPager.locateOffset = () => 6; realPager._onScroll();
for (const [id, fn] of pagerTimers) { pagerTimers.delete(id); fn(); }
assert.equal(changes.at(-1).offset, 6, 'manual viewport scrolling updates the logical anchor');
console.log('reading_sync pager anchors: PASS');
