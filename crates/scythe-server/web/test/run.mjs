import { run } from "./harness.js";
import "./format.test.js";
import "./api.test.js";
import "./domains.test.js";
import "./qr.test.js";
import "./waveform.test.js";
import "./editor.test.js";

const results = await run((r) => console.log(`${r.ok ? "ok  " : "FAIL"} ${r.name}${r.ok ? "" : `: ${r.error}`}`));
const failed = results.filter((r) => !r.ok).length;
console.log(`${results.length - failed} passed, ${failed} failed`);
process.exit(failed ? 1 : 0);
