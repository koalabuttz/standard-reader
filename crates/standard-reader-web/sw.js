/*
 * App-shell cache for standard-reader-web.
 *
 * OPFS owns reader data; this service worker only makes the generated HTML/JS/WASM bootable
 * offline. Its scope is /standard-reader/app/, so it never intercepts the surrounding website,
 * OAuth metadata, or API traffic.
 */
const CACHE_PREFIX = "standard-reader-shell-";
const CACHE_NAME = `${CACHE_PREFIX}v3`;

function inAppScope(url) {
  const scope = new URL(self.registration.scope);
  return url.origin === scope.origin && url.pathname.startsWith(scope.pathname);
}

async function discoverShellAssets(indexResponse) {
  const html = await indexResponse.text();
  const urls = new Set([self.registration.scope]);
  const generatedAsset = /["']([^"']+\.(?:css|js|wasm)(?:\?[^"']*)?)["']/g;

  for (const match of html.matchAll(generatedAsset)) {
    const url = new URL(match[1], self.registration.scope);
    if (inAppScope(url)) {
      urls.add(url.href);
    }
  }

  return [...urls];
}

async function primeShell() {
  const indexRequest = new Request(self.registration.scope, { cache: "reload" });
  const indexResponse = await fetch(indexRequest);
  if (!indexResponse.ok) {
    throw new Error(`app shell returned HTTP ${indexResponse.status}`);
  }

  const assets = await discoverShellAssets(indexResponse.clone());
  const cache = await caches.open(CACHE_NAME);
  await cache.put(self.registration.scope, indexResponse);

  // wasm-bindgen may emit small JS modules under `snippets/` (browser OAuth's Web Lock helper is
  // one). Follow static JS imports as well as HTML assets so a first online load remains bootable
  // offline even when the generated entry module has dependencies.
  const pending = assets.filter((url) => url !== self.registration.scope);
  const seen = new Set([self.registration.scope]);
  const staticImport = /(?:from\s*|import\s*)["']([^"']+\.js)["']/g;
  while (pending.length > 0) {
    const url = pending.shift();
    if (seen.has(url)) continue;
    seen.add(url);

    const response = await fetch(new Request(url, { cache: "reload" }));
    if (!response.ok) {
      throw new Error(`${url} returned HTTP ${response.status}`);
    }
    await cache.put(url, response.clone());

    if (new URL(url).pathname.endsWith(".js")) {
      const source = await response.text();
      for (const match of source.matchAll(staticImport)) {
        const dependency = new URL(match[1], url);
        if (inAppScope(dependency) && !seen.has(dependency.href)) {
          pending.push(dependency.href);
        }
      }
    }
  }
}

self.addEventListener("install", (event) => {
  event.waitUntil(
    primeShell().then(() => {
      self.skipWaiting();
    }),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    Promise.all([
      caches.keys().then((names) =>
        Promise.all(
          names
            .filter((name) => name.startsWith(CACHE_PREFIX) && name !== CACHE_NAME)
            .map((name) => caches.delete(name)),
        ),
      ),
      self.clients.claim(),
    ]),
  );
});

async function networkFirst(request) {
  const cache = await caches.open(CACHE_NAME);
  try {
    const response = await fetch(request);
    if (response.ok) {
      await cache.put(self.registration.scope, response.clone());
    }
    return response;
  } catch (error) {
    const cached = await cache.match(self.registration.scope);
    if (cached) {
      return cached;
    }
    throw error;
  }
}

async function cacheFirst(request) {
  const cache = await caches.open(CACHE_NAME);
  const cached = await cache.match(request);
  if (cached) {
    return cached;
  }

  const response = await fetch(request);
  if (response.ok) {
    await cache.put(request, response.clone());
  }
  return response;
}

self.addEventListener("fetch", (event) => {
  const { request } = event;
  const url = new URL(request.url);
  if (request.method !== "GET" || !inAppScope(url)) {
    return;
  }

  event.respondWith(request.mode === "navigate" ? networkFirst(request) : cacheFirst(request));
});
