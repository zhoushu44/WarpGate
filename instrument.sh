#!/bin/bash
# TEMPORARY instrumentation: add read-only SocketSet counters. Restored afterwards.
set -u
cd /root/warpgate-rs/app
S=crates/wg-tunnel/src/connectip/stack.rs
N=crates/wg-tunnel/src/connectip/native.rs
md5sum "$S" "$N"
mkdir -p /root/np/bak
[ -f /root/np/bak/stack.rs ] || cp "$S" /root/np/bak/stack.rs
[ -f /root/np/bak/native.rs ] || cp "$N" /root/np/bak/native.rs

# --- stack.rs: inject getter before the connect() doc comment ---
if ! grep -q 'pub async fn socket_count' "$S"; then
  perl -0777 -pe 's{^(    /// Open a TCP connection to `host:port` and return it as a byte stream\.)$}{    /// Diagnostic: number of TCP sockets currently held in the socket set.\n    pub async fn socket_count(\&self) -> usize {\n        self.sockets.lock().await.iter().count()\n    }\n\n$1}m' "$S" > /tmp/stack.new && mv /tmp/stack.new "$S"
fi

# --- native.rs: passthrough inside impl NativeTunnel, right before the closing brace
#     of the impl that contains dial_fn. Anchor on the split_target doc comment.
if ! grep -q 'pub async fn socket_count' "$N"; then
  perl -0777 -pe 's{^\n\}\n\n/// `host:port` — }{"\n    /// Diagnostic: SocketSet size for this tunnel.\n    pub async fn socket_count(&self) -> usize {\n        self.stack.socket_count().await\n    }\n}\n\n/// `host:port` — "}me' "$N" > /tmp/native.new && mv /tmp/native.new "$N"
fi

echo "=== result ==="
grep -n 'socket_count' "$S" "$N" || echo "INJECTION FAILED"
md5sum "$S" "$N"
echo "--- native.rs diff ---"
diff -u /root/np/bak/native.rs "$N" | head -20
