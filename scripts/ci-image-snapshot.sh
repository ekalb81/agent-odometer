#!/usr/bin/env bash
set -euo pipefail
workspace=$(realpath --relative-to="$HOME" "$GITHUB_WORKSPACE")
[[ "$workspace" != .. && "$workspace" != ../* && "$workspace" != /* ]]
bundle="$RUNNER_TEMP/ci-image-snapshot"
mkdir -p "$bundle"
case "$1" in
  pack)
    mkdir -p "$HOME/.cargo/registry" "$HOME/.cargo/git" "$HOME/.npm/_cacache"
    # Build artifacts are large; prefer quick compression over smaller transfers.
    # Both measured arms still restore these identical, checksummed gzip bytes.
    tar -I 'gzip -1' -cf "$bundle/snapshot.tar.gz" -C "$HOME" \
      .cargo/registry .cargo/git .npm/_cacache "$workspace/src-tauri/target" "$workspace/output/ci-image/snapshot.json"
    (cd "$bundle" && sha256sum snapshot.tar.gz > snapshot.sha256)
    ;;
  restore)
    # Artifact archive preserves the tar bytes; GNU tar preserves inner modes and links.
    grep -Eq '^[a-f0-9]{64}  snapshot\.tar\.gz$' "$bundle/snapshot.sha256"
    test "$(wc -l < "$bundle/snapshot.sha256")" = 1
    (cd "$bundle" && sha256sum -c snapshot.sha256)
    python3 - "$bundle/snapshot.tar.gz" "$workspace" "$HOME" <<'PY'
import posixpath, sys, tarfile
prefixes=('.cargo/registry', '.cargo/git', '.npm/_cacache', sys.argv[2]+'/src-tauri/target', sys.argv[2]+'/output/ci-image/snapshot.json')
def allowed(name):
    return not name.startswith('/') and '..' not in name.split('/') and any(name == p or name.startswith(p+'/') for p in prefixes)
with tarfile.open(sys.argv[1]) as archive:
    for member in archive:
        name=member.name
        if not allowed(name) or not (member.isfile() or member.isdir() or member.issym() or member.islnk()):
            raise ValueError('Snapshot contains an unexpected extraction path')
        if member.issym() or member.islnk():
            link=member.linkname
            if link.startswith(sys.argv[3]+'/'): link=link[len(sys.argv[3])+1:]
            elif not link.startswith('/') and member.issym(): link=posixpath.join(posixpath.dirname(name),link)
            if not allowed(posixpath.normpath(link)):
                raise ValueError('Snapshot link escapes the reviewed cache directories')
PY
    tar -xzf "$bundle/snapshot.tar.gz" -C "$HOME" --same-permissions
    echo 'CI_IMAGE_SNAPSHOT_RESTORED=true' >> "$GITHUB_ENV"
    ;;
  *) exit 2 ;;
esac
digest=$(cut -d' ' -f1 "$bundle/snapshot.sha256")
echo "CI_IMAGE_SNAPSHOT_SHA=$digest" >> "$GITHUB_ENV"
echo "CI_IMAGE_SNAPSHOT_BYTES=$(stat -c%s "$bundle/snapshot.tar.gz")" >> "$GITHUB_ENV"
