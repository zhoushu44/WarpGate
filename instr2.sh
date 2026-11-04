#!/bin/bash
set -eu
D=/root/np2/instr/crates/wg-tunnel/src/connectip/stack.rs
grep -q 'socket_count' "$D" && { echo "already"; exit 0; }
awk '
{
  if (!done && index($0, "Open a TCP connection to `host:port` and return it as a byte stream")) {
    print "    /// Diagnostic: number of TCP sockets currently held in the socket set."
    print "    pub async fn socket_count(&self) -> usize {"
    print "        self.sockets.lock().await.iter().count()"
    print "    }"
    print ""
    done=1
  }
  print
}' "$D" > /tmp/s2.new && mv /tmp/s2.new "$D"
grep -n 'socket_count' "$D"
