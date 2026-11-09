import { html, useState } from "../vendor/preact-htm.js";
import { Icon, Logo } from "./ui.js";

export function Login({ onLogin, hadToken }) {
  const [token, setToken] = useState("");
  const submit = (e) => {
    e.preventDefault();
    if (token.trim()) onLogin(token.trim());
  };
  return html`<div class="login">
    <form class="login-card" onSubmit=${submit}>
      <div class="login-head"><${Logo} size=${48} /><div><h1>Scythe</h1><p class="dim">This session needs its token</p></div></div>
      ${hadToken && html`<p class="login-error">That token was not accepted.</p>`}
      <label class="field">
        <span>Token</span>
        <div class="input-icon"><${Icon} name="key" size=${16} />
          <input type="password" autocomplete="current-password" autocapitalize="off" autocorrect="off" spellcheck="false"
            value=${token} onInput=${(e) => setToken(e.target.value)} placeholder="the --token Scythe was started with" autofocus /></div>
      </label>
      <button class="btn primary big" type="submit" disabled=${!token.trim()}>Connect</button>
      <p class="dim small">The token is remembered on this device. A link with <code>?token=…</code> logs in directly.</p>
    </form>
  </div>`;
}
