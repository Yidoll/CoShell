import { parse } from "smol-toml";

const dependencySections = ["dependencies", "dev-dependencies", "build-dependencies"];

// Cargo dependency keys may be aliases. Resolve the package field, including
// dependencies inherited from the workspace, before enforcing layer boundaries.
export function parseRustManifest(source, workspaceSource) {
  const manifest = parse(source);
  const workspace = parse(workspaceSource).workspace ?? {};
  const inherited = workspace.dependencies ?? {};
  const dependencies = new Set();

  function collect(sections) {
    for (const section of dependencySections) {
      for (const [alias, value] of Object.entries(sections?.[section] ?? {})) {
        const spec = typeof value === "object" && value !== null ? value : {};
        const parent = spec.workspace === true ? inherited[alias] : undefined;
        const parentSpec = typeof parent === "object" && parent !== null ? parent : {};
        dependencies.add(spec.package ?? parentSpec.package ?? alias);
      }
    }
  }

  collect(manifest);
  for (const target of Object.values(manifest.target ?? {})) collect(target);
  return { name: manifest.package?.name, dependencies };
}
