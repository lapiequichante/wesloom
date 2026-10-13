// Load a page in headless Chrome, wait until its #status matches `--until`,
// print the status and the console, and save a screenshot (ADR 0075).
//
//   node dawn/tests/web_smoke.mjs <url> <screenshot.png> --until <regex> [chrome flags...]
//
// Exits 1 when the status matches `--fail` (default: panic|error|exit [1-9])
// or nothing matched `--until` within two minutes. Needs Node 22+ (built-in
// WebSocket) and Google Chrome; set CHROME to another binary if needed.
import { spawn } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const args = process.argv.slice(2);
const option = (name, fallback) => {
  const at = args.indexOf(name);
  if (at < 0) return fallback;
  return args.splice(at, 2)[1];
};
const until = new RegExp(option("--until", "rendered|exit 0"));
const fail = new RegExp(option("--fail", "panic|error|cannot|exit [1-9]"), "i");
const [url, screenshot, ...flags] = args;
const chrome =
  process.env.CHROME ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const port = 9300 + Math.floor(Math.random() * 600);
const browser = spawn(
  chrome,
  ["--headless=new", `--remote-debugging-port=${port}`,
   `--user-data-dir=${mkdtempSync(join(tmpdir(), "wxsl-chrome-"))}`,
   "--window-size=1000,620", ...flags, "about:blank"],
  { stdio: "ignore" },
);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
let target;
for (let i = 0; i < 100 && !target; i++) {
  await sleep(100);
  try {
    const pages = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
    target = pages.find((page) => page.type === "page");
  } catch {}
}
const socket = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((resolve) => socket.addEventListener("open", resolve));
let next = 0;
const pending = new Map();
const consoleLines = [];
socket.addEventListener("message", (event) => {
  const message = JSON.parse(event.data);
  if (pending.has(message.id)) {
    pending.get(message.id)(message.result);
    pending.delete(message.id);
  }
  if (message.method === "Runtime.consoleAPICalled")
    consoleLines.push(message.params.args.map((arg) => arg.value ?? arg.description).join(" "));
  if (message.method === "Runtime.exceptionThrown")
    consoleLines.push(
      "exception: " +
        (message.params.exceptionDetails.exception?.description ??
          message.params.exceptionDetails.text),
    );
});
const send = (method, params = {}) =>
  new Promise((resolve) => {
    pending.set(++next, resolve);
    socket.send(JSON.stringify({ id: next, method, params }));
  });
await send("Runtime.enable");
await send("Page.enable");
await send("Page.navigate", { url });
let status = "";
for (let i = 0; i < 240; i++) {
  await sleep(500);
  const reply = await send("Runtime.evaluate", {
    expression: "document.getElementById('status')?.textContent ?? ''",
  });
  status = reply?.result?.value ?? "";
  if (until.test(status) || fail.test(status)) break;
}
const shot = await send("Page.captureScreenshot", { format: "png" });
if (screenshot) writeFileSync(screenshot, Buffer.from(shot.data, "base64"));
console.log(status.trim());
for (const line of consoleLines) console.log("console:", line);
socket.close();
browser.kill();
process.exit(until.test(status) && !fail.test(status) ? 0 : 1);
