import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import test from "node:test";

const root = fileURLToPath(new URL("../", import.meta.url));
const read = (file) => readFileSync(`${root}/${file}`, "utf8");

test("desktop bundles include platform icons and the offline Windows WebView runtime", () => {
  const config = JSON.parse(read("src-tauri/tauri.conf.json"));
  assert.equal(config.bundle.active, true);
  assert.equal(config.bundle.windows.webviewInstallMode.type, "offlineInstaller");
  assert.equal(config.bundle.macOS.minimumSystemVersion, "13.0");
  for (const icon of ["icons/icon.ico", "icons/icon.icns", "icons/128x128.png"]) {
    assert.ok(config.bundle.icon.includes(icon));
    assert.ok(existsSync(`${root}/src-tauri/${icon}`));
  }
});

test("CI builds the three supported native target bundles", () => {
  const workflow = read(".github/workflows/ci.yml");
  for (const target of ["x86_64-pc-windows-msvc", "x86_64-apple-darwin", "aarch64-apple-darwin"]) assert.ok(workflow.includes(target));
  assert.match(workflow, /tauri build --target/);
  assert.match(workflow, /actions\/upload-artifact@v4/);
  assert.match(workflow, /tauri-packaging\.test\.mjs/);
  assert.match(workflow, /cargo check --locked --workspace/);
  assert.match(workflow, /cargo test --locked --workspace/);
  assert.match(workflow, /tauri build --target [^\n]+ -- --locked/);
});
