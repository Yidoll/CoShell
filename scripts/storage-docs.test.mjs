import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const docPath = path.join(root, "docs/storage.md");

test("storage documentation links and runnable examples reference existing project inputs", () => {
  const doc = readFileSync(docPath, "utf8");
  const links = [...doc.matchAll(/\[[^\]]+\]\(([^)#]+)(?:#[^)]*)?\)/g)];
  assert.ok(links.length > 0, "storage guide must include local source links");
  for (const [, link] of links) {
    if (/^[a-z]+:/i.test(link)) continue;
    assert.ok(existsSync(path.resolve(path.dirname(docPath), link)), `missing documentation link: ${link}`);
  }
  const packageJson = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8"));
  const commands = [...doc.matchAll(/^((?:pnpm|cargo) [^\r\n]+)$/gm)].map((match) => match[1]);
  assert.ok(commands.length > 0, "storage guide must include checkable commands");
  for (const command of commands) {
    const parts = command.split(/\s+/);
    if (parts[0] === "pnpm") assert.ok(packageJson.scripts?.[parts[1]], `unknown script: ${command}`);
    if (parts[0] === "cargo") {
      assert.equal(parts[1], "test", `unsupported example: ${command}`);
      const packageIndex = parts.indexOf("-p");
      assert.ok(packageIndex > 0 && existsSync(path.join(root, "crates", parts[packageIndex + 1], "Cargo.toml")), `unknown Cargo package: ${command}`);
      const testIndex = parts.indexOf("--test");
      if (testIndex > 0) {
        const testFile = path.join(root, "crates", parts[packageIndex + 1], "tests", `${parts[testIndex + 1]}.rs`);
        assert.ok(existsSync(testFile), `unknown Cargo test target: ${command}`);
      }
      const featureIndex = parts.indexOf("--features");
      if (featureIndex > 0) {
        const manifest = readFileSync(path.join(root, "crates", parts[packageIndex + 1], "Cargo.toml"), "utf8");
        assert.match(manifest, new RegExp(`^${parts[featureIndex + 1]}\\s*=`, "m"));
      }
    }
  }
});
