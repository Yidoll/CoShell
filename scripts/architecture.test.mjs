import assert from "node:assert/strict";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { parseRustManifest } from "./rust-manifest.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const read = (relativePath) => readFileSync(path.join(root, relativePath), "utf8");

const expectedPaths = [
  "Cargo.toml",
  "package.json",
  "apps/desktop/src/main.ts",
  "apps/desktop/src/StartupGate.vue",
  "apps/desktop/src/App.vue",
  "apps/desktop/src/lib/backend/api.ts",
  "apps/desktop/src/lib/backend/tauri.ts",
  "apps/desktop/src/lib/backend/http.ts",
  "src-tauri/src/main.rs",
  "src-tauri/src/lib.rs",
  "agents/README.md",
  "plugins/README.md",
  "packages/README.md",
  "docs/coshell-architecture.md",
];

const expectedCrateDirectories = [
  "coshell-types",
  "coshell-platform",
  "coshell-storage",
  "coshell-shell",
  "coshell-backup",
  "coshell-formats",
  "coshell-sql",
  "coshell-ai-provider",
  "coshell-plugin-runtime",
  "coshell-drivers",
  "coshell-core",
  "coshell-web",
  "coshell-cli",
  "coshell-mcp",
];

function parseManifest(relativePath) {
  const { name, dependencies } = parseRustManifest(read(relativePath), read("Cargo.toml"));
  assert.ok(name, `${relativePath} must declare [package].name`);
  return { name, dependencies, relativePath };
}

test("renamed and workspace dependencies resolve to their real package names", () => {
  const workspace = `
[workspace.dependencies]
shared = { package = "coshell-core", path = "crates/coshell-core" }
`;
  const manifest = `
[package]
name = "example"
[dependencies]
alias = { package = "coshell-core", path = "../coshell-core" }
shared = { workspace = true }
[build-dependencies.build_alias]
package = "coshell-web"
path = "../coshell-web"
[target.'cfg(windows)'.dev-dependencies]
test_alias = { package = "coshell-cli", path = "../coshell-cli" }
`;
  const result = parseRustManifest(manifest, workspace);
  assert.deepEqual([...result.dependencies].sort(), ["coshell-cli", "coshell-core", "coshell-web"]);
});

function workspaceManifests() {
  const crateRoot = path.join(root, "crates");
  const crateManifests = existsSync(crateRoot)
    ? readdirSync(crateRoot, { withFileTypes: true })
        .filter((entry) => entry.isDirectory() && existsSync(path.join(crateRoot, entry.name, "Cargo.toml")))
        .map((entry) => `crates/${entry.name}/Cargo.toml`)
    : [];
  if (existsSync(path.join(root, "src-tauri/Cargo.toml"))) crateManifests.push("src-tauri/Cargo.toml");
  return crateManifests.map(parseManifest);
}

test("required architecture entrypoints exist", () => {
  for (const relativePath of expectedPaths) {
    assert.ok(existsSync(path.join(root, relativePath)), `missing architecture entrypoint: ${relativePath}`);
  }
  for (const directory of expectedCrateDirectories) {
    assert.ok(existsSync(path.join(root, "crates", directory, "Cargo.toml")), `missing workspace crate: crates/${directory}`);
  }
});

test("Rust package names are unique", () => {
  const manifests = workspaceManifests();
  const names = manifests.map((manifest) => manifest.name);
  assert.equal(new Set(names).size, names.length, "workspace package names must be unique");
});

test("expected crates are members of the root workspace", () => {
  const workspace = read("Cargo.toml");
  const members = workspace.match(/^members\s*=\s*\[([\s\S]*?)\]/m)?.[1] ?? "";
  const paths = new Set([...members.matchAll(/"([^"]+)"/g)].map((match) => match[1]));
  for (const directory of expectedCrateDirectories) {
    assert.ok(paths.has(`crates/${directory}`), `workspace must include crates/${directory}`);
  }
});

test("implementation crates do not depend on application layers", () => {
  const manifests = workspaceManifests();
  const internalNames = new Set(manifests.map((manifest) => manifest.name));
  const forbiddenTargets = new Set(["coshell-core", "coshell-web", "coshell-cli", "coshell-mcp", "coshell-desktop"]);
  const implementationCrates = new Set([
    "coshell-types",
    "coshell-platform",
    "coshell-storage",
    "coshell-shell",
    "coshell-backup",
    "coshell-formats",
    "coshell-sql",
    "coshell-ai-provider",
    "coshell-plugin-runtime",
    "coshell-drivers",
  ]);

  for (const manifest of manifests.filter((candidate) => implementationCrates.has(candidate.name))) {
    for (const dependency of manifest.dependencies) {
      if (internalNames.has(dependency)) {
        assert.ok(!forbiddenTargets.has(dependency), `${manifest.name} must not depend on application layer ${dependency}`);
      }
    }
  }
});

test("core does not depend on transport entrypoints", () => {
  const core = workspaceManifests().find((manifest) => manifest.name === "coshell-core");
  assert.ok(core, "coshell-core must exist");
  for (const entrypoint of ["coshell-web", "coshell-cli", "coshell-mcp", "coshell-desktop"]) {
    assert.ok(!core.dependencies.has(entrypoint), `coshell-core must not depend on ${entrypoint}`);
  }
});

test("frontend components use the unified backend boundary", () => {
  const app = read("apps/desktop/src/App.vue");
  assert.match(app, /@\/lib\/backend\/api/, "App.vue must consume the unified backend API");
  assert.doesNotMatch(app, /@tauri-apps\/api/, "App.vue must not call Tauri directly");
  const api = read("apps/desktop/src/lib/backend/api.ts");
  assert.match(api, /backend\/tauri|\.\/tauri/, "backend API must expose the Tauri transport");
  assert.match(api, /backend\/http|\.\/http/, "backend API must expose the HTTP transport");
});
