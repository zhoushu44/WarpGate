#!/bin/bash
# Build an instrumented copy of the tree in a SEPARATE directory, so the
# deployed source at /root/warpgate-rs/app keeps its md5 untouched.
set -eu
SRC=/root/warpgate-rs/app
DST=/root/np2/instr
rm -rf "$DST"
mkdir -p "$DST"
# Copy only the crate sources + manifests; reuse the deployed target dir via
# CARGO_TARGET_DIR so we don't recompile the world.
cd "$SRC"
tar -c --exclude=target --exclude=.git . 2>/dev/null | tar -x -C "$DST"
echo "copied: $(find "$DST" -name '*.rs' | wc -l) rs files"
md5sum "$SRC/crates/wg-tunnel/src/connectip/stack.rs"

# inject read-only counters
awk '
/^    \/\/\/ Open a TCP connection to `host:port` and return it as a byte stream\.$/ && !done {
  print "    /// Diagnostic: number of TCP sockets currently held in the socket set."
  print "    pub async fn socket_count(&self) -> usize {"
  print "        self.sockets.lock().await.iter().count()"
  print "    }"
  print ""
  done=1
}
{ print }
' "$DST/crates/wg-tunnel/src/connectip/stack.rs" > /tmp/s.new && mv /tmp/s.new "$DST/crates/wg-tunnel/src/connectip/stack.rs"

N="$DST/crates/wg-tunnel/src/connectip/native.rs"
python_awk() {
awk '
/^\/\/\/ `host:port`/ && !done { ins=NR; done=1 }
{ lines[NR]=$0 }
END {
  j=ins-1
  while (j>=1 && lines[j] ~ /^[[:space:]]*$/) j--
  for (i=1; i<=NR; i++) {
    if (i==j) {
      print "    "
      print "    /// Diagnostic: SocketSet size for this tunnel."
      print "    pub async fn socket_count(&self) -> usize {"
      print "        self.stack.socket_count().await"
      print "    }"
    }
    print lines[i]
  }
}' "$N"
}
python_awk > /tmp/n.new && mv /tmp/n.new "$N"

echo "=== injected ==="
grep -n 'socket_count' "$DST/crates/wg-tunnel/src/connectip/stack.rs" "$N"
echo "=== deployed source UNCHANGED? ==="
md5sum "$SRC/crates/wg-tunnel/src/connectip/stack.rs"
