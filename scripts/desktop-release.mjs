import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { readFile, stat, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parse } from "smol-toml";

const stableTag = /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const installerSuffixes = ["windows-x64.exe", "macos-x64.dmg", "macos-arm64.dmg"];

function parseTag(tag) {
  if (!stableTag.test(tag)) throw new Error("Expected a stable tag in vMAJOR.MINOR.PATCH format");
  return tag.slice(1);
}

export function validateReleaseVersion(tag, versions) {
  const version = parseTag(tag);
  for (const [manifest, declared] of Object.entries(versions)) {
    if (declared !== version) throw new Error(`${manifest} version ${declared} does not match ${tag}`);
  }
  return version;
}

export function releaseDraftStatus(tag, releases) {
  parseTag(tag);
  const matched = releases.filter((release) => release.tag_name === tag);
  if (matched.length > 1) throw new Error("Multiple releases found for this tag");
  if (matched.length === 0) return "absent";
  return matched[0].draft ? "pending" : "published";
}

export async function writeInstallerChecksums(tag, assetDir) {
  const version = parseTag(tag);
  const lines = [];
  for (const suffix of installerSuffixes) {
    const filename = `CoShell_${version}_${suffix}`;
    const path = join(assetDir, filename);
    const metadata = await stat(path);
    if (!metadata.isFile() || metadata.size === 0) throw new Error(`Missing or empty installer: ${filename}`);
    const hash = createHash("sha256");
    for await (const chunk of createReadStream(path)) hash.update(chunk);
    lines.push(`${hash.digest("hex")}  ${filename}\n`);
  }
  await writeFile(join(assetDir, "SHA256SUMS.txt"), lines.join(""));
}

async function main([mode, tag, path]) {
  switch (mode) {
    case "version": {
      const manifests = await Promise.all(
        ["Cargo.toml", "package.json", "apps/desktop/package.json", "src-tauri/tauri.conf.json"].map(async (file) => {
          const content = await readFile(file, "utf8");
          const version = file.endsWith(".toml") ? parse(content).workspace.package.version : JSON.parse(content).version;
          return [file, version];
        }),
      );
      console.log(validateReleaseVersion(tag, Object.fromEntries(manifests)));
      break;
    }
    case "draft":
      console.log(releaseDraftStatus(tag, JSON.parse(await readFile(path, "utf8"))));
      break;
    case "checksums":
      await writeInstallerChecksums(tag, path);
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
