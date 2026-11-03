#!/bin/bash
# Add a read-only diagnostic getter (NO behavioural change) so the harness can
# report the exact SocketSet size instead of inferring it from RSS.
set -eu
cd /root/warpgate-rs/app
S=crates/wg-tunnel/src/connectip/stack.rs
N=crates/wg-tunnel/src/connectip/native.rs
cp -n "$S" /root/np/stack.rs.orig
cp -n "$N" /root/np/native.rs.orig
md5sum "$S" "$N"

if ! grep -q 'pub async fn socket_count' "$S"; then
python3 - "$S" <<'EOF' 2>/dev/null || perl -0777 -i -pe 's/(    \/\/\/ Open a TCP connection to `host:port`)/    \/\/\/ Number of TCP sockets currently held in the socket set (diagnostic only).\n    pub async fn socket_count(\&self) -> usize {\n        self.sockets.lock().await.iter().count()\n    }\n\n$1/s' "$S"
import sys
p = sys.argv[1]
s = open(p).read()
anchor = "    /// Open a TCP connection to `host:port` and return it as a byte stream."
add = '''    /// Number of TCP sockets currently held in the socket set (diagnostic only).
    pub async fn socket_count(&self) -> usize {
        self.sockets.lock().await.iter().count()
    }

'''
assert anchor in s
s = s.replace(anchor, add + anchor, 1)
open(p, "w").write(s)
EOF
fi

if ! grep -q 'pub async fn socket_count' "$N"; then
python3 - "$N" <<'EOF' 2>/dev/null || perl -0777 -i -pe 's/(\n}\n\n\/\/\/ `host:port`)/\n    \/\/\/ Diagnostic: SocketSet size for this tunnel.\n    pub async fn socket_count(\&self) -> usize {\n        self.stack.socket_count().await\n    }\n}\n\n\/\/\/ `host:port`/s' "$N"
import sys
p = sys.argv[1]
s = open(p).read()
anchor = "\n}\n\n/// `host:port`"
add = '''
    /// Diagnostic: SocketSet size for this tunnel.
    pub async fn socket_count(&self) -> usize {
        self.stack.socket_count().await
    }
}
'''
idx = s.index(anchor)
s = s[:idx] + add + s[idx + len("\n}\n"):]
open(p, "w").write(s)
EOF
fi

echo "=== after ==="
grep -n 'socket_count' "$S" "$N"
md5sum "$S" "$N"
diff -u /root/np/stack.rs.orig "$S" | head -20
diff -u /root/np/native.rs.orig "$N" | head -20
