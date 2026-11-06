"""Include the managed activator in verified native payloads for fresh installs."""

from pathlib import Path
import shutil


def add_installer(package: Path, *, windows: bool) -> None:
    name = "install-codex-plus-plus." + ("ps1" if windows else "sh")
    source = Path(__file__).resolve().parents[2] / "install" / name
    destination = package / "codex-resources" / "install"
    destination.mkdir(parents=True)
    shutil.copyfile(source, destination / name)
