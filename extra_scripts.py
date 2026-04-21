Import("env")
import subprocess
import os

try:
    version = subprocess.check_output(
        ["git", "describe", "--always", "--tags", "--dirty"],
        stderr=subprocess.DEVNULL,
        cwd=env.subst("$PROJECT_DIR"),
    ).strip().decode()
except Exception:
    version = "unknown"

include_dir = os.path.join(env.subst("$PROJECT_DIR"), "firmware", "include")
os.makedirs(include_dir, exist_ok=True)
with open(os.path.join(include_dir, "version.h"), "w") as f:
    f.write(f'#pragma once\n#define FIRMWARE_VERSION "{version}"\n')

print(f"Firmware version: {version}")
