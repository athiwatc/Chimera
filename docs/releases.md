# Binary installation, upgrades, and releases

Chimera ships tested binary archives for **Linux x86_64**, **macOS Apple Silicon (ARM64)** and **Windows x86_64**. Other platforms can [build from source](../README.md#build-from-source). Releases are published at <https://github.com/athiwatc/Chimera/releases> once approved; a tag or download URL shown here is an example, not a claim that it is available.

## Install or upgrade (Linux x86_64 / macOS ARM64)

Select a **published** version explicitly; don't use an unverified "latest" redirect for automated upgrades. The archive includes `chimera` and `README.md`.

```bash
set -euo pipefail
VERSION=v0.1.0 # replace with an actual published release
case "$(uname -s):$(uname -m)" in
  Linux:x86_64) PLATFORM=Linux-X64 ;;
  Darwin:arm64) PLATFORM=macOS-ARM64 ;;
  *) echo "Unsupported binary target: $(uname -s):$(uname -m)" >&2; exit 1 ;;
esac

ROOT="$HOME/.local/opt/chimera"
BIN_DIR="$HOME/.local/bin"
ARCHIVE="chimera-$PLATFORM.tar.gz"
URL="https://github.com/athiwatc/Chimera/releases/download/$VERSION"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
curl --fail --location --retry 3 "$URL/$ARCHIVE" -o "$TMP/$ARCHIVE"
curl --fail --location --retry 3 "$URL/$ARCHIVE.sha256" -o "$TMP/$ARCHIVE.sha256"
(
  cd "$TMP"
  if [ "$(uname -s)" = Darwin ]; then
    shasum -a 256 -c "$ARCHIVE.sha256"
  else
    sha256sum -c "$ARCHIVE.sha256"
  fi
)

# Refuse untrusted archive names and extraction paths.
tar -tzf "$TMP/$ARCHIVE" | sort > "$TMP/entries"
printf './README.md\n./chimera\n' | sort > "$TMP/expected"
# GNU/BSD tar commonly includes "./" as a directory entry.
sed '/^\.\/$/d' "$TMP/entries" > "$TMP/actual"
diff -u "$TMP/expected" "$TMP/actual"

mkdir -p "$ROOT" "$BIN_DIR"
DEST="$ROOT/$VERSION"
if [ -e "$DEST" ]; then
  echo "Version already installed: $DEST (do not overwrite)" >&2
  exit 1
fi
mkdir "$DEST"
tar -xzf "$TMP/$ARCHIVE" -C "$DEST"
chmod +x "$DEST/chimera"
"$DEST/chimera" --help
"$DEST/chimera" register --help

LINK="$BIN_DIR/chimera"
if [ -e "$LINK" ] && [ ! -L "$LINK" ]; then
  echo "Refusing to overwrite non-symlink $LINK" >&2
  exit 1
fi
if [ -L "$LINK" ]; then
  case "$(readlink "$LINK")" in
    "$ROOT"/*/chimera) ;;
    *) echo "Refusing to replace unrelated symlink $LINK" >&2; exit 1 ;;
  esac
fi
ln -s "$DEST/chimera" "$BIN_DIR/.chimera-next-$$"
mv -f "$BIN_DIR/.chimera-next-$$" "$LINK"
"$LINK" --help
```

Ensure `$HOME/.local/bin` is on your shell's `PATH`. On systems where replacing a symlink with `mv` behaves differently, use a verified atomic rename or perform the final switch while clients are stopped. `mv` on a POSIX system replaces a file/symlink in the same directory atomically.

Register once with `"$HOME/.local/bin/chimera" register` from the project directory. Chimera records its absolute executable path. Later upgrades only switch the symlink and **do not** re-register or rewrite configuration. Existing registrations that point directly to a source checkout must be migrated intentionally with `register`, after examining their target project/scope. Restart MCP clients after switching versions.

### Roll back (Linux/macOS)

Keep previously installed version directories. Stop any running Chimera clients, and repoint the symlink to a previously installed and tested version:

```bash
set -euo pipefail
OLD=v0.1.0 # version you previously installed
ROOT="$HOME/.local/opt/chimera"
BIN_DIR="$HOME/.local/bin"
test -x "$ROOT/$OLD/chimera"
test -L "$BIN_DIR/chimera"
case "$(readlink "$BIN_DIR/chimera")" in "$ROOT"/*/chimera) ;; *) exit 1 ;; esac
ln -s "$ROOT/$OLD/chimera" "$BIN_DIR/.chimera-rollback-$$"
mv -f "$BIN_DIR/.chimera-rollback-$$" "$BIN_DIR/chimera"
"$BIN_DIR/chimera" --help
```

A rollback changes only the executable, not MCP registration or user settings. No configuration migration is performed by this installer.

## Windows x86_64 PowerShell installation

Windows uses a stable executable at `%USERPROFILE%\chimera\bin\chimera.exe` and retains versioned binaries in `versions\`. Close clients before upgrading: Windows may prevent replacing a running executable. This procedure keeps a backup of the previously active binary and restores it if the smoke test fails.

```powershell
$ErrorActionPreference = 'Stop'
$Version = 'v0.1.0' # replace with an actual published release
$Platform = 'Windows-X64'
$Archive = "chimera-$Platform.tar.gz"
$BaseUrl = "https://github.com/athiwatc/Chimera/releases/download/$Version"
$Root = Join-Path $HOME 'chimera'
$VersionDir = Join-Path $Root "versions\$Version"
$BinDir = Join-Path $Root 'bin'
$Live = Join-Path $BinDir 'chimera.exe'
$Backup = Join-Path $BinDir 'chimera.previous.exe'
$Stage = Join-Path $BinDir 'chimera.next.exe'
$Temp = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())

