#!/usr/bin/env python3
"""Build the on-chain BFA Android artifact locally; never publish or open a wallet."""
from pathlib import Path
import hashlib
import json
import os
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
RGB_REVISION = "3f571ac58909400046d69143ebb535f4da1f4d42"
VERSION = "0.17.0-bfa.local.2"


def run(args, **kwargs):
    subprocess.run(args, cwd=ROOT, check=True, **kwargs)


def main():
    env = os.environ.copy()
    if not (env.get("ANDROID_NDK_HOME") or env.get("ANDROID_NDK_ROOT")):
        raise SystemExit("Set ANDROID_NDK_HOME to Android NDK 27 or later.")
    env.setdefault("ANDROID_NDK_HOME", env.get("ANDROID_NDK_ROOT", ""))
    env.setdefault("CARGO_PROFILE_RELEASE_OPT_LEVEL", "1")
    env.setdefault("CARGO_PROFILE_RELEASE_LTO", "false")
    env.setdefault("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "16")
    env.setdefault("CARGO_PROFILE_RELEASE_DEBUG", "0")
    env.setdefault("CARGO_INCREMENTAL", "0")
    target = Path(env.get("CARGO_TARGET_DIR", ROOT / "target")).resolve()
    run(["cargo", "ndk", "--target", "arm64-v8a", "--platform", "24", "rustc",
         "--release", "--locked", "--features", "uniffi,vls", "--lib", "-j", "2",
         "--", "-C", "strip=none", "-C", "link-arg=-Wl,-z,max-page-size=16384"], env=env)
    native = target / "aarch64-linux-android/release/librgb_lightning_node.so"
    generated = ROOT / "target/uniffi/kotlin-android"
    temporary_udl = ROOT / "src/rgb_lightning_node.udl"
    created_udl = not temporary_udl.exists()
    if created_udl:
        shutil.copy2(ROOT / "bindings/rgb_lightning_node.udl", temporary_udl)
    try:
        run(["cargo", "run", "--locked", "--manifest-path",
             "bindings/uniffi-bindgen/Cargo.toml", "--", "generate", str(native),
             "--library", "--crate", "rgb_lightning_node", "--language", "kotlin",
             "--config", "bindings/bfa-android/uniffi.toml", "-o", str(generated)], env=env)
    finally:
        if created_udl:
            temporary_udl.unlink()
    jni = generated / "jniLibs/arm64-v8a"
    jni.mkdir(parents=True, exist_ok=True)
    shutil.copy2(native, jni / native.name)
    run([env.get("GRADLE", "gradle"), "-p", "bindings/bfa-android", "assembleRelease",
         "--no-daemon", "--max-workers=2", "--console=plain"], env=env)
    built = ROOT / "bindings/bfa-android/build/outputs/aar/bfa-android-release.aar"
    output = ROOT / "target/bfa-android-artifact"
    output.mkdir(parents=True, exist_ok=True)
    artifact = output / f"rgb-lightning-node-android-{VERSION}.aar"
    shutil.copy2(built, artifact)
    metadata = {
        "rgb_lib_sha": RGB_REVISION,
        "rln_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "aar_sha256": hashlib.sha256(artifact.read_bytes()).hexdigest(),
        "native_sha256": hashlib.sha256(native.read_bytes()).hexdigest(),
        "abi": "arm64-v8a", "artifact_version": VERSION,
        "vss": False, "lightning_runtime": False,
        "rgb_proxy_protocol": "1.0", "local_only": True,
    }
    (output / "native-provenance.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(f"Built local artifact: {output}")


if __name__ == "__main__":
    main()
