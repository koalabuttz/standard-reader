// The reader's OPFS cache preserves publications and images. The app-scoped service worker
// preserves the HTML/JS/WASM shell too, so a previously opened reader can start offline.
if ("serviceWorker" in navigator && window.isSecureContext) {
  window.addEventListener("load", () => {
    navigator.serviceWorker.register("./sw.js", { scope: "./" }).catch((error) => {
      console.warn("standard-reader service worker registration failed", error);
    });
  });
}