New-Item -ItemType Directory -Path $Temp -Force | Out-Null
try {
  Invoke-WebRequest "$BaseUrl/$Archive" -OutFile (Join-Path $Temp $Archive)
  Invoke-WebRequest "$BaseUrl/$Archive.sha256" -OutFile (Join-Path $Temp "$Archive.sha256")
  $ExpectedLine = (Get-Content (Join-Path $Temp "$Archive.sha256") -Raw).Trim()
  if ($ExpectedLine -notmatch '^([0-9a-fA-F]{64})\s+\*?chimera-Windows-X64\.tar\.gz$') {
    throw 'Unexpected checksum file format'
  }
  $Expected = $Matches[1]
  $Actual = (Get-FileHash (Join-Path $Temp $Archive) -Algorithm SHA256).Hash
  if ($Actual -ne $Expected) { throw 'SHA-256 checksum mismatch' }

  $Entries = @(tar -tzf (Join-Path $Temp $Archive))
  if ($LASTEXITCODE -ne 0) { throw 'Unable to list archive' }
  $Entries = @($Entries | Where-Object { $_ -ne './' } | Sort-Object)
  if (($Entries -join '|') -ne './README.md|./chimera.exe') {
    throw 'Unexpected archive contents'
  }

  if (Test-Path $VersionDir) { throw "Version already installed: $VersionDir" }
  New-Item -ItemType Directory -Path $VersionDir -Force | Out-Null
  tar -xzf (Join-Path $Temp $Archive) -C $VersionDir
  if ($LASTEXITCODE -ne 0) { throw 'Extraction failed' }
  & (Join-Path $VersionDir 'chimera.exe') --help
  if ($LASTEXITCODE -ne 0) { throw 'Smoke test failed' }
  & (Join-Path $VersionDir 'chimera.exe') register --help
  if ($LASTEXITCODE -ne 0) { throw 'Registration CLI smoke test failed' }

  New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
  if (Test-Path $Stage) { throw "Staging path already exists: $Stage" }
  Copy-Item (Join-Path $VersionDir 'chimera.exe') $Stage
  if (Test-Path $Backup) { throw "Existing backup: $Backup; preserve or move it first" }

  if (Test-Path $Live) { Move-Item $Live $Backup }
  try {
    Move-Item $Stage $Live
    & $Live --help
    if ($LASTEXITCODE -ne 0) { throw 'New executable smoke test failed' }
  }
  catch {
    if (Test-Path $Live) { Remove-Item $Live -Force }
    if (Test-Path $Backup) { Move-Item $Backup $Live }
    throw
  }
}
finally {
  Remove-Item $Temp -Recurse -Force -ErrorAction SilentlyContinue
}
```

Add `$HOME\chimera\bin` to your user PATH if needed, then run `& "$HOME\chimera\bin\chimera.exe" register` in your project. For rollback, stop clients, rename `chimera.exe` to `chimera.failed.exe`, then rename `chimera.previous.exe` to `chimera.exe` and run `--help`. For older versions, copy the executable from `versions\<version>\chimera.exe` after verifying it. Do not delete the backup until you have verified the upgrade.

## Publish a release (maintainers only)

1. Review and merge the release implementation separately. Confirm successful CI and required branch checks.
2. Update the package version if necessary. Review the exact commit, then **with explicit approval** create/push a `vMAJOR.MINOR.PATCH` tag pointing to a commit on `main`.
3. Allow the existing tag-push CI to finish. It must pass all jobs, including `CI passed`. The artifacts expire after 14 days.
4. **With separate approval**, manually dispatch `Release` using the existing tag and the successful tag-push CI run ID. Configure a protected GitHub Actions environment named `release` with required reviewers and tag deployment restrictions before first use.
5. The release workflow checks that the tag version matches `Cargo.toml`, the commit is on `main`, and CI succeeded on the exact tagged SHA. It downloads the existing Linux, macOS, and Windows artifacts, checks SHA-256 and archive entries, and publishes six immutable-by-policy assets. It will not replace an existing release.
6. Download each platform archive from the published GitHub Release and independently re-check its checksum and CLI help/registration commands.

The release workflow **does not** create a tag, run CI, change registrations, or publish on ordinary pushes/PRs. Note that GitHub Actions workflows ordinarily run automatically on branch pushes/PRs according to existing CI triggers; this workflow adds no automatic release publication. Releases are publicly downloadable beyond workflow artifact retention. GitHub's release edit/delete permissions should be restricted administratively for stronger immutability guarantees.

### Validation checklist

Run locally without invoking GitHub Actions manually:

```sh
cargo +stable fmt --all -- --check
cargo +stable clippy --locked --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo +stable doc --locked --workspace --all-features --no-deps --document-private-items
cargo +stable test --locked --workspace --all-targets --all-features
cargo +1.88.0 test --locked --workspace --all-targets --all-features
cargo +stable build --release --locked
./target/release/chimera --help
./target/release/chimera register --help
cargo audit --file Cargo.lock
go run github.com/rhysd/actionlint/cmd/actionlint@v1.7.12 -color
```

On Windows use `target/release/chimera.exe`. Test successful upgrades, checksum failures, invalid archives, already-installed-version guards, rollback, and persistence of Codex/Claude/OpenCode registrations using isolated profiles/test projects. The existing CI remains responsible for all three platform builds and fixture-based tests.

SHA-256 detects corruption/mismatches but by itself cannot authenticate a compromised publisher. Always download the checksum from the trusted release page over HTTPS. Signed attestations may be introduced in a later enhancement.
