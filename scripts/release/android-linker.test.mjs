import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));

test("the actual build script emits 16 KiB flags only for Android 64-bit targets", () => {
  const directory = mkdtempSync(path.join(tmpdir(), "ratspeak-android-linker-"));
  try {
    // Only stub the unrelated Tauri build dependency. Compile the real build
    // script, including its target-selection helper and behavioral tests.
    const stub = path.join(directory, "tauri_build.rs");
    const library = path.join(directory, "libtauri_build.rlib");
    const binary = path.join(directory, process.platform === "win32" ? "build-tests.exe" : "build-tests");
    writeFileSync(stub, "pub fn build() {}\n");
    const compile = (args) => {
      const result = spawnSync("rustc", args, {
        encoding: "utf8", timeout: 30_000, maxBuffer: 1024 * 1024,
      });
      assert.equal(result.error, undefined, result.error?.message);
      assert.equal(result.status, 0, result.stdout + result.stderr);
    };
    compile(["--edition=2024", "--crate-type=rlib", "--crate-name=tauri_build", stub, "-o", library]);
    const source = path.join(root, "src-tauri/build.rs");
    compile(["--edition=2024", "--test", source, "--extern", `tauri_build=${library}`, "-o", binary]);
    const result = spawnSync(binary, [], { encoding: "utf8", timeout: 10_000, maxBuffer: 1024 * 1024 });
    assert.equal(result.error, undefined, result.error?.message);
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.match(result.stdout, /4 passed; 0 failed/);

    const build = readFileSync(source, "utf8");
    assert.match(build, /fn main\(\) \{\s*configure_android_page_size\(\);/);
    assert.match(build, /env::var\("CARGO_CFG_TARGET_OS"\)/);
    assert.match(build, /env::var\("CARGO_CFG_TARGET_POINTER_WIDTH"\)/);
    assert.match(build, /for argument in android_page_size_link_args\(&target_os, &pointer_width\) \{\s*println!\("cargo:rustc-link-arg=\{argument\}"\);/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
