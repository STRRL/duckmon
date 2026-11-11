// Builds the Node-API addon in release mode and copies it to bindings/node/duckmon.node.
import { execFileSync } from "node:child_process";
import { copyFileSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const packageDir = fileURLToPath(new URL("..", import.meta.url));
const workspaceDir = path.resolve(packageDir, "../..");

const libraryNames = {
  darwin: "libduckmon_node.dylib",
  linux: "libduckmon_node.so",
  win32: "duckmon_node.dll",
};
const libraryName = libraryNames[process.platform];
if (libraryName === undefined) {
  throw new Error(`building duckmon.node on ${process.platform} is not supported yet`);
}

execFileSync("cargo", ["build", "-p", "duckmon-node", "--release"], { cwd: workspaceDir, stdio: "inherit" });

// Ask cargo where it put the build, so CARGO_TARGET_DIR and cargo config are handled the way cargo handles them.
const metadata = execFileSync("cargo", ["metadata", "--format-version", "1", "--no-deps"], {
  cwd: workspaceDir,
  encoding: "utf8",
  maxBuffer: 64 * 1024 * 1024,
});
const targetDir = JSON.parse(metadata).target_directory;

const output = path.join(packageDir, "duckmon.node");
// Remove the old file first, so a process that still has it loaded keeps its copy.
rmSync(output, { force: true });
copyFileSync(path.join(targetDir, "release", libraryName), output);
console.log(`copied ${libraryName} to ${output}`);
