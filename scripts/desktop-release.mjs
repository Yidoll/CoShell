import { createHash, createPublicKey, verify } from "node:crypto";
import { createReadStream } from "node:fs";
import { readFile, stat, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parse } from "smol-toml";

const repository = "Yidoll/CoShell";
const stableTag = /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const targets = {
  "windows-x86_64": { suffix: "x64-setup.exe", architecture: "x86_64", identifier: "CoShell" },
  "darwin-x86_64": { suffix: "x64.app.tar.gz", installer: "x64.dmg", architecture: "x86_64", identifier: "com.coshell.app" },
  "darwin-aarch64": { suffix: "aarch64.app.tar.gz", installer: "aarch64.dmg", architecture: "arm64", identifier: "com.coshell.app" },
};

function parseTag(tag) {
  if (!stableTag.test(tag)) throw new Error("Expected a stable tag in vMAJOR.MINOR.PATCH format");
  return tag.slice(1);
}

export function validateReleaseVersion(tag, cargoVersion, tauriVersion) {
  const version = parseTag(tag);
  if (cargoVersion !== version) throw new Error("Tag version differs from the Cargo workspace version");
  if (tauriVersion !== version) throw new Error("Tag version differs from the Tauri bundle version");
  return version;
}

export function selectReleaseDraft(tag, releases) {
  const version = parseTag(tag).split(".").map(BigInt);
  let draftId = "";
  for (const release of releases) {
    if (release.tag_name === tag) {
      if (!release.draft) throw new Error("This tag is already published");
      if (draftId) throw new Error("Multiple drafts exist for this tag");
      if (!Number.isSafeInteger(release.id) || release.id < 1) throw new Error("Invalid release ID");
      draftId = String(release.id);
    }
    if (!release.draft && !release.prerelease && stableTag.test(release.tag_name)) {
      const existing = parseTag(release.tag_name).split(".").map(BigInt);
      const firstDifferent = existing.findIndex((part, i) => part !== version[i]);
      if (firstDifferent >= 0 && existing[firstDifferent] > version[firstDifferent]) {
        throw new Error("A newer stable release is already published");
      }
    }
  }
  return draftId;
}

export function validatePlatformBuild(version, target, metadata) {
  const expected = targets[target];
  if (!expected) throw new Error("Unsupported release platform");
  if (metadata.version !== version) throw new Error(target + " package version does not match the tag");
  if (metadata.architecture !== expected.architecture) throw new Error(target + " package architecture does not match the target");
  if (metadata.identifier !== expected.identifier) throw new Error(target + " package identity does not match CoShell");
}

function releaseFiles(version, target) {
  const prefix = "CoShell_" + version + "_";
  const update = prefix + target.suffix;
  return { update, signature: update + ".sig", installer: target.installer ? prefix + target.installer : update };
}

function expectedAssets(version) {
  return [...new Set(Object.values(targets).flatMap((target) => Object.values(releaseFiles(version, target)))), "latest.json"];
}

async function dataFile(path) {
  const entry = await stat(path);
  if (!entry.isFile() || entry.size === 0) throw new Error("Missing or empty release asset: " + path);
  return entry;
}

async function hashFile(path, algorithm) {
  const hash = createHash(algorithm);
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest();
}

// Tauri ships base64-encoded minisign files. The second signature binds
// the file signature to the trusted comment carrying the published version.
function decodeBase64(encoded) {
  const bytes = Buffer.from(encoded, "base64");
  if (!encoded || bytes.toString("base64") !== encoded) throw new Error("Invalid minisign encoding");
  return bytes;
}

function decodePublicKey(encoded) {
  const lines = decodeBase64(encoded.trim()).toString("utf8").trimEnd().split(/\r?\n/);
  if (lines.length !== 2 || !lines[0].startsWith("untrusted comment: ")) throw new Error("Invalid minisign public key");
  const raw = decodeBase64(lines[1]);
  if (raw.length !== 42 || raw.subarray(0, 2).toString() !== "Ed") throw new Error("Unsupported minisign public key");
  const spkiEd25519 = Buffer.from("302a300506032b6570032100", "hex");
  return {
    id: raw.subarray(2, 10),
    key: createPublicKey({ key: Buffer.concat([spkiEd25519, raw.subarray(10)]), format: "der", type: "spki" }),
  };
}

