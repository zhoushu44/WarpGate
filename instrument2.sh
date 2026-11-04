#!/bin/bash
set -u
cd /root/warpgate-rs/app
N=crates/wg-tunnel/src/connectip/native.rs
[ -f /root/np/bak/native.rs ] || cp "$N" /root/np/bak/native.rs

if ! grep -q 'pub async fn socket_count' "$N"; then
  awk '
    /^\/\/\/ `host:port`/ && !done { ins=1; done=1 }
    { lines[NR]=$0 }
    END {
      # find the anchor line number
      for (i=NR; i>=1; i--) if (lines[i] ~ /^\/\/\/ `host:port`/) { a=i; break }
      # walk back to the closing brace of the impl block
      j=a-1
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
    }' "$N" > /tmp/native.new && mv /tmp/native.new "$N"
fi

echo "=== result ==="
grep -n 'socket_count' crates/wg-tunnel/src/connectip/stack.rs crates/wg-tunnel/src/connectip/native.rs
md5sum crates/wg-tunnel/src/connectip/stack.rs crates/wg-tunnel/src/connectip/native.rs
echo "--- native.rs diff ---"
diff -u /root/np/bak/native.rs "$N" | head -25
