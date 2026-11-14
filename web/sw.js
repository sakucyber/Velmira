/* Static shell cache only. Private API responses, books and media stay in the
   device LocalStore so cookies and credentials are never put in CacheStorage. */
const VERSION = 'velmira-shell-v6';
const SHELL = [
  './', './index.html', './app.js', './manifest.webmanifest',
  './styles/main.css', './util/core.js', './ui/library.js', './ui/reader.js',
  './util/local-store.js', './util/local-media.js', './ui/paging.js', './ui/panels.js',
  './ui/offline-settings.js', './ui/annotations.js', './ui/library-tools.js', './ui/comic.js',
  './ui/onboarding.js', './styles/onboarding.css',
  './ui/mobile-ai.js', './styles/mobile-ai.css',
  './ui/reader-comics.js', './ui/automatic.js', './ui/book-analysis.js', './ui/manga.js',
  './ui/reading-sync.js', './styles/reading-sync.css',
  './ui/manga-pages.js', './ui/single-page.js', './ui/model-library.js', './ui/model-routing.js', './ui/ai-settings.js',
  './ui/security-settings.js', './ui/storyboards.js', './styles/ai-settings.css',
  './styles/model-library.css', './styles/comic.css', './icons/icon-192.png', './icons/icon-512.png',
];

self.addEventListener('install', event => {
  event.waitUntil(caches.open(VERSION).then(cache => cache.addAll(SHELL)).then(() => self.skipWaiting()));
});

self.addEventListener('activate', event => {
  event.waitUntil(caches.keys().then(keys => Promise.all(keys.filter(key => key.startsWith('velmira-shell-') && key !== VERSION).map(key => caches.delete(key))))
    .then(() => self.clients.claim()));
});

self.addEventListener('fetch', event => {
  const request = event.request;
  if (request.method !== 'GET' || new URL(request.url).origin !== self.location.origin) return;
  const url = new URL(request.url);
  if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/media/')) return;
  const navigation = request.mode === 'navigate' && (url.pathname === '/' || url.pathname === '/index.html');
  const staticFile = /^\/(?:app\.js|index\.html|manifest\.webmanifest|(?:ui|util)\/[^/]+\.js|styles\/[^/]+\.css|icons\/icon-(?:192|512)\.png)$/.test(url.pathname);
  if (!navigation && !staticFile) return;
  event.respondWith(fetch(request).then(response => {
    if (response.ok) {
      const copy = response.clone(); event.waitUntil(caches.open(VERSION).then(cache => cache.put(navigation ? './index.html' : request, copy)));
    }
    return response;
  }).catch(async () => {
    const cached = await caches.match(navigation ? './index.html' : request);
    return cached || new Response('离线界面资源尚未下载', { status: 503, headers: { 'Content-Type': 'text/plain; charset=utf-8' } });
  }));
});