async function verifyUpdateSignature(file, encoded, publicKey, version) {
  const lines = decodeBase64(encoded).toString("utf8").trimEnd().split(/\r?\n/);
  const commentPrefix = "trusted comment: ";
  if (lines.length !== 4 || !lines[2].startsWith(commentPrefix)) throw new Error("Invalid updater signature format: " + file);
  const envelope = decodeBase64(lines[1]);
  const global = decodeBase64(lines[3]);
  // Tauri CLI 2.12 produces ED prehashed signatures. Stream the BLAKE2b
  // digest so a WebView2 offline installer need not reside in memory.
  if (envelope.length !== 74 || global.length !== 64 ||
      envelope.subarray(0, 2).toString() !== "ED" || !envelope.subarray(2, 10).equals(publicKey.id)) {
    throw new Error("Updater signature key or algorithm mismatch: " + file);
  }
  const signature = envelope.subarray(10);
  const trustedComment = lines[2].slice(commentPrefix.length);
  if (!verify(null, Buffer.concat([signature, Buffer.from(trustedComment)]), publicKey.key, global) ||
      !verify(null, await hashFile(file, "blake2b512"), publicKey.key, signature)) {
    throw new Error("Updater signature verification failed: " + file);
  }
  const signedVersions = trustedComment.split("\t").filter((field) => field.startsWith("version:"));
  if (signedVersions.length !== 1 || signedVersions[0] !== "version:" + version) {
    throw new Error("Updater signed version does not match the release: " + file);
  }
}

export async function buildUpdateManifest({ tag, assetDir, publicKey, notes, publishedAt }) {
  const version = parseTag(tag);
  if (typeof notes !== "string" || !Number.isFinite(Date.parse(publishedAt))) {
    throw new Error("Release notes or date are invalid");
  }
  const key = decodePublicKey(publicKey);
  const platforms = {};
  for (const [platform, target] of Object.entries(targets)) {
    const { update, signature, installer } = releaseFiles(version, target);
    const updatePath = join(assetDir, update);
    await dataFile(updatePath);
    await dataFile(join(assetDir, installer));
    const encoded = (await readFile(join(assetDir, signature), "utf8")).trim();
    await verifyUpdateSignature(updatePath, encoded, key, version);
    platforms[platform] = {
      url: "https://github.com/" + repository + "/releases/download/" + tag + "/" + update,
      signature: encoded,
    };
  }
  return { version, notes, pub_date: publishedAt, platforms };
}

export async function verifyUploadedAssets(tag, assetDir, assets) {
  const names = expectedAssets(parseTag(tag));
  const received = new Map();
  for (const asset of assets) {
    if (!names.includes(asset.name) || received.has(asset.name)) {
      throw new Error("Unexpected or duplicate release asset: " + asset.name);
    }
    received.set(asset.name, asset);
  }
  for (const name of names) {
    const asset = received.get(name);
    if (!asset) throw new Error("Missing uploaded release asset: " + name);
    if (asset.state !== "uploaded") throw new Error("Incomplete upload: " + name);
    const path = join(assetDir, name);
    const file = await dataFile(path);
    const sha256 = "sha256:" + (await hashFile(path, "sha256")).toString("hex");
    if (asset.size !== file.size || asset.digest !== sha256) {
      throw new Error("Uploaded asset size or digest mismatch: " + name);
    }
  }
}

async function currentVersion(tag) {
  const [cargoText, tauriText] = await Promise.all([
    readFile("Cargo.toml", "utf8"),
    readFile("src-tauri/tauri.conf.json", "utf8"),
  ]);
  return validateReleaseVersion(tag, parse(cargoText).workspace.package.version, JSON.parse(tauriText).version);
}

async function main([mode, tag, ...args]) {
  switch (mode) {
    case "version":
      console.log(await currentVersion(tag));
      break;
    case "verify-build":
      validatePlatformBuild(await currentVersion(tag), args[0], {
        version: args[1],
        architecture: args[2],
        identifier: args[3],
      });
      break;
    case "manifest": {
      const [assetDir, notesFile] = args;
      const manifest = await buildUpdateManifest({
        tag, assetDir, publicKey: process.env.COSHELL_UPDATER_PUBLIC_KEY ?? "",
        notes: (await readFile(notesFile, "utf8")).trim(),
        publishedAt: new Date().toISOString(),
      });
      await writeFile(join(assetDir, "latest.json"), JSON.stringify(manifest, null, 2) + "\n");
      break;
    }
    case "verify-uploads":
      await verifyUploadedAssets(tag, args[0], JSON.parse(await readFile(args[1], "utf8")));
      break;
    case "select-draft":
      console.log(selectReleaseDraft(tag, JSON.parse(await readFile(args[0], "utf8"))));
      break;
    default:
      throw new Error("Unknown desktop release command");
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
