"""Vérification et capture ciblées de Qoder, sans relancer les autres QA."""
import json
import os
import pathlib
import subprocess

ROOT = pathlib.Path(__file__).resolve().parent.parent
QA = ROOT / "qa"
PROFILE = QA / "qoder-profile"
EXE = pathlib.Path(os.environ.get("TOKENSCOPE_EXE", ROOT / "target" / "release" / "tokenscope.exe"))
SCREENSHOT = QA / "qoder.png"
CREATE_NO_WINDOW = 0x08000000


def run(*args):
    subprocess.run(
        [str(EXE), "--data-dir", str(PROFILE), *map(str, args)],
        cwd=ROOT, check=True, timeout=60, capture_output=True,
        creationflags=CREATE_NO_WINDOW,
    )


if not EXE.is_file():
    raise SystemExit("Compilez d'abord : cargo build --release --locked")
PROFILE.mkdir(exist_ok=True)
config = {
    "schema_version": 3,
    "providers": [{"id": "qoder", "name": "Qoder", "kind": "Qoder", "enabled": True,
                   "online": True, "log_path": "", "auth_path": "", "key_env": "",
                   "color": [120, 174, 250], "limits": []}],
    "refresh_seconds": 60,
    "alert_percent": 80,
}
(PROFILE / "config.json").write_text(json.dumps(config), encoding="utf-8")
run("--check")
report = json.loads((PROFILE / "diagnostic.json").read_text(encoding="utf-8"))
provider = next((p for p in report["providers"] if p["id"] == "qoder"), None)
if not provider or not provider["live"] or not provider["quotas"]:
    raise SystemExit("Qoder indisponible : vérifiez Qoder CLI et sa connexion avant la capture.")
run("--provider", "qoder", "--screenshot", SCREENSHOT)
if not SCREENSHOT.is_file() or SCREENSHOT.stat().st_size < 10_000:
    raise SystemExit("La capture Qoder n'a pas été produite correctement.")
print("Qoder connecté ; capture créée :", SCREENSHOT)
