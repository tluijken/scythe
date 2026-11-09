import { html, render, useEffect, useState } from "../vendor/preact-htm.js";
import { Client, adoptTokenFromUrl, storeToken, storedToken } from "./api.js";
import { pageForAccess } from "./domains.js";
import { Login } from "./login.js";
import { Store, useStoreValue } from "./store.js";
import { Toasts } from "./ui.js";

const PAGES = {
  "/": () => import("./editor.js").then((m) => m.Editor),
  "/monitor": () => import("./monitor.js").then((m) => m.MonitorPage),
  "/setlist": () => import("./setlist.js").then((m) => m.SetlistPage),
};

function pageFor(pathname) {
  const path = pathname.replace(/\/+$/, "") || "/";
  return PAGES[path] ? path : "/";
}

function App({ client, store, path: asked }) {
  const status = useStoreValue(store, "status");
  const access = useStoreValue(store, "access");
  const path = pageForAccess(asked, access);
  const [loaded, setLoaded] = useState(null);
  useEffect(() => {
    let current = true;
    PAGES[path]().then((page) => current && setLoaded({ path, page }));
    return () => {
      current = false;
    };
  }, [path]);
  const Page = loaded?.path === path ? loaded.page : null;
  useEffect(() => {
    document.body.dataset.page = path.slice(1) || "editor";
    if (path !== asked) history.replaceState(null, "", path);
  }, [path]);
  const login = (token) => {
    storeToken(token);
    client.setToken(token);
    client.stop();
    client.start();
  };
  if (status === "unauthorized") return html`<${Login} onLogin=${login} hadToken=${!!client.token} />`;
  return html`${Page ? html`<${Page} client=${client} store=${store} />` : html`<div class="boot">Loading…</div>`}
    <${Toasts} store=${store} />`;
}

adoptTokenFromUrl();
const client = new Client({ token: storedToken() });
const store = new Store(client);
client.start();
document.addEventListener("visibilitychange", () => {
  // Phones freeze background tabs; coming back should not wait out a long backoff.
  if (document.visibilityState === "visible" && client.status === "reconnecting") {
    client.attempt = 0;
    client.stop();
    client.start();
  }
});
render(html`<${App} client=${client} store=${store} path=${pageFor(location.pathname)} />`, document.getElementById("app"));
