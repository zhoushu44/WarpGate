#!/usr/bin/perl
use strict;
use warnings;
use File::Copy qw(cp);

my $dir = "/root/warpgate-rs/app/crates/wg-tunnel/src/connectip";
cp("$dir/stack.rs", "/tmp/stack.rs.before_diag") or die "backup stack: $!";
cp("$dir/native.rs", "/tmp/native.rs.before_diag") or die "backup native: $!";

sub slurp {
    my ($p) = @_;
    open(my $fh, '<', $p) or die "$p: $!";
    local $/;
    my $c = <$fh>;
    close $fh;
    return $c;
}
sub spew {
    my ($p, $c) = @_;
    open(my $fh, '>', $p) or die "$p: $!";
    print $fh $c;
    close $fh;
}

# ---- stack.rs: expose socket-set size ----
my $sp = "$dir/stack.rs";
my $s = slurp($sp);
my $anchor = '    /// Open a TCP connection to';
my $idx = index($s, $anchor);
die "anchor not found in stack.rs" if $idx < 0;

my $method = <<'RUST';
    /// Number of TCP sockets currently held in the socket set.
    ///
    /// Diagnostic only: a socket that is never removed keeps its 128 KiB of
    /// buffers alive for the life of the tunnel, which /proc/<pid>/fd cannot
    /// show because none of it is a file descriptor.
    pub async fn socket_count(&self) -> usize {
        self.sockets.lock().await.iter().count()
    }

RUST
$s = substr($s, 0, $idx) . $method . substr($s, $idx);
spew($sp, $s);

# ---- native.rs: forward it ----
my $np = "$dir/native.rs";
my $n = slurp($np);
my $nanchor = "/// `host:port` \x{2192} `(host, port)`, tolerating `[v6]:port`.";
my $nidx = index($n, $nanchor);
die "anchor not found in native.rs" if $nidx < 0;
# back up over the closing "}\n\n" of the impl block
my $close = "}\n\n";
my $cidx = rindex($n, $close, $nidx);
die "impl close not found" if $cidx < 0;
my $nmethod = <<'RUST';
    /// Number of TCP sockets currently held by this tunnel's smoltcp socket set.
    ///
    /// Diagnostic only: the stress harness uses this to detect socket leaks
    /// that /proc/<pid>/fd cannot see, since smoltcp sockets are heap state.
    pub async fn socket_count(&self) -> usize {
        self.stack.socket_count().await
    }
}

RUST
$n = substr($n, 0, $cidx) . $nmethod . substr($n, $nidx);
spew($np, $n);

print "patched ok\n";
