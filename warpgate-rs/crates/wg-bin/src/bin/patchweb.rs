//! Insert host-metric cards into the console's overview page.
//!
//! The page is a single static HTML file served from `public/`; there is no
//! build step, so this edit is applied directly and must stay idempotent.

use std::fs;
use std::path::Path;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "public/index.html".into());
    let path = Path::new(&path);
    let html = fs::read_to_string(path).expect("read index.html");

    if html.contains("sysCpu") {
        println!("already patched");
        return;
    }

    // 1. Cards, appended to the existing stats bar.
    let cards = r#"
           <div class="stat-card">
             <div class="stat-label">CPU</div>
             <div class="stat-value" id="sysCpu">--</div>
           </div>
           <div class="stat-card">
             <div class="stat-label">内存</div>
             <div class="stat-value" id="sysMem">--</div>
           </div>
           <div class="stat-card">
             <div class="stat-label">网络 ↓</div>
             <div class="stat-value" id="sysRx">--</div>
           </div>
           <div class="stat-card">
             <div class="stat-label">网络 ↑</div>
             <div class="stat-value" id="sysTx">--</div>
           </div>
"#;
    let anchor = r#"             <div class="stat-value" id="statSessions">0</div>
           </div>"#;
    let html = html.replace(
        anchor,
        &format!("{anchor}\n{cards}"),
    );

    // 2. Renderer, called from the same place the usage table is filled.
    let renderer = r#"
     function renderSystem(sys) {
       if (!sys) return;
       const set = (id, text) => { const e = document.getElementById(id); if (e) e.textContent = text; };
       const bps = (v) => v >= 1048576 ? (v / 1048576).toFixed(1) + ' MB/s'
                        : v >= 1024 ? (v / 1024).toFixed(1) + ' KB/s'
                        : v + ' B/s';
       set('sysCpu', (sys.cpu_pct || 0).toFixed(1) + '%' + (sys.load1 ? ` (load ${sys.load1.toFixed(2)})` : ''));
       set('sysMem', `${sys.mem_used_mb || 0} / ${sys.mem_total_mb || 0} MB (${(sys.mem_pct || 0).toFixed(1)}%)`);
       set('sysRx', bps(sys.net_rx_bps || 0));
       set('sysTx', bps(sys.net_tx_bps || 0));
     }

     function renderUsage() {"#;
    let html = html.replace("     function renderUsage() {", renderer);

    // 3. Feed it from the usage response.
    let html = html.replace(
        "           usageData = resp.data.days || [];",
        "           usageData = resp.data.days || [];\n           renderSystem(resp.data.system);",
    );

    fs::write(path, html).expect("write index.html");
    println!("patched {}", path.display());
}
