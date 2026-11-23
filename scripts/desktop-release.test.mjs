import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { releaseDraftStatus, validateReleaseVersion, writeInstallerChecksums } from "./desktop-release.mjs";

const tag = "v0.1.2";
const names = ["CoShell_0.1.2_windows-x64.exe", "CoShell_0.1.2_macos-x64.dmg", "CoShell_0.1.2_macos-arm64.dmg"];

test("stable release tags must match every declared application version", () => {
  const manifests = {
    "Cargo.toml": "0.1.2",
    "package.json": "0.1.2",
    "apps/desktop/package.json": "0.1.2",
    "src-tauri/tauri.conf.json": "0.1.2",
  };
  assert.equal(validateReleaseVersion(tag, manifests), "0.1.2");
  assert.throws(() => validateReleaseVersion("v0.1.2-beta", manifests), /stable tag/);
  for (const manifest of Object.keys(manifests)) {
    assert.throws(
      () => validateReleaseVersion(tag, { ...manifests, [manifest]: "0.1.1" }),
      (error) => error.message.includes(`${manifest} version`),
    );
  }
});

test("the destination is created once, reused as a draft, or left alone after publication", () => {
  assert.equal(releaseDraftStatus(tag, [{ tag_name: "v0.1.1", draft: false }]), "absent");
  assert.equal(releaseDraftStatus(tag, [{ tag_name: tag, draft: true }]), "pending");
  assert.equal(releaseDraftStatus(tag, [{ tag_name: tag, draft: false }]), "published");
  assert.throws(() => releaseDraftStatus(tag, [{ tag_name: tag }, { tag_name: tag }]), /Multiple releases/);
});

test("checksums are written only when all three installers have content", async (t) => {
  const dir = await mkdtemp(join(tmpdir(), "coshell-release-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  await writeFile(join(dir, names[0]), names[0]);
  await writeFile(join(dir, names[1]), names[1]);
  await assert.rejects(writeInstallerChecksums(tag, dir), /ENOENT/);
  await writeFile(join(dir, names[2]), "");
  await assert.rejects(writeInstallerChecksums(tag, dir), /empty installer/);
  await assert.rejects(readFile(join(dir, "SHA256SUMS.txt")), /ENOENT/);
  await writeFile(join(dir, names[2]), names[2]);
  await writeInstallerChecksums(tag, dir);
  const expected = names.map((name) => `${createHash("sha256").update(name).digest("hex")}  ${name}\n`).join("");
  assert.equal(await readFile(join(dir, "SHA256SUMS.txt"), "utf8"), expected);
});
