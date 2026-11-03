#!/bin/bash
# Turn nativepress TSV into per-scenario statistics. bash/awk only.
set -u
for f in "$@"; do
  name=$(basename "$f" .tsv)
  echo "=== $name ==="
  awk -F'\t' '
    $1=="REQ" {
      n++; tot+=$5
      if ($4==1) { ok++; lat[okc++]=$5 }
      else { fail++; if ($8=="") $8="unknown"; why[$8]++ }
    }
    END {
      printf "total=%d ok=%d fail=%d\n", n, ok, fail
      # sort latencies
      for (i=0;i<okc;i++) for (j=i+1;j<okc;j++) if (lat[j]<lat[i]) {t=lat[i];lat[i]=lat[j];lat[j]=t}
      if (okc>0) {
        printf "P50=%dms P95=%dms max=%dms min=%dms mean=%.1fms\n", \
          lat[int((okc-1)*0.50)], lat[int((okc-1)*0.95)], lat[okc-1], lat[0], tot/n
      }
      for (k in why) printf "  fail_reason %-20s %d\n", k, why[k]
    }
  ' "$f"
done
