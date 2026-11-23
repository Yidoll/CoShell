import assert from "node:assert/strict";
import { execFile as execFileCallback } from "node:child_process";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";
import { promisify } from "node:util";

const execFile = promisify(execFileCallback);
import {
  buildUpdateManifest,
  verifyUploadedAssets,
  validateReleaseVersion,
  validatePlatformBuild,
  selectReleaseDraft,
} from "./desktop-release.mjs";

const version = "0.1.2";
const tag = "v" + version;
const releasedAt = "2026-10-20T10:00:00Z";
const filenames = [
  "CoShell_0.1.2_x64-setup.exe",
  "CoShell_0.1.2_x64.app.tar.gz",
  "CoShell_0.1.2_aarch64.app.tar.gz",
];
const installers = ["CoShell_0.1.2_x64.dmg", "CoShell_0.1.2_aarch64.dmg"];

function signingFixture() {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const keyId = Buffer.from("CoShell!");
  const rawPublicKey = publicKey.export({ format: "der", type: "spki" }).subarray(-32);
  const pubkey = Buffer.from(
    "untrusted comment: test key\n" +
      Buffer.concat([Buffer.from("Ed"), keyId, rawPublicKey]).toString("base64") + "\n",
  ).toString("base64");
  return {
    pubkey,
    signature(body, signedVersion = version) {
      const digest = createHash("blake2b512").update(body).digest();
      const signature = sign(null, digest, privateKey);
      const comment = "timestamp:10\tfile:update\tversion:" + signedVersion;
      const trusted = sign(null, Buffer.concat([signature, Buffer.from(comment)]), privateKey);
      return Buffer.from([
        "untrusted comment: test signature",
        Buffer.concat([Buffer.from("ED"), keyId, signature]).toString("base64"),
        "trusted comment: " + comment,
        trusted.toString("base64"),
        "",
      ].join("\n")).toString("base64");
    },
  };
}

