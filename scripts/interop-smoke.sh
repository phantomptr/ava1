#!/usr/bin/env bash
#
# The C example node and the Rust example client end to end: the node starts with nobody
# trusted, the client pairs with the code the node prints, sends a folder, and the copy must be
# byte-identical. A second send needs no code (both sides remember each other).
#
#   scripts/interop-smoke.sh build/ava1-node
set -euo pipefail

node_bin="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d)"
port="${AVA1_SMOKE_PORT:-29120}"
node_pid=""
cleanup() {
	[[ -n "$node_pid" ]] && kill "$node_pid" 2>/dev/null && wait "$node_pid" 2>/dev/null
	rm -rf "$work"
}
trap cleanup EXIT

# Something to send: small files, an empty one, a nested folder, one larger than a chunk.
mkdir -p "$work/src/a/b" "$work/share"
echo hello > "$work/src/hello.txt"
: > "$work/src/empty"
for i in $(seq 1 50); do echo "file $i" > "$work/src/a/b/f$i.txt"; done
head -c 3000000 /dev/urandom > "$work/src/a/big.bin"

"$node_bin" "$work/state" "$work/share" "$port" > "$work/node.out" 2> "$work/node.err" &
node_pid=$!
for _ in $(seq 1 50); do grep -q listening "$work/node.out" 2>/dev/null && break; sleep 0.1; done
grep -q listening "$work/node.out" || { cat "$work/node.err"; echo "the node did not start" >&2; exit 1; }

cargo build -q --manifest-path "$root/Cargo.toml" -p ava1 --example send_to_node
client="$root/target/debug/examples/send_to_node"
share="$(cd "$work/share" && pwd -P)"

# First send: the node shows a code; feed it to the client once it appears.
(
	for _ in $(seq 1 100); do
		code=$(sed -n 's/.*code \([0-9]*\).*/\1/p' "$work/node.out" | tail -1)
		[[ -n "$code" ]] && { echo "$code"; exit 0; }
		sleep 0.1
	done
	echo "no pairing code" >&2
	exit 1
) | "$client" "127.0.0.1:$port" "$work/src" "$share/first" "$work/client"
diff -r "$work/src" "$share/first"

# Second send: already paired, nothing on standard input.
"$client" "127.0.0.1:$port" "$work/src" "$share/second" "$work/client" < /dev/null
diff -r "$work/src" "$share/second"
echo "interop smoke: C node + Rust client OK (paired, two sends verified)"
