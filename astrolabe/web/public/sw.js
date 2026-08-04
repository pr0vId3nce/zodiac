// Minimal service worker: cache-first for hashed assets, network-first for
// the shell. Only ever registered in a secure context (https / localhost).
const CACHE = "astrolabe-v1";

self.addEventListener("install", (e) => self.skipWaiting());
self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim())
  );
});

self.addEventListener("fetch", (e) => {
  const url = new URL(e.request.url);
  if (e.request.method !== "GET" || url.pathname === "/ws" || url.pathname === "/healthz") return;
  const hashed = url.pathname.startsWith("/assets/");
  e.respondWith(
    hashed
      ? caches.open(CACHE).then(async (c) => {
          const hit = await c.match(e.request);
          if (hit) return hit;
          const res = await fetch(e.request);
          if (res.ok) c.put(e.request, res.clone());
          return res;
        })
      : fetch(e.request).catch(() => caches.match(e.request).then((m) => m || caches.match("/")))
  );
});
