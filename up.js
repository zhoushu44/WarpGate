/**
 * 上传文件到生产机（node up.js <本地路径> <远程路径>）
 * 用读写流方式传输，避免 fastPut 在某些相对路径下误判文件不存在。
 */
const path = require('path');
const fs = require('fs');
const { Client } = require('ssh2');
const [local, remote] = process.argv.slice(2);
if (!local || !remote) { console.error('用法: node up.js <本地> <远程>'); process.exit(1); }
const absLocal = path.resolve(local);
if (!fs.existsSync(absLocal)) { console.error('本地文件不存在: ' + absLocal); process.exit(1); }
const conn = new Client();
conn.on('ready', () => {
  conn.sftp((err, sftp) => {
    if (err) { console.error('sftp失败: ' + err.message); process.exit(1); }
    sftp.mkdir(path.dirname(remote), { recursive: true }, () => {
      const rs = fs.createReadStream(absLocal);
      const ws = sftp.createWriteStream(remote);
      let done = false;
      const finish = () => { if (!done) { done = true; console.log(`✓ ${absLocal} → ${remote}`); conn.end(); process.exit(0); } };
      const fail = (who, e) => { if (!done) { done = true; console.error(`上传失败 [${who}] ${e && e.message ? e.message : e}`); conn.end(); process.exit(1); } };
      ws.on('close', finish);
      ws.on('error', (e) => fail('write', e));
      rs.on('error', (e) => fail('read', e));
      rs.pipe(ws);
    });
  });
}).connect({ host: '192.6.121.16', port: 22, username: 'root', password: '6Qz6ao0T1zvL', readyTimeout: 20000 });