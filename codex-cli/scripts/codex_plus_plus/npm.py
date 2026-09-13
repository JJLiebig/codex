ROOT_PACKAGE = "codex-plus-plus"
NPM_NAME = "@jjliebig/codex-plus-plus"
REPOSITORY = {
    "type": "git",
    "url": "git+https://github.com/JJLiebig/codex.git",
    "directory": "codex-cli",
}
PLATFORM_PACKAGES = {
    "codex-plus-plus-linux-x64": {
        "npm_name": "@jjliebig/codex-plus-plus-linux-x64",
        "npm_tag": "linux-x64",
        "target_triple": "x86_64-unknown-linux-musl",
        "os": "linux",
        "cpu": "x64",
    },
    "codex-plus-plus-darwin-arm64": {
        "npm_name": "@jjliebig/codex-plus-plus-darwin-arm64",
        "npm_tag": "darwin-arm64",
        "target_triple": "aarch64-apple-darwin",
        "os": "darwin",
        "cpu": "arm64",
    },
    "codex-plus-plus-win32-x64": {
        "npm_name": "@jjliebig/codex-plus-plus-win32-x64",
        "npm_tag": "win32-x64",
        "target_triple": "x86_64-pc-windows-msvc",
        "os": "win32",
        "cpu": "x64",
    },
}

ROOT_CONFIG = {
    "npm_name": NPM_NAME,
    "repository": REPOSITORY,
    "bin": {"codex": "bin/codex.js", "codex-plus-plus": "bin/codex.js"},
    "platform_packages": PLATFORM_PACKAGES,
    "requires_vendor_src": True,
}
