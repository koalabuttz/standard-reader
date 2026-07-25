# Web deployment

The browser shell is served by the existing `koalabuttz.github.io` GitHub Pages repository at:

```text
https://www.davidlewis.xyz/standard-reader/app/
```

The parent `/standard-reader/` path remains the human-facing project page and hosts separate
`client_metadata.json` (desktop/native) and `web_client_metadata.json` (browser) OAuth clients.
The static website stays build-free: this repository builds the WASM application, and only the
finished files plus browser metadata are copied into it.

## Stage a release

From the `standard-reader` repository:

```sh
./scripts/stage-web-release.sh
```

The script:

1. runs the pinned Trunk release build;
2. verifies that the generated HTML uses `/standard-reader/app/`;
3. replaces only `standard-reader/app/` and stages `web_client_metadata.json` in the sibling
   `website` checkout; and
4. prints the website repository changes for review.

It does not commit or push either repository. Pass another website checkout as the first argument
when it is not available at `../website`.

## Cloudflare response headers

WASM threads require `SharedArrayBuffer`, so the production document must be cross-origin
isolated. GitHub Pages cannot configure these response headers; add a Cloudflare Response Header
Transform Rule with this expression:

```text
http.host eq "www.davidlewis.xyz"
and starts_with(http.request.uri.path, "/standard-reader/app/")
```

Set these static response headers:

```text
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: credentialless
```

Do not apply the rule to the entire site. `credentialless` is intentional: the reader performs
cross-origin, no-credential PDS requests.

Recommended cache policy:

- `/standard-reader/app/`, `/standard-reader/app/index.html`, and `sw.js`: revalidate;
- fingerprinted `.js` and `.wasm` files: `public, max-age=31536000, immutable`.

The website repository's existing `page_build` workflow purges Cloudflare after GitHub Pages
deploys. Fingerprinted assets make long browser caching safe, while revalidating the entry point
lets it discover each release's new filenames.

## Acceptance

After committing and pushing the website repository:

```sh
curl -sSI https://www.davidlewis.xyz/standard-reader/app/
```

Confirm a `200` response plus both COOP and COEP headers. In the browser console:

```js
window.crossOriginIsolated
```

must be `true`.

Then:

1. open a publication and a post containing images;
2. wait for the offline-storage writes to finish;
3. switch the browser network to Offline;
4. reload `/standard-reader/app/`; and
5. confirm that the app shell, publication, post, and images all render.

Also recheck `/standard-reader/` and `/standard-reader/client_metadata.json`; the app deployment
must not change either route.

Before testing browser sign-in, verify that its client metadata is directly fetchable and does not
redirect:

```sh
curl -sSI https://www.davidlewis.xyz/standard-reader/web_client_metadata.json
```

It must return `200` with a JSON content type. Then sign in by handle, approve the request, and
confirm the redirect returns to `/standard-reader/app/` signed in. Reload the page, reconcile a
local-only follow, confirm follow/unfollow writes upstream, and finally log out and reload.
