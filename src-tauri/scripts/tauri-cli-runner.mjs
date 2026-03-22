import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const projectRoot = path.resolve(__dirname, "../..");
const defaultTauriConfig = path.resolve(projectRoot, "src-tauri/tauri.conf.json");

const command = process.argv[2];
if (!command) {
  console.error("Usage: node tauri-cli-runner.mjs <dev|build> [config-path]");
  process.exit(1);
}
const npmCommand = process.platform === "win32" ? "npm.cmd" : "npm";
const configArg = process.argv[3] ? path.resolve(projectRoot, process.argv[3]) : null;
const tauriConfig = configArg || defaultTauriConfig;

const argsByCommand = {
  info: ["--version"],
  dev: ["dev", "--config", tauriConfig],
  build: ["build", "--config", tauriConfig]
};

const commandArgs = argsByCommand[command];
if (!commandArgs) {
  console.error(`Unknown command: ${command}`);
  process.exit(1);
}

function resolveTauriCliBin(rootDir) {
  const binName = process.platform === "win32" ? "tauri.cmd" : "tauri";
  const candidate = path.resolve(rootDir, "node_modules", ".bin", binName);
  return fs.existsSync(candidate) ? candidate : null;
}

function runTauri(tauriBin, args) {
  if (process.platform === "win32" && tauriBin.toLowerCase().endsWith(".cmd")) {
    return spawnSync("cmd.exe", ["/d", "/s", "/c", tauriBin, ...args], {
      cwd: projectRoot,
      stdio: "inherit"
    });
  }
  return spawnSync(tauriBin, args, { cwd: projectRoot, stdio: "inherit" });
}

const tauriBin = resolveTauriCliBin(projectRoot);
const result = tauriBin
  ? runTauri(tauriBin, commandArgs)
  : spawnSync(npmCommand, ["exec", "--no", "--", "tauri", ...commandArgs], {
      cwd: projectRoot,
      stdio: "inherit"
    });

if (result.error) {
  console.error(result.error.code === "ENOENT" ? "tauri CLI not found — run: npm install" : result.error.message);
  process.exit(1);
}
process.exit(result.status || 0);
