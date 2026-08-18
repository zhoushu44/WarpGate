/**
 * 一次性远程诊断/执行助手（node remote.js '<shell命令>'）
 */
const { Client } = require('ssh2');
const cmd = process.argv[2];
if (!cmd) { console.error('用法: node remote.js "<命令>"'); process.exit(1); }
const conn = new Client();
conn.on('ready', async () => {
  conn.exec(cmd, (e, s) => {
    if (e) { console.error(e.message); conn.end(); process.exit(1); }
    let o = '';
    s.on('data', d => process.stdout.write(d));
    s.stderr.on('data', d => process.stdout.write(d));
    s.on('close', (code) => { conn.end(); process.exit(code || 0); });
  });
}).connect({
  host: '192.6.121.16', port: 22, username: 'root', password: '6Qz6ao0T1zvL', readyTimeout: 20000,
});
conn.on('error', (e) => { console.error('SSH 失败: ' + e.message); process.exit(1); });
