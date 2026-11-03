#!/usr/bin/perl
use strict;
use warnings;
use File::Copy qw(cp);

my $dir = "/root/warpgate-rs/app/crates/wg-tunnel/src/connectip";
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

my $np = "$dir/native.rs";
my $n = slurp($np);
my $nl = ($n =~ /\r\n/) ? "\r\n" : "\n";

my $nanchor = "/// `host:port` \x{2192} `(host, port)`, tolerating `[v6]:port`.";
my $nidx = index($n, $nanchor);
die "anchor not found in native.rs" if $nidx < 0;

my $close = "}" . $nl . $nl;
my $cidx = rindex($n, $close, $nidx);
die "impl close not found" if $cidx < 0;

my $nmethod = join($nl,
'    /// Number of TCP sockets currently held by this tunnel\'s smoltcp socket set.',
'    ///',
'    /// Diagnostic only: the stress harness uses this to detect socket leaks',
'    /// that /proc/<pid>/fd cannot see, since smoltcp sockets are heap state.',
'    pub async fn socket_count(&self) -> usize {',
'        self.stack.socket_count().await',
'    }',
'}',
'',
''
);

$n = substr($n, 0, $cidx) . $nmethod . substr($n, $nidx);
spew($np, $n);
print "patched ok (nl=" . ($nl eq "\r\n" ? "crlf" : "lf") . ")\n";
