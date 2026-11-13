import { el, apiGet } from '../util/core.js';

// The current chapter only: bounded image DOM and independent scrolling.
export function mountReaderComics({ main, bookId, signal, onLayout }) {
  const root = el('aside', 'reader-comics'); root.hidden = true;
  root.setAttribute('aria-label', '本章漫画'); root.tabIndex = 0;
  const head = el('div', 'reader-comics-head');
  const title = el('h2', null, '本章漫画');
  head.appendChild(title);
  const note = el('p', 'manga-note', '自动生成草稿 · 可独立滚动查看');
  const gallery = el('div', 'reader-comics-pages');
  const more = el('button', 'btn', '加载更多本章漫画'); more.type = 'button'; more.hidden = true;
  root.append(head, note, gallery, more); main.appendChild(root);
  let key = '', chapter = -1, run = '', offset = 0, version = 0, loading = false, hasMore = false;
  async function load(reset = false) {
    const token = ++version;
    loading = true; more.disabled = true;
    try {
      const data = await apiGet('/api/books/' + encodeURIComponent(bookId) + '/manga/auto/pages?chapter=' + chapter + '&run_id=' + encodeURIComponent(run) + '&offset=' + (reset ? 0 : offset) + '&limit=12', signal);
      if (signal.aborted || token !== version) return;
      if (reset) { gallery.replaceChildren(); offset = 0; root.scrollTop = 0; }
      gallery.querySelector('.reader-comics-empty')?.remove();
      for (const page of data.pages) {
        const card = el('figure', 'reader-comic-page');
        const image = el('img'); image.src = page.url; image.loading = 'lazy'; image.alt = page.title + '，漫画第 ' + (page.ordinal + 1) + ' 页';
        card.append(image, el('figcaption', 'manga-note', '第 ' + (page.ordinal + 1) + ' 页 · ' + (page.error ? '需要复核：' + page.error : '草稿' + (page.pending_review ? ' · ' + page.pending_review + ' 格待复核' : ''))));
        gallery.appendChild(card);
      }
      offset += data.pages.length; hasMore = data.has_more; more.hidden = !hasMore;
      if (!offset) gallery.appendChild(el('p', 'manga-note reader-comics-empty', '本章漫画尚未生成。其他章节可在“漫画进度”中查看。'));
    } catch (err) { if (!signal.aborted && token === version) { key = ''; note.textContent = '漫画加载失败，正在重试：' + err.message; } }
    finally { if (token === version) { loading = false; more.disabled = false; } }
  }
  more.addEventListener('click', () => { if (!loading) load(); });
  return {
    update(value, currentChapter) {
      const visible = value.pages > 0;
      if (root.hidden === visible) {
        root.hidden = !visible; main.classList.toggle('with-comics', visible); onLayout();
      }
      if (!visible) { ++version; key = ''; gallery.replaceChildren(); return; }
      const next = [currentChapter, value.run_id, value.pages].join(':');
      if (next === key) return;
      const reset = chapter !== currentChapter || run !== value.run_id || !key;
      if (loading && !reset) return;
      key = next; chapter = currentChapter; run = value.run_id;
      title.textContent = '第 ' + (chapter + 1) + ' 章 · 漫画';
      note.textContent = '自动生成草稿 · 可独立滚动查看';
      if (reset || !hasMore) load(reset);
    },
  };
}
