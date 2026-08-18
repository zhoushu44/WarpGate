/**
 * Node 版部署脚本（替代 Posh-SSH 的 deploy-v3.ps1）
 * SFTP 上传 server.js + public/*.html → 语法检查 → systemd 重启 → 状态确认
 * 用法: node deploy.js
 */
const path = require('path');
const fs = require('fs');
const { Client } = require('ssh2');

const HOST = '192.6.121.16';
const USER = 'root';
const PASS = '6Qz6ao0T1zvL';
const LOCAL_DIR = __dirname;
const REMOTE_DIR = '/root/warp';

const files = [
  { local: 'server.js', remote: `${REMOTE_DIR}/server.js` },
  { local: 'public/index.html', remote: `${REMOTE_DIR}/public/index.html` },
  { local: 'public/docs.html', remote: `${REMOTE_DIR}/public/docs.html` },
];

const conn = new Client();

function exec(cmd, timeout = 30000) {
  return new Promise((resolve, reject) => {
    conn.exec(cmd, (err, stream) => {
      if (err) return reject(err);
      let out = '', errOut = '';
      const timer = setTimeout(() => { stream.close(); reject(new Error('命令超时: ' + cmd)); }, timeout);
      stream.on('data', d => out += d);
      stream.stderr.on('data', d => errOut += d);
      stream.on('close', (code) => {
        clearTimeout(timer);
        resolve({ code, out: out.trim(), err: errOut.trim() });
      });
    });
  });
}

function upload(sftp, local, remote) {
  return new Promise((resolve, reject) => {
    sftp.fastPut(path.join(LOCAL_DIR, local), remote, (err) => {
      if (err) reject(err); else resolve();
    });
  });
}

conn.on('ready', async () => {
  try {
    console.log('[1/4] SSH 连接成功');
    await exec(`mkdir -p ${REMOTE_DIR}/public`);

    console.log('[2/4] 上传文件...');
    const sftp = await new Promise((resolve, reject) => {
      conn.sftp((err, sftp) => err ? reject(err) : resolve(sftp));
    });
    for (const f of files) {
      if (!fs.existsSync(path.join(LOCAL_DIR, f.local))) throw new Error('本地文件不存在: ' + f.local);
      await upload(sftp, f.local, f.remote);
      console.log(`       ✓ ${f.local} → ${f.remote}`);
    }

    console.log('[3/4] 服务器语法检查...');
    const check = await exec(`cd ${REMOTE_DIR} && node --check server.js && echo SYNTAX_OK`);
    console.log('       ' + (check.out || check.err));
    if (!check.out.includes('SYNTAX_OK')) throw new Error('语法检查失败: ' + check.err);

    console.log('[4/4] 重启服务...');
    const restart = await exec('systemctl restart warp && sleep 8 && systemctl is-active warp && journalctl -u warp -n 8 --no-pager | tail -8', 60000);
    console.log(restart.out || restart.err);

    console.log('\nDEPLOY DONE');
    conn.end();
    process.exit(0);
  } catch (e) {
    console.error('部署失败: ' + e.message);
    conn.end();
    process.exit(1);
  }
}).connect({
  host: HOST, port: 22, username: USER, password: PASS,
  readyTimeout: 20000,
});

conn.on('error', (e) => {
  console.error('SSH 连接失败: ' + e.message);
  process.exit(1);
});