async function releaseFixture(t) {
  const root = await mkdtemp(join(tmpdir(), "coshell-release-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const signer = signingFixture();
  for (const name of filenames) {
    const bytes = Buffer.from("test update " + name);
    await writeFile(join(root, name), bytes);
    await writeFile(join(root, name + ".sig"), signer.signature(bytes));
  }
  for (const name of installers) await writeFile(join(root, name), "test installer " + name);
  return {
    root, signer,
    manifest: () => buildUpdateManifest({
      tag, assetDir: root, publicKey: signer.pubkey, notes: "测试版本", publishedAt: releasedAt,
    }),
  };
}

test("publishes a manifest with three exact targets and embedded signatures", async (t) => {
  const fixture = await releaseFixture(t);
  const manifest = await fixture.manifest();
  assert.equal(manifest.version, version);
  assert.equal(manifest.pub_date, releasedAt);
  assert.equal(manifest.notes, "测试版本");
  const targets = ["windows-x86_64", "darwin-x86_64", "darwin-aarch64"];
  assert.deepEqual(Object.keys(manifest.platforms), targets);
  for (const [index, target] of targets.entries()) {
    const name = filenames[index];
    assert.equal(manifest.platforms[target].url, "https://github.com/Yidoll/CoShell/releases/download/v0.1.2/" + name);
    assert.equal(manifest.platforms[target].signature, await readFile(join(fixture.root, name + ".sig"), "utf8"));
  }
});

test("rejects missing signatures, wrong architecture names and absent installers", async (t) => {
  const fixture = await releaseFixture(t);
  const signature = join(fixture.root, filenames[2] + ".sig");
  const value = await readFile(signature);
  await rm(signature);
  await writeFile(join(fixture.root, "CoShell_0.1.2_arm.app.tar.gz.sig"), value);
  await assert.rejects(fixture.manifest, /aarch64\.app\.tar\.gz\.sig/);
  await writeFile(signature, value);
  await rm(join(fixture.root, installers[1]));
  await assert.rejects(fixture.manifest, /aarch64\.dmg/);
});

test("rejects a replaced update, the wrong key and signatures from another version", async (t) => {
  const fixture = await releaseFixture(t);
  const path = join(fixture.root, filenames[0]);
  const other = Buffer.from("different update");
  await writeFile(path, other);
  await assert.rejects(fixture.manifest, /signature/i);
  await writeFile(path + ".sig", signingFixture().signature(other));
  await assert.rejects(fixture.manifest, /signature/i);
  await writeFile(path + ".sig", fixture.signer.signature(other, "0.1.1"));
  await assert.rejects(fixture.manifest, /signed version/i);
});

test("requires a stable tag and matching project version declarations", () => {
  assert.equal(validateReleaseVersion(tag, version, version), version);
  assert.throws(() => validateReleaseVersion("v0.1.2-rc.1", version, version), /stable tag/i);
  assert.throws(() => validateReleaseVersion(tag, "0.1.1", version), /Cargo/i);
  assert.throws(() => validateReleaseVersion(tag, version, "0.1.1"), /Tauri/i);
});

test("rejects package architecture, version and identity mismatches", () => {
  const mac = { version, architecture: "arm64", identifier: "com.coshell.app" };
  const win = { version, architecture: "x86_64", identifier: "CoShell" };
  assert.doesNotThrow(() => validatePlatformBuild(version, "darwin-aarch64", mac));
  assert.doesNotThrow(() => validatePlatformBuild(version, "windows-x86_64", win));
  assert.throws(() => validatePlatformBuild(version, "darwin-x86_64", mac), /architecture/i);
  assert.throws(() => validatePlatformBuild(version, "darwin-aarch64", { ...mac, version: "0.1.1" }), /version/i);
  assert.throws(() => validatePlatformBuild(version, "windows-x86_64", { ...win, identifier: "Another app" }), /identity/i);
});

test("promotion rejects incomplete or modified remote assets", async (t) => {
  const fixture = await releaseFixture(t);
  await writeFile(join(fixture.root, "latest.json"), JSON.stringify(await fixture.manifest()));
  const files = [...filenames, ...filenames.map((name) => name + ".sig"), ...installers, "latest.json"];
  const assets = await Promise.all(files.map(async (name) => {
    const body = await readFile(join(fixture.root, name));
    return { name, size: body.length, digest: "sha256:" + createHash("sha256").update(body).digest("hex"), state: "uploaded" };
  }));
  await verifyUploadedAssets(tag, fixture.root, assets);
  await assert.rejects(verifyUploadedAssets(tag, fixture.root, assets.slice(1)), /missing/i);
  await assert.rejects(verifyUploadedAssets(tag, fixture.root, [...assets, { ...assets[0], name: "unexpected.exe" }]), /unexpected/i);
  await assert.rejects(verifyUploadedAssets(tag, fixture.root, [{ ...assets[0], digest: "sha256:wrong" }, ...assets.slice(1)]), /digest/i);
  await assert.rejects(verifyUploadedAssets(tag, fixture.root, [{ ...assets[0], state: "starter" }, ...assets.slice(1)]), /upload/i);
});

test("refuses empty native build assets", async (t) => {
  const fixture = await releaseFixture(t);
  await mkdir(join(fixture.root, "unused"));
  await writeFile(join(fixture.root, installers[0]), "");
  await assert.rejects(fixture.manifest, /empty/i);
});

test("reuses only the draft for this tag and prevents replacing or downgrading a stable release", () => {
  const stable = { tag_name: "v0.1.1", draft: false, prerelease: false };
  const draft = { tag_name: tag, draft: true, prerelease: false, id: 10 };
  assert.equal(selectReleaseDraft(tag, [stable]), "");
  assert.equal(selectReleaseDraft(tag, [stable, draft]), "10");
  assert.throws(() => selectReleaseDraft(tag, [{ ...draft, draft: false }]), /already published/i);
  assert.throws(() => selectReleaseDraft(tag, [{ ...stable, tag_name: "v0.10.0" }]), /newer/i);
  assert.throws(() => selectReleaseDraft(tag, [draft, { ...draft, id: 11 }]), /multiple/i);
});

test("accepts version-bound signatures generated by the locked Tauri CLI", async (t) => {
  const fixture = await releaseFixture(t);
  const keyPath = join(fixture.root, "fixture-key");
  const tauri = resolve("node_modules/@tauri-apps/cli/tauri.js");
  const cli = (...args) => execFile(process.execPath, [tauri, "signer", ...args]);
  const password = "test-only-fixture-password";
  await cli("generate", "--ci", "--password", password, "--write-keys", keyPath);
  for (const name of filenames) {
    const file = join(fixture.root, name);
    await rm(file + ".sig");
    await cli("sign", "--private-key-path", keyPath, "--password", password, "--app-version", version, file);
  }
  const publicKey = await readFile(keyPath + ".pub", "utf8");
  const manifest = await buildUpdateManifest({
    tag, assetDir: fixture.root, publicKey, notes: "测试签名", publishedAt: releasedAt,
  });
  assert.equal(Object.keys(manifest.platforms).length, 3);
});
