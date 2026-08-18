/**
 * 一次性工具：用 base64 走 SSH stdin 上传文件（绕过 SFTP 偶发 ENOENT）
 * 用法: node .zcode/upload-b64.js <本地文件> <远程文件>
 */
const path = require('path');
const fs = require('fs');
const { Client } = require('ssh2');
const [local, remote] = process.argv.slice(2);
if (!local || !remote) { console.error('用法: node upload-b64.js <本地> <远程>'); process.exit(1); }
const b64 = fs.readFileSync(path.resolve(local)).toString('base64');
console.log(`上传 ${local} (${Math.round(b64.length / 1024)}KB base64) → ${remote}`);
const conn = new Client();
let done = false;
conn.on('ready', () => {
  conn.exec(`mkdir -p $(dirname "${remote}") && base64 -d > "${remote}" && md5sum "${remote}"`, (err, stream) => {
    if (err) { console.error(err.message); conn.end(); process.exit(1); }
    stream.stderr.on('data', d => process.stdout.write(d));
    stream.on('close', (code) => {
      done = true;
      conn.end();
      process.exit(code || 0);
    });
    stream.stdin.end(b64);
  });
}).connect({ host: '192.6.121.16', port: 22, username: 'root', password: '6Qz6ao0T1zvL', readyTimeout: 20000 });
conn.on('error', (e) => { if (!done) { console.error('SSH 失败: ' + e.message); process.exit(1); } });