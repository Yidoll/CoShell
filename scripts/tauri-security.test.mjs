import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import test from "node:test";

const root = fileURLToPath(new URL("../", import.meta.url));
const readJson = (relative) => JSON.parse(readFileSync(`${root}/${relative}`, "utf8"));

test("desktop security config restricts content and capabilities", () => {
  const config = readJson("src-tauri/tauri.conf.json");
  const csp = config.app.security.csp;
  const directives = (policy) => Object.fromEntries(policy.split(";").map((entry) => entry.trim().split(/\s+/)).map(([key, ...values]) => [key, values]));
  assert.equal(typeof csp, "string");
  assert.match(csp, /default-src 'self'/);
  assert.match(csp, /object-src 'none'/);
  assert.doesNotMatch(csp, /(?:script-src|default-src)[^;]*'unsafe-eval'/);
  assert.doesNotMatch(csp, /(?:script-src)[^;]*'unsafe-inline'|connect-src[^;]*\*/);
  assert.equal(config.build.devUrl, "http://127.0.0.1:5173");
  assert.match(config.app.security.devCsp, /connect-src[^;]*ws:\/\/127\.0\.0\.1:5173/);
  const expected = {
    "default-src": ["'self'"], "script-src": ["'self'"],
    "style-src": ["'self'", "'unsafe-inline'"],
    "img-src": ["'self'", "asset:", "http://asset.localhost", "data:"],
    "connect-src": ["'self'", "ipc:", "http://ipc.localhost"],
    "object-src": ["'none'"], "base-uri": ["'self'"], "frame-ancestors": ["'none'"],
  };
  assert.deepEqual(directives(csp), expected);
  assert.deepEqual(directives(config.app.security.devCsp), {
    ...expected, "connect-src": [...expected["connect-src"], "ws://127.0.0.1:5173"],
  });

  const files = readdirSync(`${root}/src-tauri/capabilities`);
  assert.ok(files.length > 0);
  assert.ok(files.every((name) => name.endsWith(".json")), "capabilities must use checked JSON format");
  const capabilities = files.map((name) => readJson(`src-tauri/capabilities/${name}`));
  assert.ok(capabilities.some((item) => item.windows?.includes("main")));
  for (const capability of capabilities) {
    assert.deepEqual(capability.windows, ["main"]);
    assert.equal(capability.remote, undefined);
    assert.deepEqual(capability.permissions, ["core:default"]);
    for (const permission of capability.permissions) {
      const id = typeof permission === "string" ? permission : permission.identifier;
      assert.doesNotMatch(id, /^(shell|fs|clipboard-manager):/);
    }
  }
});

test("Windows desktop resource icon exists", () => {
  const icon = readFileSync(`${root}/src-tauri/icons/icon.ico`);
  assert.equal(icon.readUInt16LE(0), 0);
  assert.equal(icon.readUInt16LE(2), 1);
  assert.ok(icon.readUInt16LE(4) > 0);
});
