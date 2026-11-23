#!/bin/sh
# Suite dependency guard, the same script in every tetron suite client: every
# git dependency on a github.com/tetronos repository names a release tag, and
# the tetron-proto tag has this crate's minor (tetron 0.15.x for a 0.15.x
# client). A bare git dependency lets Cargo.lock freeze any commit, so two
# clients could speak different protocol revisions to the same daemon.
set -eu
cd "$(dirname "$0")/.."
own=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
minor() { echo "$1" | sed -E 's/^v?([0-9]+\.[0-9]+).*/\1/'; }
deps=$(grep -E '^[A-Za-z0-9_-]+ = \{.*git = "https://github\.com/tetronos/' Cargo.toml || true)
fail=0
while IFS= read -r line; do
	[ -n "$line" ] || continue
	name=${line%% *}
	tag=$(echo "$line" | sed -nE 's/.*tag = "([^"]+)".*/\1/p')
	if [ -z "$tag" ]; then
		echo "FAIL: $name is a git dependency without a tag"; fail=1
	elif [ "$name" = tetron-proto ] && [ "$(minor "$tag")" != "$(minor "$own")" ]; then
		echo "FAIL: tetron-proto tag $tag is not this crate's minor $(minor "$own")"; fail=1
	fi
done <<DEPS
$deps
DEPS
[ "$fail" = 0 ] || exit 1
echo "ok: suite git dependencies are tagged, tetron-proto is on this minor"
