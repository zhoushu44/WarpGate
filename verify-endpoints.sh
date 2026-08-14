#!/bin/bash
echo "=== 等待恢复完成并验证多endpoint分配 ==="
sleep 90
echo "--- ENDPOINT PER INSTANCE ---"
for d in /root/warp-instances/warp-*/; do
  idx=$(basename $d | sed 's/warp-//')
  ep=$(node -e "const m=require('$d/meta.json');console.log(m.endpoint||'none')" 2>/dev/null)
  ip=$(node -e "const m=require('$d/meta.json');console.log(m.ip||'none')" 2>/dev/null)
  echo "  warp-$idx: endpoint=$ep ip=$ip"
done
echo "--- POOL STATUS ---"
curl -s http://127.0.0.1:4433/api/containers 2>/dev/null | node -e '
let d="";process.stdin.on("data",c=>d+=c).on("end",()=>{
  try {
    const j=JSON.parse(d);
    const list=j.data||j.containers||[];
    if(Array.isArray(list)){
      const endpoints={};
      list.forEach(c=>{const e=c.endpoint||c.meta?.endpoint||"?";endpoints[e]=(endpoints[e]||0)+1;});
      console.log("Total:",list.length,"| Endpoint distribution:",JSON.stringify(endpoints));
      console.log("Unique IPs:",new Set(list.filter(c=>c.ip).map(c=>c.ip)).size);
    } else { console.log("Response:",d.substring(0,300)); }
  }catch(e){console.log("RAW:",d.substring(0,300));}
})'
