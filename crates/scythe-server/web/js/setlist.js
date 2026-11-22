// The setlist for the stage: what plays now and next in type readable from the drum stool, big
// previous / play-stop / next, and the set's songs to cue with a tap.

import { html, useCallback, useEffect, useRef, useState } from "../vendor/preact-htm.js";
import { countInLabel, detector, normalizeSetlists, normalizeStatus, setlistCommands, songProgress } from "./domains.js";
import { formatClock } from "./format.js";
import { Unavailable } from "./monitor.js";
import { useStoreValue } from "./store.js";
import { FullscreenButton, Icon, Logo, OfflineVeil, StatusPill, Switch } from "./ui.js";

function useSetlists(client, status) {
  const [data, setData] = useState(null);
  const [state, setState] = useState(null);
  const [supported, setSupported] = useState(null);
  const detect = useRef(detector(client, setlistCommands.list()));
  const load = useCallback(() => {
    Promise.all([client.send(setlistCommands.list()), client.send(setlistCommands.songs())]).then(
      ([list, songs]) => setData(normalizeSetlists(list, songs)),
      () => {},
    );
    client.send(setlistCommands.status()).then((s) => setState(normalizeStatus(s)), () => {});
  }, [client]);
  useEffect(() => {
    if (status !== "open") return;
    detect.current().then((ok) => {
      if (ok === null) return;
      setSupported(ok);
      if (ok) load();
    });
  }, [status]);
  useEffect(() => {
    let timer = null;
    const soon = () => {
      clearTimeout(timer);
      timer = setTimeout(load, 150);
    };
    const offChanged = client.on("event:project_changed", soon);
    const offEvent = client.on("event:setlist", (e) => setState(normalizeStatus(e)));
    return () => {
      clearTimeout(timer);
      offChanged();
      offEvent();
    };
  }, [load]);
  return { data, state, setState, supported, load };
}

function Progress({ song, fraction }) {
  const ticks = song?.duration > 0 ? song.sections.filter((s) => s.position > song.start && s.position < song.start + song.duration) : [];
  return html`<div class="progress">
    <div class="progress-fill" style=${{ width: `${fraction * 100}%` }}></div>
    ${ticks.map((s) => html`<div class="cue-tick" style=${{ left: `${((s.position - song.start) / song.duration) * 100}%` }} title=${s.name}>
      ${s.name && html`<span>${s.name}</span>`}</div>`)}
  </div>`;
}

export function SetlistPage({ client, store }) {
  const status = useStoreValue(store, "status");
  const transport = useStoreValue(store, "transport");
  const { data, state, setState, supported, load } = useSetlists(client, status);
  const listRef = useRef(null);
  const active = data ? data.lists.find((l) => l.index === (state?.setlist ?? data.active)) || data.lists[0] : null;
  const current = state?.current ?? null;
  const song = active && current != null ? active.songs[current] : null;
  const next = active && state?.next != null ? active.songs[state.next] : null;
  const playing = transport?.playing ?? state?.playing ?? false;
  const progress = song && transport ? songProgress(song, transport.position) : { elapsed: state?.elapsed, remaining: state?.remaining, fraction: 0 };

  useEffect(() => {
    document.title = song ? `${song.name} – Scythe` : "Setlist – Scythe";
  }, [song?.name]);
  useEffect(() => {
    listRef.current?.querySelector(".song.current")?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [current, active?.index]);

  if (supported === false) return html`<${Unavailable} what="The setlist" />`;
  if (!data) return html`<div class="boot"><${Logo} size=${56} /><div>Connecting…</div><${OfflineVeil} status=${status} /></div>`;

  const run = (command, optimistic) => {
    if (optimistic) setState((s) => ({ ...(s || {}), ...optimistic }));
    store.run(command).then((answer) => setState(normalizeStatus(answer)), () => load());
  };
  const autoAdvance = state?.autoAdvance ?? data.autoAdvance;
  const countIn = state?.countIn ?? active?.countIn ?? false;
  const counting = countInLabel(transport);
  return html`<div class="setlist-page">
    <header class="setlist-head">
      <${Logo} size=${40} />
      ${data.lists.length > 1
        ? html`<select class="setlist-switch" value=${active?.index} onChange=${(e) => run(setlistCommands.use(parseInt(e.target.value, 10)), { setlist: parseInt(e.target.value, 10), current: 0 })}>
            ${data.lists.map((l) => html`<option value=${l.index}>${l.name}</option>`)}
          </select>`
        : html`<div class="setlist-name">${active?.name || "No setlist"}</div>`}
      <span class="grow"></span>
      <label class="auto-advance"><span>Auto-advance</span>
        <${Switch} on=${!!autoAdvance} title="Cue the next song when one ends" onChange=${(on) => run(setlistCommands.autoAdvance(on), { autoAdvance: on })} /></label>
      <label class="auto-advance"><span>Count-in</span>
        <${Switch} on=${!!countIn} title="Count each song in with the metronome" onChange=${(on) => run(setlistCommands.countIn(on), { countIn: on })} /></label>
      <${StatusPill} status=${status} />
      <${FullscreenButton} />
    </header>
    <main class="setlist-main">
      <section class="now">
        <div class=${`label ${counting ? "counting" : ""}`}>${counting ? `Count-in ${counting}` : playing ? "Now playing" : "Cued"}</div>
        <div class="now-title">
          ${song ? html`<span class="now-num mono">${current + 1}</span><span class="now-name">${song.name}</span>` : html`<span class="now-name dim">Nothing cued</span>`}
        </div>
        <div class="clocks mono">
          <span class="elapsed">${formatClock(progress.elapsed ?? 0)}</span>
          <span class="remaining">${progress.remaining != null ? `-${formatClock(progress.remaining)}` : ""}</span>
        </div>
        <${Progress} song=${song} fraction=${progress.fraction || 0} />
        <div class="next-up">
          <div class="label">Next up</div>
          <div class="next-name">${next ? html`<span class="mono dim">${state.next + 1}</span> ${next.name}` : html`<span class="dim">End of the set</span>`}</div>
        </div>
        <div class="big-controls">
          <button class="big-btn" onClick=${() => run(setlistCommands.previous())} aria-label="Previous song"><${Icon} name="previous" size=${44} /></button>
          <button class=${`big-btn play ${playing ? "playing" : ""}`} onClick=${() => run(playing ? setlistCommands.stop() : setlistCommands.play())}
            aria-label=${playing ? "Stop" : "Play"}><${Icon} name=${playing ? "stop" : "play"} size=${56} /></button>
          <button class="big-btn" onClick=${() => run(setlistCommands.next())} aria-label="Next song"><${Icon} name="next" size=${44} /></button>
        </div>
      </section>
      <section class="songs" ref=${listRef}>
        ${(active?.songs || []).map(
          (s, i) => html`<button class=${`song ${i === current ? "current" : ""} ${i === state?.next ? "next" : ""} ${s.missing ? "missing" : ""}`}
            key=${i} onClick=${() => run(setlistCommands.cue(i), { current: i })} disabled=${s.missing}>
            <span class="song-num mono">${i + 1}</span>
            <span class="song-name">${s.name}</span>
            <span class="song-len mono">${formatClock(s.duration)}</span>
          </button>`,
        )}
        ${active && active.songs.length === 0 && html`<p class="dim">This setlist is empty. Setlists are made from the session's markers in Scythe.</p>`}
      </section>
    </main>
    <${OfflineVeil} status=${status} />
  </div>`;
}
