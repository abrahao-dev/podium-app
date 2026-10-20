// Builds the `podium` agent CLI and stages it as a Tauri sidecar:
// src-tauri/binaries/podium-<target-triple>[.exe]
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const cliDir = join(root, "src-tauri", "cli");

execFileSync(
  "cargo",
  ["build", "--release", "--manifest-path", join(cliDir, "Cargo.toml")],
  { stdio: "inherit", cwd: root },
);

function targetTriple() {
  const fromEnv = process.env.TAURI_ENV_TARGET_TRIPLE;
  if (fromEnv) return fromEnv;
  const info = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  const match = /^host:\s*(\S+)/m.exec(info);
  if (!match) {
    throw new Error("build-cli: could not determine host target triple from `rustc -vV`");
  }
  return match[1];
}

const triple = targetTriple();
const builtExt = process.platform === "win32" ? ".exe" : "";
const outExt = triple.includes("windows") ? ".exe" : "";
const source = join(cliDir, "target", "release", `podium${builtExt}`);
const destDir = join(root, "src-tauri", "binaries");
const dest = join(destDir, `podium-${triple}${outExt}`);

mkdirSync(destDir, { recursive: true });
copyFileSync(source, dest);
console.log(`build-cli: ${dest}`);
