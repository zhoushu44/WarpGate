// WarpGate 版本（发布递增）
const WARPGATE_VERSION = '11.1';
const express = require('express');
const { exec, execSync, spawn, spawnSync } = require('child_process');
const path = require('path');
const fs = require('fs');
const net = require('net');
const crypto = require('crypto');
const logger = require('./logger');

// ==================== 配置管理 ====================
const CONFIG_PATH = path.join(__dirname, 'config.json');
const INSTANCES_DIR = '/root/warp-instances';
const SINGBOX_BIN = '/usr/local/bin/sing-box';
const WGCF_BIN = '/usr/local/bin/wgcf';
const USQUE_BIN = '/usr/local/bin/usque';
// MASQUE 传输（warp_transport=masque）时每实例的内部 SOCKS 引擎端口基准（本机回环使用，不与公网端口冲突）
const ENGINE_PORT_BASE = 20000;
const WARP_ENDPOINT_IP = '188.114.96.1';
const WARP_ENDPOINT_PORT = 2408;
const WARP_PUBLIC_KEY = 'bmXOC+F1FxEMF9dyiK2H5/1SUtzH0JuVo51h2wPfgyo=';
// 候选WARP对端（endpoint），同一账号连接不同endpoint会得到不同出口IP
// 实测可用: 188.114.96.1 / 188.114.97.1 / 188.114.96.2 / 188.114.96.3
// 注: 162.159.192.1 实测链路不通（L1 出口检测全部超时），已从列表移除
const WARP_ENDPOINTS = [
  { address: '188.114.96.1', port: 2408 },
  { address: '188.114.97.1', port: 2408 },
  { address: '188.114.96.2', port: 2408 },
  { address: '188.114.96.3', port: 2408 },
];

const DEFAULT_CONFIG = {
  port: 4433,
  max_containers: 1,
  resident_count: 5,
  sticky_minutes: 60,
  auto_rebuild_on_unhealthy: true,
  health_check_interval: 600,
  bind_address: '0.0.0.0',
  mode: 'resident',
  base_port: 10010,
  max_instances: 200,
  min_available: 20,
  max_concurrent_create: 3,
  ip_check_url: 'https://api.ipify.org',
  ip_check_url_v6: 'https://api64.ipify.org',
  openai_check_url: 'https://chatgpt.com',
  ip_check_retries: 10,
  ip_check_interval: 3000,
  max_rebuild_attempts: 3,
  cleanup_on_exit: false,
  rate_limit_enabled: true,
  rate_limit_per_minute: 60,
  auto_rotate_on_high_403: true,
  cf_403_threshold: 50,
  cf_min_check_count: 3,
  // 出口IP风控值检测（scamalytics，带浏览器UA抓取；风控值≥阈值触发换IP）
  ip_risk_check: true,
  ip_risk_threshold: 70,
  ip_risk_ttl: 21600, // 风控值缓存有效期（秒），6小时
  ip_risk_check_url: 'https://scamalytics.com/ip/',
  // WARP 传输方式: 'wireguard'（默认, sing-box 端点）| 'masque'（usque, 绕开 WireGuard UDP 封禁）
  warp_transport: 'wireguard',
  // MASQUE 端点策略：部分服务器网络会封锁 CF 注册下发的默认端点(162.159.198.2 TCP/UDP 全丢包)，
  // masque_endpoint_v4/h2_v4 非空时强制使用（手动兜底）；留空则按 masque_endpoint_candidates
  // 顺序自动探测（用 usque 本体做金丝雀，真实发一次请求能取回出口 IP 才算可用），
  // 取第一个可用 IP 写入 usque.json
  masque_endpoint_v4: '',
  masque_endpoint_h2_v4: '',
  masque_endpoint_candidates: ['162.159.198.3', '162.159.198.4', '162.159.198.2', '162.159.198.5', '162.159.198.6', '162.159.198.7', '162.159.198.8', '162.159.198.1'],
  log_level: 'info',
  public_ip: 'auto-detect',
  session_cleanup_interval: 30,
  // WARP Teams (Zero Trust) 无限流量: 填写团队Token后注册的账号为Teams账户(无限流量)
  // 获取地址: https://web--public--warp-team-api--coia-mfs4.code.run/
  team_token: '',
  // 接入模式（商用对齐）
  proxy_auth_enabled: true,     // 账密模式: SOCKS5 端口用户名密码认证（false=端口免密开放）
  proxy_username: 'warpuser',   // 全局代理用户名（全部实例共用一套账密）
  proxy_password: '',           // 全局代理密码（首次启动自动生成，可修改）
  api_key: 'zs1236547',         // 管理台登录 Key（API Key，初始统一 zs1236547，可在 config.json 修改）
  ip_whitelist: [],             // 白名单: 源IP免密用代理白名单端口 + 免Key调API
  whitelist_port_offset: 1000,  // 白名单端口 = base_port + offset + 实例序号
};

function loadConfig() {
  try {
    if (fs.existsSync(CONFIG_PATH)) {
      const saved = JSON.parse(fs.readFileSync(CONFIG_PATH, 'utf8'));
      return { ...DEFAULT_CONFIG, ...saved };
    }
  } catch (e) {
    console.error('配置加载失败，使用默认配置: ' + e.message);
  }
  return { ...DEFAULT_CONFIG };
}

function saveConfig() {
  try {
    fs.writeFileSync(CONFIG_PATH, JSON.stringify(config, null, 2));
    logger.info('CONFIG', '配置已保存');
  } catch (e) {
    logger.error('CONFIG', '配置保存失败: ' + e.message);
  }
}

// 白名单匹配器（提前声明，rebuildWhitelistMatcher 在配置加载后立即调用）
let wlBlockList = null;

const config = loadConfig();
logger.setLogLevel(config.log_level);
normalizeWhitelistEntries();
// 全局代理密码：首次启动自动生成
if (!config.proxy_password) {
  config.proxy_password = crypto.randomBytes(9).toString('base64url');
  try { fs.writeFileSync(CONFIG_PATH, JSON.stringify(config, null, 2)); } catch (e) { }
}
// 管理台登录 Key：缺失或旧占位符时统一为初始 Key（zs1236547），管理员可在 config.json 修改
if (!config.api_key || config.api_key === 'auto-generate-on-first-run') {
  config.api_key = 'zs1236547';
  try { fs.writeFileSync(CONFIG_PATH, JSON.stringify(config, null, 2)); } catch (e) { }
}
rebuildWhitelistMatcher();

// 全局代理账密（全部实例共用，商用形态：一套用户名密码 + 任一端口）
function getProxyCreds() {
  return {
    username: config.proxy_username || 'warpuser',
    password: config.proxy_password,
  };
}

// 首次启动时保存默认配置到文件
if (!fs.existsSync(CONFIG_PATH)) {
  saveConfig();
}

// ==================== 公网IP检测 ====================
let publicIP = '127.0.0.1';

async function detectPublicIP() {
  if (config.public_ip === 'auto-detect') {
    try {
      const ip = await runCmd('curl -s --max-time 10 https://api.ipify.org', 15000);
      if (ip && /^\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(ip.trim())) {
        publicIP = ip.trim();
        logger.info('CONFIG', '公网IP自动检测成功: ' + publicIP);
        return;
      }
    } catch (e) {
      logger.warn('CONFIG', '公网IP自动检测失败: ' + e.message);
    }
    publicIP = '127.0.0.1';
    logger.warn('CONFIG', '公网IP检测失败，回退到 127.0.0.1');
  } else {
    publicIP = config.public_ip;
    logger.info('CONFIG', '使用配置的公网IP: ' + publicIP);
  }
}

// ==================== 工具函数 ====================
function runCmd(cmd, timeoutMs = 120000) {
  return new Promise((resolve, reject) => {
    exec(cmd, { maxBuffer: 1024 * 1024 * 10, timeout: timeoutMs }, (err, stdout, stderr) => {
      if (err) {
        reject(new Error(stderr || err.message));
      } else {
        resolve(stdout.trim());
      }
    });
  });
}

function runCmdFast(cmd, timeoutMs = 10000) {
  return runCmd(cmd, timeoutMs);
}

function sleep(ms) {
  return new Promise(r => setTimeout(r, ms));
}

async function isPortAvailable(port) {
  return new Promise((resolve) => {
    const tester = net.createServer()
      .once('error', () => resolve(false))
      .once('listening', () => { tester.close(); resolve(true); })
      .listen(port);
  });
}

// 输入校验
function validateContainerName(name) {
  return /^warp-\d+$/.test(name);
}

function validatePort(port) {
  const p = parseInt(port);
  const maxPort = config.base_port + config.max_instances;
  return !isNaN(p) && p >= config.base_port && p < maxPort;
}

// ==================== 统一响应（商用风格） ====================
// 成功: HTTP 200 {"code":"00000","msg":"","data":{...}}
// 业务错误: HTTP 200 {"code":"Exxxx","msg":"...","data":null}
// 鉴权/限流/系统错误保留 HTTP 状态码（401/429/500）
function success(res, data) {
  res.json({ code: '00000', msg: '', data });
}

function error(res, code, message, httpStatus = 200) {
  res.status(httpStatus).json({
    code,
    msg: message,
    data: null,
  });
}

const ERROR_CODES = {
  OK: '00000',              // 成功
  UNAUTHORIZED: 'A0001',    // API Key 缺失或无效（HTTP 401）
  RATE_LIMITED: 'A0002',    // 请求频率超限（HTTP 429）
  NO_AVAILABLE_PROXY: 'P0001', // 无可用代理
  SESSION_NOT_FOUND: 'S0001',  // 会话不存在或已过期
  INVALID_INPUT: 'I0001',   // 参数校验失败
  NOT_FOUND: 'I0002',       // 资源不存在
  DEDICATED_BOUND: 'D0001', // 实例已被固定IP绑定
  INSTANCE_ERROR: 'R0001',  // 实例操作失败（HTTP 500）
  IP_CHECK_FAILED: 'R0002', // IP 检测失败
  INTERNAL_ERROR: '99999',  // 系统异常（HTTP 500）
};

// ==================== Express 应用 ====================
const app = express();

app.use((req, res, next) => {
  req.setTimeout(0);
  res.setTimeout(0);
  next();
});

app.use(express.json());
app.use(express.static(path.join(__dirname, 'public'), {
  etag: false,
  lastModified: false,
  setHeaders: (res) => {
    res.setHeader('Cache-Control', 'no-cache, no-store, must-revalidate');
  }
}));

// 请求日志
app.use((req, res, next) => {
  const start = Date.now();
  res.on('finish', () => {
    const ms = Date.now() - start;
    logger.debug('REQUEST', `${req.method} ${req.path} ${res.statusCode} ${ms}ms`);
  });
  next();
});

// ==================== API 访问鉴权（白名单模式） ====================
// 仅白名单内的源IP可调用 API（免Key）；服务器本机(127.0.0.1/::1)始终放行便于运维
function normalizeClientIP(ip) {
  if (!ip) return '';
  // IPv6-mapped IPv4 (::ffff:1.2.3.4 → 1.2.3.4)
  if (ip.startsWith('::ffff:')) {
    const v4 = ip.slice(7);
    if (/^\d{1,3}(\.\d{1,3}){3}$/.test(v4)) return v4;
  }
  return ip;
}

function rebuildWhitelistMatcher() {
  const list = wlEntries();
  if (!list.length) { wlBlockList = null; return; }
  const bl = new net.BlockList();
  for (const entry of list) {
    try {
      if (entry.includes('/')) {
        const [addr, bits] = entry.split('/');
        bl.addSubnet(addr.trim(), parseInt(bits));
      } else if (entry.includes(':')) {
        bl.addAddress(entry.trim(), 'ipv6');
      } else {
        bl.addAddress(entry.trim(), 'ipv4');
      }
    } catch (e) {
      logger.warn('WHITELIST', `白名单条目无效已跳过: ${entry}`);
    }
  }
  wlBlockList = bl;
}

function getClientIP(req) {
  return normalizeClientIP(req.ip || (req.socket && req.socket.remoteAddress) || '');
}

function isClientWhitelisted(req) {
  const ip = getClientIP(req);
  if (!ip) return false;
  if (ip === '127.0.0.1' || ip === '::1') return true; // 本机始终放行
  if (!wlBlockList) return false;
  try {
    return wlBlockList.check(ip, ip.includes(':') ? 'ipv6' : 'ipv4');
  } catch (e) {
    return false;
  }
}

// 白名单条目统一为 {ip, remark, region, added_at}（兼容旧字符串格式自动迁移）
function normalizeWhitelistEntries() {
  if (!Array.isArray(config.ip_whitelist)) { config.ip_whitelist = []; return; }
  config.ip_whitelist = config.ip_whitelist.map(e =>
    typeof e === 'string' ? { ip: e, remark: '', region: '', added_at: null } : e
  );
}
function wlEntries() {
  return (Array.isArray(config.ip_whitelist) ? config.ip_whitelist : []).map(e => typeof e === 'string' ? e : e.ip);
}

// 白名单转 sing-box source_ip_cidr（裸IP补全为 /32 或 /128）
function whitelistToCidrs() {
  return wlEntries().map(e => e.includes('/') ? e : (e.includes(':') ? `${e}/128` : `${e}/32`));
}

// IP归属地查询（ip-api.com 免费接口，失败返回空）
async function lookupIpRegion(ip) {
  if (!ip || ip.includes('/')) return ip && ip.includes('/') ? '网段' : '';
  if (ip === '127.0.0.1' || ip === '::1') return '本机';
  try {
    const r = await runCmdFast(`curl -s --max-time 5 "http://ip-api.com/json/${ip}?lang=zh-CN&fields=status,country,regionName,city"`, 8000);
    const j = JSON.parse(r);
    if (j && j.status === 'success') {
      return [j.country, j.regionName, j.city].filter(Boolean).join(' ');
    }
  } catch (e) { }
  return '';
}

// ==================== 管理台登录 Token（无状态 HMAC，浏览器 localStorage 持久化） ====================
const ADMIN_SESSION_TTL = 30 * 24 * 3600 * 1000; // 30 天
function sessionSecret() { return 'warp-admin:' + (config.api_key || ''); }
function signSessionToken() {
  const payload = Buffer.from(JSON.stringify({ u: 'admin', iat: Date.now() })).toString('base64url');
  const sig = crypto.createHmac('sha256', sessionSecret()).update(payload).digest('base64url');
  return payload + '.' + sig;
}
function verifySessionToken(token) {
  if (!token) return false;
  const parts = String(token).split('.');
  if (parts.length !== 2) return false;
  const expect = crypto.createHmac('sha256', sessionSecret()).update(parts[0]).digest('base64url');
  if (parts[1] !== expect) return false;
  try {
    const p = JSON.parse(Buffer.from(parts[0], 'base64url').toString('utf8'));
    return !!(p && p.u === 'admin' && p.iat && (Date.now() - p.iat) < ADMIN_SESSION_TTL);
  } catch (e) { return false; }
}
function getTokenFromReq(req) {
  const h = req.headers['authorization'] || '';
  if (h.startsWith('Bearer ')) return h.slice(7).trim();
  return req.headers['x-auth-token'] || '';
}
// 代理提取类接口：维持白名单规则（白名单内免Key），登录 token 不放行
const PROXY_EXTRACT_PATHS = ['/api/proxies', '/api/pool/acquire', '/api/pool/release', '/api/pool/report'];
function isProxyExtractPath(p) {
  return PROXY_EXTRACT_PATHS.some(x => p === x || p.startsWith(x + '/'));
}

function apiKeyAuth(req, res, next) {
  // Express 挂载 app.use('/api', ...) 后 req.path 是相对路径（不含 /api 前缀），需用 baseUrl 还原完整路径
  const fullPath = (req.baseUrl || '') + req.path;
  if (fullPath === '/api/login') return next(); // 登录接口免鉴权
  if (isClientWhitelisted(req)) return next();  // 白名单/本机：全部放行（免Key）
  const ip = getClientIP(req);
  // 管理类接口：登录 token 放行；代理提取类接口维持白名单规则（token 不放行）
  if (!isProxyExtractPath(fullPath) && verifySessionToken(getTokenFromReq(req))) {
    return next();
  }
  logger.warn('AUTH', `鉴权失败 ${req.method} ${fullPath} 来源:${ip || 'unknown'}（不在白名单/未登录）`);
  const hint = isProxyExtractPath(fullPath)
    ? '代理提取接口仅白名单内IP免Key调用；登录后请在「白名单管理」页添加你的IP'
    : '请先登录管理台（输入 API Key）';
  return res.status(401).json({
    code: ERROR_CODES.UNAUTHORIZED,
    msg: '来源IP不在白名单或未登录',
    data: { client_ip: ip, need_login: !isProxyExtractPath(fullPath), hint },
  });
}

// ==================== 限流 ====================
const rateLimitMap = new Map();

function rateLimit(req, res, next) {
  if (!config.rate_limit_enabled) return next();
  const key = getClientIP(req) || 'anonymous';
  const now = Date.now();
  if (!rateLimitMap.has(key)) {
    rateLimitMap.set(key, { count: 1, resetTime: now + 60000 });
    return next();
  }
  const record = rateLimitMap.get(key);
  if (now > record.resetTime) {
    record.count = 1;
    record.resetTime = now + 60000;
    return next();
  }
  if (record.count >= config.rate_limit_per_minute) {
    res.setHeader('Retry-After', '60');
    return error(res, ERROR_CODES.RATE_LIMITED, `请求频率超限（${config.rate_limit_per_minute}/分钟），请稍后重试`, 429);
  }
  record.count++;
  next();
}

// 全部 /api/* 接口启用鉴权；代理池提取与管理类接口另加限流
app.use('/api', apiKeyAuth);
app.use('/api/pool', rateLimit);
app.use('/api/create', rateLimit);
app.use('/api/delete', rateLimit);
app.use('/api/rebuild', rateLimit);
app.use('/api/start', rateLimit);
app.use('/api/stop', rateLimit);

// ==================== 管理台登录（API Key → 会话 Token） ====================
const loginAttempts = new Map(); // ip -> {fail, lockedUntil}
const LOGIN_MAX_FAILS = 5;
const LOGIN_LOCK_MS = 60 * 1000;

app.post('/api/login', (req, res) => {
  const ip = getClientIP(req) || 'unknown';
  const now = Date.now();
  const rec = loginAttempts.get(ip) || { fail: 0, lockedUntil: 0 };
  if (now < rec.lockedUntil) {
    const wait = Math.ceil((rec.lockedUntil - now) / 1000);
    return error(res, ERROR_CODES.RATE_LIMITED, `登录失败次数过多，请 ${wait} 秒后重试`, 429);
  }
  const key = (req.body && req.body.key) || '';
  if (key !== config.api_key) {
    rec.fail++;
    if (rec.fail >= LOGIN_MAX_FAILS) {
      rec.lockedUntil = now + LOGIN_LOCK_MS;
      rec.fail = 0;
      loginAttempts.set(ip, rec);
      logger.warn('AUTH', `登录失败过多已锁定 ${ip} 60s`);
      return error(res, ERROR_CODES.RATE_LIMITED, '登录失败次数过多，请 60 秒后重试', 429);
    }
    loginAttempts.set(ip, rec);
    logger.warn('AUTH', `管理台登录失败 来源:${ip}`);
    return res.status(401).json({ code: ERROR_CODES.UNAUTHORIZED, msg: 'API Key 无效', data: { client_ip: ip } });
  }
  loginAttempts.delete(ip);
  const token = signSessionToken();
  logger.info('AUTH', `管理台登录成功 来源:${ip}`);
  return success(res, {
    token,
    expires_in: Math.floor(ADMIN_SESSION_TTL / 1000),
    api_key_hint: config.api_key ? config.api_key.slice(0, 2) + '****' : '',
  });
});

// ==================== 页面路由（静态页，免鉴权） ====================
app.get('/docs', (req, res) => res.sendFile(path.join(__dirname, 'public', 'docs.html')));
// 白名单管理已并入控制台（#whitelist 标签页），旧地址重定向
app.get('/whitelist', (req, res) => res.redirect(301, '/#whitelist'));
app.get('/key', (req, res) => res.redirect(301, '/#whitelist'));

// ==================== 任务队列 ====================
const jobs = {};
let jobCounter = 0;

function createJob(type, total) {
  const id = ++jobCounter;
  jobs[id] = {
    id, type, status: 'running', current: 0, total,
    logs: [], results: [], createdAt: Date.now(),
  };
  return jobs[id];
}

function addLog(job, msg, level = 'info') {
  const entry = { time: new Date().toISOString(), msg, level };
  job.logs.push(entry);
  if (job.logs.length > 500) job.logs.shift();
  logger.info('JOB', `[Job ${job.id}] ${msg}`);
}

// ==================== 用量统计（数据概览） ====================
// 按天累计：提取/粘性/释放/上报/换IP/绑定/解绑 次数，持久化 usage.json
const USAGE_PATH = path.join(__dirname, 'usage.json');
let usage = { days: {} };
let usageSaveTimer = null;

function loadUsage() {
  try {
    if (fs.existsSync(USAGE_PATH)) usage = JSON.parse(fs.readFileSync(USAGE_PATH, 'utf8'));
  } catch (e) { usage = { days: {} }; }
}
loadUsage();

function saveUsageSoon() {
  if (usageSaveTimer) return;
  usageSaveTimer = setTimeout(() => {
    usageSaveTimer = null;
    try { fs.writeFileSync(USAGE_PATH, JSON.stringify(usage, null, 2)); } catch (e) { }
  }, 3000);
}

function bumpUsage(field, n = 1) {
  const day = new Date().toISOString().slice(0, 10);
  if (!usage.days[day]) {
    usage.days[day] = { acquires: 0, sticky: 0, releases: 0, reports: 0, rotates: 0, binds: 0, unbinds: 0 };
  }
  usage.days[day][field] = (usage.days[day][field] || 0) + n;
  saveUsageSoon();
}

// 固定IP绑定历史（长效静态IP页「历史记录」数据源，持久化在 usage.json）
function pushBindHistory(item) {
  if (!Array.isArray(usage.bindHistory)) usage.bindHistory = [];
  usage.bindHistory.unshift(item);
  if (usage.bindHistory.length > 500) usage.bindHistory.length = 500;
  saveUsageSoon();
}

function closeBindHistory(name) {
  if (!Array.isArray(usage.bindHistory)) return;
  const rec = usage.bindHistory.find(r => r.name === name && !r.unbound_at);
  if (rec) {
    rec.unbound_at = new Date().toISOString();
    saveUsageSoon();
  }
}

// ==================== 代理池数据结构 ====================
// pool: Map<key, { container, instance_index, host_port, ip, health, status }>
// sessions: Map<session_id, { session_id, pool_key, acquired_at, expires_at }>
const pool = new Map();
const sessions = new Map();
let roundRobinIdx = 0;
let healthCheckTimer = null;
let sessionCleanupTimer = null;

// ==================== WARP对端(endpoint)优选 ====================
// 实测结论: 同一账号连接不同endpoint → 出口IP不同（IPv4/IPv6均不同）
// 通过轮询分配endpoint, 单账号可扩展出 账号数×endpoint数 个不同出口IP
let availableEndpoints = [...WARP_ENDPOINTS]; // 探测后更新的可用列表
let endpointProbeInProgress = false;
let recoveryInProgress = false; // 启动恢复进行中：热更新任务等待其完成，避免重启进程打断健康检查

// 测试单个endpoint的UDP连通性（快速）
function testEndpointUDP(ep) {
  return new Promise((resolve) => {
    const { exec: execProbe } = require('child_process');
    execProbe(`timeout 3 bash -c "echo -n x > /dev/udp/${ep.address}/${ep.port}" 2>/dev/null && echo OK`, { timeout: 6000 }, (err, stdout) => {
      resolve(!!(stdout && stdout.includes('OK')));
    });
  });
}

// 全量探测endpoint可用性（UDP层快速过滤，启动时/创建时调用）
async function probeEndpoints() {
  if (endpointProbeInProgress) return;
  endpointProbeInProgress = true;
  try {
    logger.info('ENDPOINT', '开始探测WARP对端可用性...');
    const results = await Promise.all(WARP_ENDPOINTS.map(async (ep) => {
      const ok = await testEndpointUDP(ep);
      return { ep, ok };
    }));
    const okList = results.filter(r => r.ok).map(r => r.ep);
    if (okList.length > 0) {
      availableEndpoints = okList;
      logger.info('ENDPOINT', `可用WARP对端(${okList.length}个): ${okList.map(e => e.address + ':' + e.port).join(', ')}`);
    } else {
      availableEndpoints = [...WARP_ENDPOINTS];
      logger.warn('ENDPOINT', '探测无可用对端，使用默认列表');
    }
  } catch (e) {
    logger.warn('ENDPOINT', 'endpoint探测异常: ' + e.message);
  } finally {
    endpointProbeInProgress = false;
  }
}

// 按实例索引轮询分配endpoint（相邻实例用不同endpoint → 出口IP更分散）
function getEndpointForInstance(instanceIndex) {
  if (!availableEndpoints.length) availableEndpoints = [...WARP_ENDPOINTS];
  return availableEndpoints[instanceIndex % availableEndpoints.length];
}

// 换IP时轮换到下一个endpoint（避免同一endpoint换IP后仍相同出口）
function nextEndpointForInstance(instanceIndex) {
  if (!availableEndpoints.length) availableEndpoints = [...WARP_ENDPOINTS];
  const base = instanceIndex % availableEndpoints.length;
  // 换IP时跳过一个，尝试不同的endpoint
  return availableEndpoints[(base + 1) % availableEndpoints.length];
}

// 异步锁（代理池级别）
let _poolLock = Promise.resolve();
function acquirePoolLock() {
  const next = _poolLock;
  let release;
  _poolLock = new Promise(r => { release = r; });
  return next.then(() => release);
}

// ==================== IP检测 ====================
// 内部检测走实例 SOCKS5 端口：账密模式开启时必须携带该实例凭据，否则被秒拒
function socksProxyFlag(port) {
  if (config.proxy_auth_enabled) {
    const idx = port - config.base_port;
    const { username, password } = getProxyCreds();
    return `--proxy socks5h://${username}:${password}@127.0.0.1:${port}`;
  }
  return `--socks5-hostname 127.0.0.1:${port}`;
}

// L1: 连通性检测 - 强制IPv6出口（每个WARP账号有独立IPv6地址，返回不同IP），回退IPv4
// 返回: { ip: 主出口IP(string|null), v6: IPv6出口(string|null), v4: IPv4出口(string|null) }
async function checkExitIP(port) {
  const tag = `L1[端口${port}]`;
  logger.debug('IP_CHECK', `${tag} 开始IP检测 (最多${config.ip_check_retries}次)`);

  for (let i = 0; i < config.ip_check_retries; i++) {
    const attempt = i + 1;
    const t0 = Date.now();
    try {
      // 强制IPv6: 每个WARP账号分配独立IPv6地址，出口IP必然不同
      // 注意: 代理由 socksProxyFlag 提供（账密模式带凭据），api6.ipify.org 仅AAAA→走IPv6出口
      const ipPrimary = await runCmdFast(
        `curl -s --max-time 10 ${socksProxyFlag(port)} ${config.ip_check_url_v6}`, 12000
      ).catch(() => '');
      // 回退: IPv4出口（共享NAT，仅用于连通性确认）
      const ipFallback = await runCmdFast(
        `curl -s --max-time 10 ${socksProxyFlag(port)} ${config.ip_check_url}`, 12000
      ).catch(() => '');
      const elapsed = Date.now() - t0;
      const primaryTrim = ipPrimary.trim();
      const fallbackTrim = ipFallback.trim();
      
      // 优先使用IPv6结果（每个WARP账号唯一IPv6地址）
      if (primaryTrim && (primaryTrim.includes(':') || /^\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(primaryTrim))) {
        logger.info('IP_CHECK', `${tag} 第${attempt}次成功 → 出口IP:${primaryTrim} (api4:${fallbackTrim}) (${elapsed}ms)`);
        const v4 = /^\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(primaryTrim) ? primaryTrim
          : (/^\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(fallbackTrim) ? fallbackTrim : null);
        return { ip: primaryTrim, v6: primaryTrim.includes(':') ? primaryTrim : null, v4 };
      }
      
      // 回退到IPv4（仅当v6始终不可用时才兜底）
      if (fallbackTrim && /^\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(fallbackTrim)) {
        if (!primaryTrim) {
          // v6 未就绪（WireGuard 隧道握手需要数秒），继续重试 v6，IPv4 仅作最终兜底
          logger.debug('IP_CHECK', `${tag} 第${attempt}次 v6未就绪(仅v4)，继续等待v6...`);
          if (i < config.ip_check_retries - 1) {
            await sleep(config.ip_check_interval);
            continue;
          }
        }
        logger.info('IP_CHECK', `${tag} 第${attempt}次成功(回退IPv4) → IP:${fallbackTrim} (v6:${primaryTrim || '无'}) (${elapsed}ms)`);
        return { ip: fallbackTrim, v6: null, v4: fallbackTrim };
      }
      
      logger.warn('IP_CHECK', `${tag} 第${attempt}次返回非IP: v6="${primaryTrim.substring(0, 40)}" v4="${fallbackTrim.substring(0, 40)}" (${elapsed}ms)`);
    } catch (e) {
      const elapsed = Date.now() - t0;
      logger.warn('IP_CHECK', `${tag} 第${attempt}/${config.ip_check_retries}次失败 (${elapsed}ms): ${e.message}`);
    }
    
    if (i < config.ip_check_retries - 1) {
      logger.debug('IP_CHECK', `${tag} 等待${config.ip_check_interval}ms后重试...`);
      await sleep(config.ip_check_interval);
    }
  }
  
  logger.error('IP_CHECK', `${tag} 全部${config.ip_check_retries}次重试均失败，IP检测失败`);
  return { ip: null, v6: null, v4: null };
}

// L2: CF可访问性检测（返回HTTP状态码，0=连接失败）
async function checkOpenAIAccess(port) {
  const tag = `L2[端口${port}]`;
  const url = config.openai_check_url;
  const t0 = Date.now();
  
  try {
    logger.debug('IP_CHECK', `${tag} 开始CF检测 → ${url} (超时15s)`);
    
    const result = await runCmd(
      `curl -s -o /dev/null -w "%{http_code}" --max-time 15 ${socksProxyFlag(port)} ${url}`,
      20000
    );
    
    const elapsed = Date.now() - t0;
    const code = result.trim();
    const codeNum = parseInt(code) || 0;
    
    if (codeNum === 0) {
      logger.warn('IP_CHECK', `${tag} CF检测失败: 连接失败 (curl输出:"${code}", ${elapsed}ms)`);
    } else if (codeNum === 403) {
      logger.info('IP_CHECK', `${tag} CF检测: 403 (被CF拦截, ${elapsed}ms)`);
    } else if (codeNum === 200 || codeNum === 302) {
      logger.info('IP_CHECK', `${tag} CF检测: ${codeNum} (正常放行, ${elapsed}ms)`);
    } else {
      logger.warn('IP_CHECK', `${tag} CF检测: ${codeNum} (非预期状态码, ${elapsed}ms)`);
    }
    
    return codeNum;
  } catch (e) {
    const elapsed = Date.now() - t0;
    logger.error('IP_CHECK', `${tag} CF检测异常 (${elapsed}ms): ${e.message}`);
    return 0;
  }
}

// ==================== L1.5: 出口IP风控值检测（scamalytics） ====================
// scamalytics 对无UA/短UA的curl返回403，必须带完整浏览器UA
const BROWSER_UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36';
const ipRiskCache = new Map(); // ip -> { score, risk, ts }

async function checkIpRiskScore(ip, port) {
  if (!ip || !config.ip_risk_check) return null;

  // 缓存命中（同IP在TTL内不重复请求）
  const cached = ipRiskCache.get(ip);
  if (cached && Date.now() - cached.ts < config.ip_risk_ttl * 1000) {
    return cached;
  }

  const tag = `RISK[${ip.substring(0, 24)}]`;
  try {
    const t0 = Date.now();
    const url = `${config.ip_risk_check_url}${ip}`;
    // 走实例出口检测，与业务出口一致（服务器直连易被 scamalytics 反爬拦截）
    const proxy = port ? socksProxyFlag(port) : '';
    // scamalytics 对新 IP 首访返回验证页（无 "Fraud Score"），用 cookie jar 持久化 + 重试绕过
    const cookieJar = '/tmp/warp-risk-cookie.txt';
    let html = '';
    for (let attempt = 1; attempt <= 2; attempt++) {
      html = await runCmdFast(
        `curl -s --max-time 12 -c ${cookieJar} -b ${cookieJar} -A "${BROWSER_UA}" ${proxy} ${url}`, 15000
      ).catch(() => '');
      if (/Fraud Score:\s*\d+/.test(html)) break;
      if (attempt === 1) await sleep(3000);
    }

    const scoreMatch = html.match(/Fraud Score:\s*(\d+)/);
    const riskMatch = html.match(/"risk"\s*:\s*"(\w+)"/);
    const score = scoreMatch ? parseInt(scoreMatch[1]) : null;

    if (score === null) {
      logger.warn('RISK', `${tag} 解析失败 (HTML长度:${html.length})`);
      return null;
    }

    const result = {
      score,
      risk: riskMatch ? riskMatch[1] : (score >= 70 ? 'high' : score >= 40 ? 'medium' : 'low'),
      ts: Date.now(),
    };
    ipRiskCache.set(ip, result);
    logger.info('RISK', `${tag} 风控值=${score} (${result.risk}) (${Date.now() - t0}ms)`);
    return result;
  } catch (e) {
    logger.error('RISK', `${tag} 检测异常: ${e.message}`);
    return null;
  }
}

function getRiskLabel(risk) {
  return risk === 'high' ? '高风险' : risk === 'medium' ? '中风险' : '低风险';
}

// 实例空闲后执行待处理的自动换IP
function rotatePendingIfAvailable(poolKey) {
  const entry = pool.get(poolKey);
  if (!entry || !entry.pending_rotate || entry.status !== 'available' || entry.rotate_running) return;

  // 固定IP绑定实例：清除待换标记，绝不自动换IP
  if (entry.dedicated) {
    entry.pending_rotate = false;
    logger.warn('DEDICATED', `[${entry.container}:${entry.instance_index}] 固定IP实例跳过待处理自动换IP`);
    return;
  }

  entry.pending_rotate = false;
  entry.rotate_running = true;
  setImmediate(async () => {
    try {
      logger.info('CF_USAGE', `[${entry.container}:${entry.instance_index}] 实例已空闲，执行待处理的403自动换IP`);
      await rotateInstanceIP(poolKey);
    } catch (e) {
      logger.error('CF_USAGE', `[${entry.container}:${entry.instance_index}] 待处理换IP异常: ${e.message}`);
    } finally {
      entry.rotate_running = false;
    }
  });
}

// 提取代理后的后台CF探测（不阻塞acquire响应）
async function probeCfOnAcquire(poolKey, sessionId) {
  const entry = pool.get(poolKey);
  if (!entry || entry.cf_probe_running) return;

  entry.cf_probe_running = true;
  const tag = `[${entry.container}:${entry.instance_index}]`;
  try {
    logger.info('CF_USAGE', `${tag} 会话${sessionId.substring(0, 8)}... 提取后开始后台CF探测`);
    const httpCode = await checkOpenAIAccess(entry.host_port);

    // 探测连接失败（引擎僵死/隧道挂起）：立即重启引擎，坏出口不留给客户端
    if (httpCode === 0 && isMasque()) {
      const connFails = (entry.conn_fail_count || 0) + 1;
      entry.conn_fail_count = connFails;
      logger.warn('CF_USAGE', `${tag} 探测连接失败 x${connFails} (HTTP:0)，${connFails >= 2 ? '重启引擎' : '记录在案'}`);
      if (connFails >= 2) {
        restartInstanceEngine(entry.instance_index, `acquire探测连续${connFails}次失败`);
        entry.conn_fail_count = 0;
        entry.health = 'unknown'; // 等下次巡检验证
      }
      return;
    }
    entry.conn_fail_count = 0;

    if (entry.cf_check_count === undefined) {
      entry.cf_check_count = 0;
      entry.cf_403_count = 0;
      entry.cf_last_code = 0;
    }
    entry.cf_check_count++;
    entry.cf_last_code = httpCode;
    if (httpCode === 403) entry.cf_403_count++;

    const rate = Math.round(entry.cf_403_count / entry.cf_check_count * 100);
    logger.info('CF_USAGE', `${tag} 使用时探测 HTTP:${httpCode}，403统计 ${entry.cf_403_count}/${entry.cf_check_count}=${rate}%`);

    if (config.auto_rotate_on_high_403
        && !entry.dedicated
        && entry.cf_check_count >= config.cf_min_check_count
        && rate >= config.cf_403_threshold) {
      entry.pending_rotate = true;
      logger.warn('CF_USAGE', `${tag} 403率${rate}%达到阈值${config.cf_403_threshold}%，标记为自动换IP`);
      rotatePendingIfAvailable(poolKey);
    } else if (entry.dedicated && rate >= config.cf_403_threshold) {
      logger.warn('DEDICATED', `${tag} 固定IP实例403率${rate}%达到阈值，跳过自动换IP（保持绑定IP不变）`);
    }
  } catch (e) {
    logger.error('CF_USAGE', `${tag} 使用时CF探测异常: ${e.message}`);
  } finally {
    entry.cf_probe_running = false;
  }
}

// 综合健康检测
async function checkInstanceHealth(poolKey) {
  const entry = pool.get(poolKey);
  if (!entry) return;

  const tag = `[${entry.container}:${entry.instance_index}]`;
  const t0 = Date.now();
  logger.info('HEALTH_CHECK', `${tag} ===== 开始健康检测 (端口${entry.host_port}, 当前IP:${entry.ip || 'null'}, 状态:${entry.status}) =====`);

  // 初始化CF统计
  if (entry.cf_check_count === undefined) {
    entry.cf_check_count = 0;
    entry.cf_403_count = 0;
    entry.cf_last_code = 0;
  }

  // ---- L1: IP连通性检测 ----
  logger.debug('HEALTH_CHECK', `${tag} L1阶段: 检测IP连通性...`);
  const exit = await checkExitIP(entry.host_port);
  const ip = exit.ip;
  entry.ipv6 = exit.v6;
  entry.ipv4 = exit.v4;
  
  if (!ip) {
    const elapsed = Date.now() - t0;
    entry.health = 'unhealthy';
    logger.warn('HEALTH_CHECK', `${tag} 判定: 不健康 ❌ (L1失败-IP不可达, ${elapsed}ms) → 当前IP:${entry.ip} 健康度:${entry.health}`);
    if (entry.dedicated) {
      logger.warn('DEDICATED', `${tag} 固定IP实例L1失败，跳过自动重建（保持绑定IP不变），请人工检查`);
    } else if (config.auto_rebuild_on_unhealthy && entry.status === 'available') {
      logger.info('HEALTH_CHECK', `${tag} 触发自动重建 (auto_rebuild_on_unhealthy=true)`);
      await rotateInstanceIP(poolKey);
    }
    return;
  }

  logger.info('HEALTH_CHECK', `${tag} L1通过: IP=${ip} (之前: ${entry.ip || 'null'})`);

  // 更新IP（如果变化）
  if (entry.ip !== ip) {
    logger.info('HEALTH_CHECK', `${tag} IP已变化: ${entry.ip} → ${ip}`);
    entry.ip = ip;
  }

  // ---- L1.5: 出口IP风控值检测 ----
  if (config.ip_risk_check) {
    logger.debug('HEALTH_CHECK', `${tag} L1.5阶段: 检测出口IP风控值...`);
    const risk = await checkIpRiskScore(ip, entry.host_port);
    entry.ip_risk = risk ? risk.score : null;
    entry.ip_risk_label = risk ? getRiskLabel(risk.risk) : null;

    if (risk && risk.score >= config.ip_risk_threshold) {
      logger.warn('HEALTH_CHECK', `${tag} 出口IP风控值${risk.score}(${getRiskLabel(risk.risk)}) ≥ 阈值${config.ip_risk_threshold}，判定为风控IP，触发自动换IP`);
      if (entry.dedicated) {
        logger.warn('DEDICATED', `${tag} 固定IP实例风控值${risk.score}超阈值，跳过自动换IP（保持绑定IP不变）`);
      } else if (entry.status === 'available' && !entry.rotate_running) {
        const ok = await rotateInstanceIP(poolKey);
        if (ok) {
          logger.info('HEALTH_CHECK', `${tag} 风控值触发换IP成功`);
        }
      }
      return; // 风控IP不参与CF检测，换完IP后等下次巡检验证
    }
    logger.info('HEALTH_CHECK', `${tag} L1.5通过: 风控值=${entry.ip_risk} (${entry.ip_risk_label})`);
  }

  // ---- L2: CF可访问性检测 ----
  logger.debug('HEALTH_CHECK', `${tag} L2阶段: 检测CF可访问性...`);
  const httpCode = await checkOpenAIAccess(entry.host_port);
  entry.cf_check_count++;
  entry.cf_last_code = httpCode;

  if (httpCode === 403) {
    entry.cf_403_count++;
  }

  const cf403Rate = entry.cf_check_count > 0 ? (entry.cf_403_count / entry.cf_check_count * 100).toFixed(0) : 0;
  const cfStats = `CF统计: ${entry.cf_403_count}/${entry.cf_check_count} = ${cf403Rate}%`;

  // ---- 决策 ----
  if (httpCode === 0) {
    // L2连接失败
    const elapsed = Date.now() - t0;
    entry.health = 'unhealthy';
    logger.warn('HEALTH_CHECK', `${tag} 判定: 不健康 ❌ (L2失败-CF不可达, HTTP:0, ${elapsed}ms) → IP:${ip} ${cfStats}`);
    if (entry.dedicated) {
      logger.warn('DEDICATED', `${tag} 固定IP实例L2失败，跳过自动重建（保持绑定IP不变），请人工检查`);
    } else if (config.auto_rebuild_on_unhealthy && entry.status === 'available') {
      logger.info('HEALTH_CHECK', `${tag} 触发自动重建 (auto_rebuild_on_unhealthy=true)`);
      await rotateInstanceIP(poolKey);
    }
    return;
  }

  if (httpCode === 200 || httpCode === 302) {
    // 正常放行
    entry.health = 'healthy';
    const elapsed = Date.now() - t0;
    logger.info('HEALTH_CHECK', `${tag} 判定: 健康 ✅ (CF:${httpCode}放行, ${elapsed}ms) → IP:${ip} ${cfStats}`);
  } else if (httpCode === 403) {
    // 被CF拦截但能连上
    entry.health = 'healthy';
    const elapsed = Date.now() - t0;
    logger.warn('HEALTH_CHECK', `${tag} 判定: 健康(但CF拦截) ⚠️ (CF:403, ${elapsed}ms) → IP:${ip} ${cfStats}`);
  } else {
    // 非预期状态码
    const elapsed = Date.now() - t0;
    entry.health = 'unhealthy';
    logger.warn('HEALTH_CHECK', `${tag} 判定: 不健康 ❌ (L2异常-非预期HTTP:${httpCode}, ${elapsed}ms) → IP:${ip} ${cfStats}`);
    if (entry.dedicated) {
      logger.warn('DEDICATED', `${tag} 固定IP实例L2异常(HTTP:${httpCode})，跳过自动重建（保持绑定IP不变），请人工检查`);
    } else if (config.auto_rebuild_on_unhealthy && entry.status === 'available') {
      logger.info('HEALTH_CHECK', `${tag} 触发自动重建 (auto_rebuild_on_unhealthy=true)`);
      await rotateInstanceIP(poolKey);
    }
    return;
  }

  // ---- 403率自动换IP ----
  if (config.auto_rotate_on_high_403
      && !entry.dedicated
      && entry.status === 'available'
      && entry.cf_check_count >= config.cf_min_check_count
      && parseInt(cf403Rate) >= config.cf_403_threshold) {
    logger.warn('HEALTH_CHECK', `${tag} 触发403自动换IP: 403率${cf403Rate}% >= 阈值${config.cf_403_threshold}% (检测${entry.cf_check_count}次, 最少${config.cf_min_check_count}次)`);
    const ok = await rotateInstanceIP(poolKey);
    if (ok) {
      entry.cf_check_count = 0;
      entry.cf_403_count = 0;
      logger.info('HEALTH_CHECK', `${tag} 403自动换IP成功, CF统计已重置`);
    } else {
      logger.error('HEALTH_CHECK', `${tag} 403自动换IP失败, CF统计保留 (下次巡检再触发)`);
    }
  } else if (config.auto_rotate_on_high_403 && parseInt(cf403Rate) > 0) {
    // 有403但未达阈值，记录原因
    const reason = entry.cf_check_count < config.cf_min_check_count
      ? `检测次数不足(${entry.cf_check_count}/${config.cf_min_check_count})`
      : `403率${cf403Rate}% < 阈值${config.cf_403_threshold}%`;
    logger.debug('HEALTH_CHECK', `${tag} 未触发自动换IP: ${reason}`);
  }

  const totalElapsed = Date.now() - t0;
  logger.info('HEALTH_CHECK', `${tag} ===== 健康检测完成 (${totalElapsed}ms) IP:${ip} health:${entry.health} ${cfStats} =====`);
}

// ==================== 实例级IP轮换（方案C） ====================
// options.force: 显式手动换IP（固定IP绑定实例仅允许强制场景）
async function rotateInstanceIP(poolKey, options = {}) {
  const entry = pool.get(poolKey);
  if (!entry) return false;

  if (entry.dedicated && !options.force) {
    logger.warn('DEDICATED', `[${entry.container}:${entry.instance_index}] 固定IP绑定实例拒绝自动换IP（手动强制请传 force）`);
    return false;
  }

  const instanceIdx = entry.instance_index;
  const tag = `[${entry.container}:${instanceIdx}]`;

  const oldIP = entry.ip;
  const t0 = Date.now();
  logger.info('ROTATE', `${tag} ===== 开始换IP (旧IP:${oldIP || 'null'}, 端口:${entry.host_port}, 最多${config.max_rebuild_attempts}次) =====`);

  for (let attempt = 0; attempt < config.max_rebuild_attempts; attempt++) {
    const attemptNum = attempt + 1;
    const attemptT0 = Date.now();
    try {
      logger.info('ROTATE', `${tag} 第${attemptNum}/${config.max_rebuild_attempts}次尝试: 重新注册WARP账号...`);

      // 停止旧进程（sing-box + 引擎）
      const metaPath = path.join(getInstanceDir(entry.instance_index), 'meta.json');
      let oldMeta = {};
      try {
        if (fs.existsSync(metaPath)) {
          oldMeta = JSON.parse(fs.readFileSync(metaPath, 'utf8'));
          stopInstanceProcesses(oldMeta);
        }
      } catch (e) { }

      // 重新供应出口引擎 + sing-box（换新账号 = 换新出口IP；wireguard 走不同 endpoint，masque 删账号重注册）
      let newPid = null;
      if (isMasque()) {
        // 删除旧账号配置 → 注册全新账号 → 重启引擎与 sing-box
        const engCfg = engineConfigPath(entry.instance_index);
        if (fs.existsSync(engCfg)) {
          try { fs.unlinkSync(engCfg); } catch (e) { }
        }
        oldMeta.engine_pid = startMasqueEngine(entry.instance_index);
        oldMeta.transport = 'masque';
        oldMeta.endpoint = null;
        const configPath = generateSingboxConfig(entry.instance_index, null, null, entry.host_port, null);
        newPid = startSingbox(entry.instance_index, configPath);
      } else {
        // 重新注册WARP账号
        const { privateKey, ipv6Addr } = registerWarpAccount(entry.instance_index);
        // 换IP时轮换到下一个endpoint（同一账号不同endpoint → 不同出口IP）
        const newEndpoint = nextEndpointForInstance(entry.instance_index);
        const configPath = generateSingboxConfig(entry.instance_index, privateKey, ipv6Addr, entry.host_port, newEndpoint);
        newPid = startSingbox(entry.instance_index, configPath);
        oldMeta.ipv6 = ipv6Addr;
        oldMeta.endpoint = `${newEndpoint.address}:${newEndpoint.port}`;
      }

      logger.debug('ROTATE', `${tag} 等待8秒进程启动...`);
      await sleep(8000);

      // 保存新元数据
      oldMeta.pid = newPid;
      fs.writeFileSync(metaPath, JSON.stringify(oldMeta, null, 2));

      logger.debug('ROTATE', `${tag} 检测新IP...`);
      const newExit = await checkExitIP(entry.host_port);
      const newIP = newExit.ip;
      const attemptElapsed = Date.now() - attemptT0;

      if (!newIP) {
        logger.warn('ROTATE', `${tag} 第${attemptNum}次失败: 新IP检测失败 (${attemptElapsed}ms)`);
        if (isMasque()) invalidateMasqueEndpointCache('换IP失败');
        continue;
      }

      if (newIP === oldIP && attempt < config.max_rebuild_attempts - 1) {
        logger.warn('ROTATE', `${tag} 第${attemptNum}次: IP未变化 (${oldIP} → ${newIP}, ${attemptElapsed}ms)`);
        continue;
      }
      if (newIP === oldIP) {
        logger.info('ROTATE', `${tag} 最后一次尝试: 接受未变化的IP ${newIP} (WARP同服务器IP有限)`);
      }

      // 跨实例IP去重 - WARP同服务器IP有限，最后一次尝试时接受重复
      let isDuplicate = false;
      let dupKey = null;
      for (const [key, val] of pool) {
        if (key !== poolKey && val.ip === newIP) {
          isDuplicate = true;
          dupKey = key;
          break;
        }
      }
      if (isDuplicate && attempt < config.max_rebuild_attempts - 1) {
        logger.warn('ROTATE', `${tag} 第${attemptNum}次: 新IP ${newIP} 与实例 ${dupKey} 重复，继续重试 (${attemptElapsed}ms)`);
        continue;
      }
      if (isDuplicate) {
        logger.info('ROTATE', `${tag} 最后一次尝试: 接受重复IP ${newIP} (WARP同服务器IP有限)`);
      }

      // 风控验收（warp-sh式"刷完验收"）：新IP风控值高则继续换，最后一次尝试时接受
      let newRisk = null;
      if (config.ip_risk_check && !isDuplicate) {
        newRisk = await checkIpRiskScore(newIP, entry.host_port);
        if (newRisk && newRisk.score >= config.ip_risk_threshold) {
          if (attempt < config.max_rebuild_attempts - 1) {
            logger.warn('ROTATE', `${tag} 第${attemptNum}次: 新IP ${newIP} 风控值${newRisk.score}(${getRiskLabel(newRisk.risk)}) ≥ ${config.ip_risk_threshold}，验收不通过，继续重试 (${Date.now() - attemptT0}ms)`);
            continue;
          }
          logger.warn('ROTATE', `${tag} 最后一次尝试: 接受风控值${newRisk.score}的IP ${newIP} (WARP同服务器IP有限)`);
        }
      }

      // 成功
      entry.ip = newIP;
      entry.ipv6 = newExit.v6;
      entry.ipv4 = newExit.v4;
      entry.ip_risk = newRisk ? newRisk.score : (config.ip_risk_check && !isDuplicate ? null : entry.ip_risk);
      entry.ip_risk_label = newRisk ? getRiskLabel(newRisk.risk) : (entry.ip_risk_label || null);
      entry.health = 'healthy';
      entry.cf_check_count = 0;
      entry.cf_403_count = 0;
      entry.cf_last_code = 0;
      const totalElapsed = Date.now() - t0;
      logger.info('ROTATE', `${tag} ===== 换IP成功 ✅ ${oldIP} → ${newIP} (第${attemptNum}次, 总耗时${totalElapsed}ms, CF统计已重置) =====`);
      return true;

    } catch (e) {
      const attemptElapsed = Date.now() - attemptT0;
      logger.error('ROTATE', `${tag} 第${attemptNum}次异常 (${attemptElapsed}ms): ${e.message}`);
    }

    if (attempt < config.max_rebuild_attempts - 1) {
      logger.debug('ROTATE', `${tag} 等待2秒后重试...`);
      await sleep(2000);
    }
  }

  const totalElapsed = Date.now() - t0;
  entry.health = 'unhealthy';
  logger.error('ROTATE', `${tag} ===== 换IP失败 ❌ ${config.max_rebuild_attempts}次均失败 (总耗时${totalElapsed}ms), 标记unhealthy =====`);
  return false;
}

// ==================== 实例管理 (sing-box + wgcf) ====================

function getInstancePort(instanceIndex) {
  return config.base_port + instanceIndex;
}

function getInstanceName(instanceIndex) {
  return `warp-${instanceIndex}`;
}

function getInstanceDir(instanceIndex) {
  return path.join(INSTANCES_DIR, `warp-${instanceIndex}`);
}

// ==================== MASQUE 引擎 (usque) ====================
// warp_transport='masque' 时，每个实例通过 usque 建立 MASQUE 隧道（QUIC/UDP，可 --http2 降级 TCP 443），
// 绕开 Cloudflare 对 WARP-WireGuard UDP 服务的风控封锁。sing-box 仍是收口入口（账密/白名单不变），
// 其 socks 出站指向本机引擎端口。
function isMasque() {
  return config.warp_transport === 'masque';
}
function enginePort(instanceIndex) {
  return ENGINE_PORT_BASE + instanceIndex;
}
function engineConfigPath(instanceIndex) {
  return path.join(getInstanceDir(instanceIndex), 'usque.json');
}
function engineLogPath(instanceIndex) {
  return path.join(getInstanceDir(instanceIndex), 'engine.log');
}

// 引擎日志 fd 被子进程长期持有，超限只能原地截断（rename 后子进程仍写旧 inode，等于没删）
function openEngineLog(p) {
  try { if (fs.statSync(p).size > 10 * 1024 * 1024) fs.truncateSync(p, 0); } catch (e) { }
  return fs.openSync(p, 'a');
}

// ==================== MASQUE 端点自愈 ====================
// 注册接口下发的默认端点可能被服务器网络封锁（实测 162.159.198.2 TCP/UDP 全丢包、同网段其它 IP 正常），
// 隧道建不起来会导致 IP 检测失败被误判 unhealthy。策略: 手动覆盖 > 进程内缓存 > 候选列表自动探测。
// 探测方式: 用 usque 本体做"金丝雀"——复制实例配置、替换端点后在临时端口起一个引擎并真实发一次请求，
// 能取回出口 IP 才算可用（与生产同一条路径，含 usque 的证书固定校验，比单纯 TLS 探测可靠）
const MASQUE_PROBE_CFG_PATH = '/tmp/masque_endpoint_probe.json';
const MASQUE_PROBE_PORT = 20299;
let masqueEndpointPick = null; // 进程内缓存: 探测选中的端点 { v4, h2 }

// 探测单个候选端点，返回是否可用
function probeMasqueEndpoint(ip, cfgPath) {
  let child = null;
  try {
    const cfg = JSON.parse(fs.readFileSync(cfgPath, 'utf8'));
    cfg.endpoint_v4 = ip;
    cfg.endpoint_h2_v4 = ip;
    fs.writeFileSync(MASQUE_PROBE_CFG_PATH, JSON.stringify(cfg, null, 2));
    const args = ['socks', '-c', MASQUE_PROBE_CFG_PATH, '-b', '127.0.0.1', '-p', String(MASQUE_PROBE_PORT), '--always-reconnect'];
    if (config.masque_http2) args.push('--http2');
    child = spawn(USQUE_BIN, args, { stdio: 'ignore' });
    const r = spawnSync('curl', ['-s', '--max-time', '8', '--socks5-hostname', `127.0.0.1:${MASQUE_PROBE_PORT}`, config.ip_check_url], { timeout: 11000, encoding: 'utf8' });
    const out = (r.stdout || '').trim();
    const ok = /^(\d{1,3}\.){3}\d{1,3}$/.test(out) || (out.includes(':') && /^[0-9a-fA-F:]+$/.test(out));
    if (!ok) logger.debug('ENGINE', `MASQUE 端点探测 ${ip} 不可用: ${out || `curl exit ${r.status}`}`);
    return ok;
  } catch (e) {
    logger.debug('ENGINE', `MASQUE 端点探测 ${ip} 异常: ${e.message}`);
    return false;
  } finally {
    if (child) { try { child.kill('SIGKILL'); } catch (e) { } }
    try { fs.unlinkSync(MASQUE_PROBE_CFG_PATH); } catch (e) { }
  }
}

// 换IP失败等疑似端点被封锁的场景下调用，下次引擎启动会重新探测
function invalidateMasqueEndpointCache(reason) {
  if (masqueEndpointPick) {
    logger.warn('ENGINE', `MASQUE 端点缓存失效(${reason})，下次引擎启动将重新探测`);
    masqueEndpointPick = null;
  }
}

// 解析本实例应使用的端点: 手动覆盖 > 缓存 > 候选探测; 返回 { v4, h2 } 或 null(沿用注册默认)
function resolveMasqueEndpoint(cfgPath) {
  const v4 = String(config.masque_endpoint_v4 || '').trim();
  const h2 = String(config.masque_endpoint_h2_v4 || '').trim();
  if (v4 || h2) return { v4: v4 || null, h2: h2 || null };
  if (masqueEndpointPick) return masqueEndpointPick;
  const list = Array.isArray(config.masque_endpoint_candidates)
    ? config.masque_endpoint_candidates.map(s => String(s).trim()).filter(Boolean)
    : [];
  for (const ip of list) {
    if (probeMasqueEndpoint(ip, cfgPath)) {
      masqueEndpointPick = { v4: ip, h2: ip };
      logger.info('ENGINE', `MASQUE 端点自动探测选中: ${ip}（候选 ${list.length} 个）`);
      return masqueEndpointPick;
    }
  }
  logger.warn('ENGINE', 'MASQUE 端点自动探测全部失败，沿用注册默认端点');
  return null;
}

// 按解析结果改写 usque.json（只写非空字段，值未变则不落盘）
function applyMasqueEndpoint(cfgPath) {
  const pick = resolveMasqueEndpoint(cfgPath);
  if (!pick || (!pick.v4 && !pick.h2)) return;
  try {
    const cfg = JSON.parse(fs.readFileSync(cfgPath, 'utf8'));
    const changed = (pick.v4 && cfg.endpoint_v4 !== pick.v4) || (pick.h2 && cfg.endpoint_h2_v4 !== pick.h2);
    if (!changed) return;
    if (pick.v4) cfg.endpoint_v4 = pick.v4;
    if (pick.h2) cfg.endpoint_h2_v4 = pick.h2;
    fs.writeFileSync(cfgPath, JSON.stringify(cfg, null, 2));
    logger.info('ENGINE', `MASQUE 端点已写入: v4=${cfg.endpoint_v4} h2=${cfg.endpoint_h2_v4} (${cfgPath})`);
  } catch (e) {
    logger.warn('ENGINE', `MASQUE 端点写入失败 (${cfgPath}): ${e.message}`);
  }
}

// 注册（如无账号）+ 启动本实例的 usque 引擎，返回 PID。
// 注意: 账号=出口IP（每账号独立 IPv6），引擎配置持久化在实例目录，删除文件即触发下次换新账号。
function startMasqueEngine(instanceIndex) {
  const dir = getInstanceDir(instanceIndex);
  if (!fs.existsSync(dir)) fs.mkdirSync(dir, { recursive: true });
  const cfgPath = engineConfigPath(instanceIndex);
  if (!fs.existsSync(cfgPath)) {
    const reg = spawnSync(USQUE_BIN, ['register', '-a', '-c', cfgPath], { cwd: dir, timeout: 60000, stdio: 'pipe' });
    if (reg.status !== 0) {
      throw new Error(`usque 账号注册失败: ${(reg.stderr || '').toString().trim() || `exit ${reg.status}`}`);
    }
    logger.info('ENGINE', `warp-${instanceIndex} 已注册全新 MASQUE 账号 (${cfgPath})`);
  }
  applyMasqueEndpoint(cfgPath);
  const logFd = openEngineLog(engineLogPath(instanceIndex));
  const args = ['socks', '-c', cfgPath, '-b', '127.0.0.1', '-p', String(enginePort(instanceIndex)), '--always-reconnect'];
  if (config.masque_http2) args.push('--http2'); // TCP 443 兜底（QUIC/UDP 也被风控时）
  const child = spawn(USQUE_BIN, args, { detached: true, stdio: ['ignore', logFd, logFd], env: { ...process.env, RUST_LOG: 'warn' } });
  fs.closeSync(logFd);
  child.unref();
  logger.info('ENGINE', `warp-${instanceIndex} usque已启动 (端口:${enginePort(instanceIndex)} PID:${child.pid}${config.masque_http2 ? ' http2' : ''})`);
  return child.pid;
}

function stopEngine(pid) {
  if (!pid) return;
  try { process.kill(pid, 'SIGTERM'); } catch (e) { }
}

// 停止实例的所有进程（sing-box + 引擎）
function stopInstanceProcesses(meta) {
  if (meta.pid) stopSingbox(meta.pid);
  if (meta.engine_pid) stopEngine(meta.engine_pid);
}

// 引擎是否在运行
function engineRunning(pid) {
  if (!pid) return false;
  try { process.kill(pid, 0); return true; } catch (e) { return false; }
}

// 重启实例的 usque 引擎（不重新注册账号 → IP 不变；修复引擎僵死/隧道挂起）
// 返回新 PID 或 null。sing-box 不动（账密/白名单接入无变化）。
function restartInstanceEngine(instanceIndex, reason = '') {
  const dir = getInstanceDir(instanceIndex);
  const metaPath = path.join(dir, 'meta.json');
  let meta = {};
  try {
    if (fs.existsSync(metaPath)) meta = JSON.parse(fs.readFileSync(metaPath, 'utf8'));
  } catch (e) { }
  // 停旧引擎（SIGTERM；不存在的 PID 忽略）
  if (meta.engine_pid) {
    try { process.kill(meta.engine_pid, 'SIGTERM'); } catch (e) { }
  }
  // usque 有时会卡住不退出，等 1.5s 后强杀
  setTimeout(() => {
    try { process.kill(meta.engine_pid, 'SIGKILL'); } catch (e) { }
  }, 1500).unref?.();
  // 起新引擎（配置文件还在 → 不重新注册 → IP 不变）
  let newPid = null;
  try {
    newPid = startMasqueEngine(instanceIndex);
    meta.engine_pid = newPid;
    fs.writeFileSync(metaPath, JSON.stringify(meta, null, 2));
    logger.warn('ENGINE', `warp-${instanceIndex} 引擎已重启 (新PID:${newPid}${reason ? `, 原因:${reason}` : ''})`);
  } catch (e) {
    logger.error('ENGINE', `warp-${instanceIndex} 引擎重启失败: ${e.message}`);
  }
  return newPid;
}

// ==================== 固定IP（专用实例绑定） ====================
// dedicated 标记持久化在实例目录 meta.json，服务重启后恢复
function isInstanceDedicated(instanceIndex) {
  try {
    const metaPath = path.join(getInstanceDir(instanceIndex), 'meta.json');
    if (fs.existsSync(metaPath)) {
      return !!JSON.parse(fs.readFileSync(metaPath, 'utf8')).dedicated;
    }
  } catch (e) { }
  return false;
}

function setDedicatedFlag(instanceIndex, dedicated, remark) {
  try {
    const metaPath = path.join(getInstanceDir(instanceIndex), 'meta.json');
    let meta = {};
    if (fs.existsSync(metaPath)) meta = JSON.parse(fs.readFileSync(metaPath, 'utf8'));
    if (dedicated) {
      meta.dedicated = true;
      meta.dedicated_at = Date.now();
      if (remark !== undefined) meta.dedicated_remark = remark;
    } else {
      delete meta.dedicated;
      delete meta.dedicated_at;
      delete meta.dedicated_remark;
    }
    fs.writeFileSync(metaPath, JSON.stringify(meta, null, 2));
  } catch (e) {
    logger.warn('DEDICATED', `meta.json 写入失败 warp-${instanceIndex}: ${e.message}`);
  }
}

// 生成sing-box配置文件
// endpoint: {address, port} 可选，默认使用轮询分配的对端（wireguard 传输）
// 账密模式: 主端口(base_port+idx)启用 username/password 认证
// 白名单模式: 白名单非空时额外监听白名单端口(base_port+offset+idx)，仅白名单源IP可连（免密）
function generateSingboxConfig(instanceIndex, privateKey, ipv6Addr, port, endpoint) {
  const configDir = getInstanceDir(instanceIndex);
  const configPath = path.join(configDir, 'singbox.json');

  const ep = endpoint || getEndpointForInstance(instanceIndex);
  const wlPort = port + config.whitelist_port_offset;
  const wlCidrs = whitelistToCidrs();
  const { username, password } = getProxyCreds();

  const singboxConfig = buildSingboxConfig({
    privateKey, ipv6Addr, port, wlPort, wlCidrs,
    authUsers: config.proxy_auth_enabled ? [{ username, password }] : null,
    endpoint: ep,
    transport: config.warp_transport,
    enginePort: enginePort(instanceIndex),
  });

  fs.writeFileSync(configPath, JSON.stringify(singboxConfig, null, 2));
  return configPath;
}

// 纯配置构造（创建实例与"热更新接入模式"共用，不落盘）
// transport: 'wireguard'（endpoints 起 WARP 隧道）| 'masque'（socks 出站指向本机 usque 引擎）
function buildSingboxConfig({ privateKey, ipv6Addr, port, wlPort, wlCidrs = [], authUsers = null, endpoint, transport, enginePort }) {
  const masque = transport === 'masque';
  const inbounds = [
    { type: 'mixed', tag: 'mixed-in', listen: '0.0.0.0', listen_port: port }
  ];
  if (authUsers && authUsers.length) {
    inbounds[0].users = authUsers;
  }
  if (wlPort && wlCidrs.length) {
    inbounds.push({ type: 'mixed', tag: 'wl-in', listen: '0.0.0.0', listen_port: wlPort });
  }

  const finalOutbound = masque ? 'socks-out' : 'WARP';
  const rules = [
    { inbound: ['mixed-in', ...(wlCidrs.length ? ['wl-in'] : [])], action: 'sniff' },
    { protocol: 'dns', action: 'hijack-dns' },
    { action: 'resolve', strategy: 'prefer_ipv6' },
  ];
  if (wlPort && wlCidrs.length) {
    // 白名单端口仅允许白名单源IP（其余拒绝），免密直连
    // 注意: sing-box 1.14 中 invert:true 反选整条规则（含 inbound），会把主端口也拒绝；
    // 也不支持 source_ip_cidr 的 ! 前缀取反。改为双规则：
    //   1) wl-in + 白名单源IP → 放行（走最终出站）
    //   2) wl-in + 其余源 → reject（无需 invert，靠顺序实现）
    rules.push({ inbound: ['wl-in'], source_ip_cidr: wlCidrs, outbound: finalOutbound });
    rules.push({ inbound: ['wl-in'], action: 'reject' });
  }
  rules.push({ ip_is_private: true, outbound: 'direct-out' });

  const cfg = {
    log: { level: 'warn', timestamp: true },
    dns: {
      servers: [
        { tag: 'remote', type: 'tls', server: 'dns.quad9.net', domain_resolver: 'local', detour: 'direct-out' },
        { tag: 'local', type: 'udp', server: '119.29.29.29', detour: 'direct-out' }
      ],
      final: 'remote',
      reverse_mapping: true,
      strategy: 'prefer_ipv6'
    },
    route: {
      default_domain_resolver: { server: 'local', rewrite_ttl: 60 },
      rules,
      auto_detect_interface: true,
      final: finalOutbound
    },
    inbounds,
  };

  if (masque) {
    cfg.outbounds = [
      { tag: 'direct-out', type: 'direct', udp_fragment: true },
      { tag: 'socks-out', type: 'socks', server: '127.0.0.1', server_port: enginePort }
    ];
  } else {
    cfg.endpoints = [
      {
        tag: 'WARP',
        type: 'wireguard',
        address: ['172.16.0.2/32', `${ipv6Addr}/128`],
        private_key: privateKey,
        peers: [
          {
            address: endpoint.address,
            port: endpoint.port,
            public_key: WARP_PUBLIC_KEY,
            allowed_ips: ['0.0.0.0/0', '::/0'],
            persistent_keepalive_interval: 30
          }
        ],
        mtu: 1408,
        udp_fragment: true
      }
    ];
    cfg.outbounds = [
      { tag: 'direct-out', type: 'direct', udp_fragment: true }
    ];
  }
  return cfg;
}

// 提取/绑定响应的接入字段：账密(主端口) + 白名单免密端口
function proxyAccessFields(entry) {
  const creds = getProxyCreds();
  const wlActive = whitelistToCidrs().length > 0;
  const authOn = !!config.proxy_auth_enabled;
  const authProxy = authOn
    ? `socks5://${creds.username}:${creds.password}@${publicIP}:${entry.host_port}`
    : `socks5://${publicIP}:${entry.host_port}`;
  return {
    username: authOn ? creds.username : null,
    password: authOn ? creds.password : null,
    proxy: `socks5://${publicIP}:${entry.host_port}`,
    proxy_auth: authProxy,
    proxy_whitelist: wlActive ? `socks5://${publicIP}:${entry.host_port + config.whitelist_port_offset}` : null,
  };
}

// 热更新接入模式（改账密开关/白名单后调用）：
// - wireguard: 复用现有账号与endpoint，仅重写配置并重启进程，IP不变
// - masque: 引擎账号不变（IP不变），仅重启 sing-box
async function reapplyInstanceAccess(instanceIndex) {
  const dir = getInstanceDir(instanceIndex);
  const configPath = path.join(dir, 'singbox.json');
  if (!fs.existsSync(configPath)) return false;

  const port = getInstancePort(instanceIndex);
  const { username, password } = getProxyCreds();
  const wlCidrs = whitelistToCidrs();

  const buildOpts = {
    port,
    wlPort: wlCidrs.length ? port + config.whitelist_port_offset : null,
    wlCidrs,
    authUsers: config.proxy_auth_enabled ? [{ username, password }] : null,
    transport: config.warp_transport,
    enginePort: enginePort(instanceIndex),
  };

  const metaPath = path.join(dir, 'meta.json');
  let meta = {};
  try {
    if (fs.existsSync(metaPath)) meta = JSON.parse(fs.readFileSync(metaPath, 'utf8'));
  } catch (e) { }

  if (isMasque()) {
    // 引擎保活（进程不在则拉起；已有 usque.json 不会重复注册账号 → IP 不变）
    if (!engineRunning(meta.engine_pid)) {
      meta.engine_pid = startMasqueEngine(instanceIndex);
      fs.writeFileSync(metaPath, JSON.stringify(meta, null, 2));
      logger.warn('REAPPLY', `warp-${instanceIndex} 引擎进程不在，已重新拉起 (PID:${meta.engine_pid})`);
    }
  } else {
    let old;
    try {
      old = JSON.parse(fs.readFileSync(configPath, 'utf8'));
    } catch (e) {
      logger.warn('REAPPLY', `warp-${instanceIndex} 旧配置解析失败: ${e.message}`);
      return false;
    }
    const ep0 = old.endpoints && old.endpoints[0];
    const peer = ep0 && ep0.peers && ep0.peers[0];
    if (!ep0 || !peer || !ep0.private_key) {
      logger.warn('REAPPLY', `warp-${instanceIndex} 旧配置缺少 WireGuard 信息，跳过`);
      return false;
    }
    const ipv6Addr = (ep0.address.find(a => a.includes(':')) || '').replace(/\/128$/, '');
    if (!ipv6Addr) {
      logger.warn('REAPPLY', `warp-${instanceIndex} 旧配置缺少 IPv6 地址，跳过`);
      return false;
    }
    buildOpts.privateKey = ep0.private_key;
    buildOpts.ipv6Addr = ipv6Addr;
    buildOpts.endpoint = { address: peer.address, port: peer.port };
  }

  const newConfig = buildSingboxConfig(buildOpts);

  // 重启单进程（账号不变 → IP不变）
  if (meta.pid) stopSingbox(meta.pid);
  fs.writeFileSync(configPath, JSON.stringify(newConfig, null, 2));
  const pid = startSingbox(instanceIndex, configPath);
  meta.pid = pid;
  meta.proxy_user = username;
  meta.proxy_pass = password;
  fs.writeFileSync(metaPath, JSON.stringify(meta, null, 2));
  logger.info('REAPPLY', `warp-${instanceIndex} 接入模式已更新并重启 (auth=${config.proxy_auth_enabled}, whitelist=${wlCidrs.length}条, PID:${pid}, transport:${config.warp_transport})`);
  return true;
}

// 批量热更新（异步job）：返回 jobId
function startReapplyAccessJob(needRestart = true) {
  rebuildWhitelistMatcher();
  const instances_list_promise = listWarpInstances();
  const job = createJob('reapply', 1);
  (async () => {
    // 等待启动恢复完成（恢复中重启进程会打断健康检查，导致实例被误判为失败而烧新账号）
    const waitStart = Date.now();
    while (recoveryInProgress && Date.now() - waitStart < 15 * 60 * 1000) {
      addLog(job, '等待启动恢复完成后再应用（避免打断健康检查）...', 'info');
      await new Promise(r => setTimeout(r, 10000));
    }
    const instances_list = await instances_list_promise;
    job.total = instances_list.length;
    addLog(job, `接入模式更新: 账密=${config.proxy_auth_enabled ? '开' : '关'}, 白名单=${(config.ip_whitelist || []).length} 条`, 'info');
    let ok = 0;
    for (const inst of instances_list) {
      try {
        const r = needRestart ? await reapplyInstanceAccess(inst.index) : true;
        if (r) ok++;
        addLog(job, `warp-${inst.index} ${r ? '已应用' : '跳过（无配置）'}`, r ? 'success' : 'info');
      } catch (e) {
        addLog(job, `warp-${inst.index} 应用失败: ${e.message}`, 'fail');
      }
      job.current++;
    }
    job.status = 'done';
    addLog(job, `接入模式更新完成: ${ok}/${instances_list.length}`, 'success');
  })().catch(err => {
    job.status = 'error';
    addLog(job, `任务异常: ${err.message}`, 'fail');
  });
  return job;
}

// 注册新的WARP账号并生成配置
// 方式A（主）: 直接调用Cloudflare注册API（同wgcf底层），支持Teams无限流量(team_token)
// 方式B（回退）: wgcf CLI
function registerWarpAccount(instanceIndex) {
  const dir = getInstanceDir(instanceIndex);
  if (!fs.existsSync(dir)) {
    fs.mkdirSync(dir, { recursive: true });
  }

  // 清理旧的wgcf文件（防止"existing account detected"错误）
  for (const f of ['wgcf-account.toml', 'wgcf-profile.conf']) {
    const fp = path.join(dir, f);
    if (fs.existsSync(fp)) {
      fs.unlinkSync(fp);
    }
  }

  try {
    return registerViaAPI(dir, instanceIndex);
  } catch (apiErr) {
    logger.warn('REGISTER', `API注册失败(${apiErr.message})，回退wgcf...`);
    try {
      return registerViaWgcf(dir);
    } catch (wgcfErr) {
      throw new Error(`注册失败: API(${apiErr.message}) / wgcf(${wgcfErr.message})`);
    }
  }
}

// 生成X25519密钥对，返回WireGuard格式(base64)
function generateWireGuardKeypair() {
  const cryptoMod = require('crypto');
  const { publicKey, privateKey } = cryptoMod.generateKeyPairSync('x25519');
  const pubDer = publicKey.export({ type: 'spki', format: 'der' });
  const privDer = privateKey.export({ type: 'pkcs8', format: 'der' });
  const pubB64 = pubDer.subarray(pubDer.length - 32).toString('base64');
  const privB64 = privDer.subarray(privDer.length - 32).toString('base64');
  return { pubB64, privB64 };
}

// 方式A: 直接调用Cloudflare注册API（含429限流重试）
function registerViaAPI(dir, instanceIndex) {
  const { pubB64, privB64 } = generateWireGuardKeypair();

  // 请求体 - Teams无限流量账户在config.team_token非空时启用
  const bodyObj = {
    key: pubB64,
    tos: '2023-07-17T10:24:48.735Z',
    type: 'Android',
    model: 'warp-manager',
    serial_number: `warp-${instanceIndex}`,
    device_name: `warp-manager-${instanceIndex}`,
    warp_enabled: true,
  };
  if (config.team_token) {
    bodyObj.team_token = config.team_token;
  }

  const bodyJson = JSON.stringify(bodyObj);
  const dataFile = path.join(dir, 'reg-body.json');
  fs.writeFileSync(dataFile, bodyJson);

  // 调用注册API，429限流时退避重试（最多5次，间隔递增）
  let out = '';
  for (let attempt = 0; attempt < 5; attempt++) {
    if (attempt > 0) {
      const backoff = 5000 * attempt; // 5s, 10s, 15s, 20s
      logger.warn('REGISTER', `注册请求可能限流，等待${backoff / 1000}秒后重试(${attempt}/5)...`);
      // 同步等待
      execSync(`sleep ${backoff / 1000}`, { timeout: 30000 });
    }
    try {
      out = execSync(
        `curl -s --compressed --max-time 25 -X POST "https://api.cloudflareclient.com/v0a2158/reg" ` +
        `-H "User-Agent: okhttp/3.12.1" -H "Content-Type: application/json" ` +
        `--data-binary @${dataFile}`,
        { timeout: 30000, stdio: 'pipe' }
      ).toString('utf8');
      // 检查是否429响应（Cloudflare API可能返回非JSON）
      if (!out.trim().startsWith('{')) {
        throw new Error(`非JSON响应(${out.substring(0, 60)})`);
      }
      break;
    } catch (e) {
      if (attempt >= 4) throw new Error(`注册API调用失败: ${e.message}`);
    }
  }

  const reg = JSON.parse(out);
  const addresses = reg.config && reg.config.interface && reg.config.interface.addresses;
  if (!addresses || !addresses.v6) {
    throw new Error('API响应缺少IPv6地址: ' + out.substring(0, 200));
  }

  const ipv4Addr = addresses.v4 || '172.16.0.2';
  const ipv6Addr = addresses.v6; // 格式: 2606:4700:110:xxxx:xxxx:xxxx:xxxx:xxxx

  // 保存注册信息供排查
  fs.writeFileSync(path.join(dir, 'registration.json'), JSON.stringify({
    id: reg.id,
    account_type: reg.account && reg.account.account_type,
    warp_plus: reg.account && reg.account.warp_plus,
    ipv4: ipv4Addr,
    ipv6: ipv6Addr,
    created_at: new Date().toISOString(),
  }, null, 2));

  logger.info('REGISTER', `API注册成功 ipv6=${ipv6Addr} type=${reg.account && reg.account.account_type} plus=${reg.account && reg.account.warp_plus}`);
  return { privateKey: privB64, ipv6Addr, ipv4Addr };
}

// 方式B: 回退到wgcf CLI
function registerViaWgcf(dir) {
  // 注册新账号
  execSync(`${WGCF_BIN} register --accept-tos`, { cwd: dir, timeout: 30000, stdio: 'pipe' });
  // 生成WireGuard配置
  execSync(`${WGCF_BIN} generate`, { cwd: dir, timeout: 30000, stdio: 'pipe' });

  // 读取生成的配置
  const profilePath = path.join(dir, 'wgcf-profile.conf');
  const profile = fs.readFileSync(profilePath, 'utf8');

  // 提取PrivateKey和IPv6地址
  const privateKey = (profile.match(/PrivateKey\s*=\s*(.+)/) || [])[1]?.trim();
  const addressLine = (profile.match(/Address\s*=\s*(.+)/) || [])[1]?.trim();
  const ipv6Addr = addressLine?.split(',')[1]?.trim()?.split('/')[0];

  if (!privateKey || !ipv6Addr) {
    throw new Error(`无法从wgcf-profile.conf提取密钥: private=${!!privateKey} ipv6=${!!ipv6Addr}`);
  }

  return { privateKey, ipv6Addr };
}

// 启动sing-box进程
function startSingbox(instanceIndex, configPath) {
  const name = getInstanceName(instanceIndex);
  const logPath = path.join(getInstanceDir(instanceIndex), 'singbox.log');
  const logFd = openEngineLog(logPath);
  const child = spawn(SINGBOX_BIN, ['run', '-c', configPath], {
    detached: true,
    stdio: ['ignore', logFd, logFd]
  });
  fs.closeSync(logFd);
  child.unref();
  return child.pid;
}

// 停止sing-box进程
function stopSingbox(pid) {
  if (!pid) return;
  try {
    process.kill(pid, 'SIGTERM');
  } catch (e) {
    // 进程可能已退出
  }
}

// 列出所有warp实例
async function listWarpInstances() {
  const instances = [];
  if (!fs.existsSync(INSTANCES_DIR)) return instances;

  const dirs = fs.readdirSync(INSTANCES_DIR).filter(d => d.startsWith('warp-'));
  for (const dir of dirs) {
    const m = dir.match(/warp-(\d+)/);
    if (!m) continue;
    const idx = parseInt(m[1]);
    const dirPath = path.join(INSTANCES_DIR, dir);
    const configPath = path.join(dirPath, 'singbox.json');
    const metaPath = path.join(dirPath, 'meta.json');

    let meta = {};
    try {
      if (fs.existsSync(metaPath)) {
        meta = JSON.parse(fs.readFileSync(metaPath, 'utf8'));
      }
    } catch (e) { }

    // 检查进程是否在运行
    let running = false;
    if (meta.pid) {
      try {
        process.kill(meta.pid, 0);
        running = true;
      } catch (e) {
        running = false;
      }
    }
    let engineRunningFlag = false;
    if (meta.engine_pid) {
      try {
        process.kill(meta.engine_pid, 0);
        engineRunningFlag = true;
      } catch (e) {
        engineRunningFlag = false;
      }
    }

    instances.push({
      name: dir,
      index: idx,
      port: getInstancePort(idx),
      running,
      pid: meta.pid || null,
      enginePid: meta.engine_pid || null,
      engineRunning: engineRunningFlag,
      configExists: fs.existsSync(configPath),
      ip: meta.ip || null,
      endpoint: meta.endpoint || null,
      ipv6: meta.ipv6 || null,
      dedicated: !!meta.dedicated,
      dedicated_remark: meta.dedicated_remark || '',
    });
  }
  return instances.sort((a, b) => a.index - b.index);
}

// 创建单个WARP实例
async function createWarpInstance(instanceIndex) {
  const name = getInstanceName(instanceIndex);
  const port = getInstancePort(instanceIndex);
  const dir = getInstanceDir(instanceIndex);

  logger.info('INSTANCE', `[${name}] 开始创建 (端口:${port}, transport:${config.warp_transport})`);

  // 停止旧进程
  const metaPath = path.join(dir, 'meta.json');
  let oldMeta = {};
  if (fs.existsSync(metaPath)) {
    try {
      oldMeta = JSON.parse(fs.readFileSync(metaPath, 'utf8'));
      stopInstanceProcesses(oldMeta);
    } catch (e) { }
  }
  // 保留固定IP绑定标记（重建后仍维持绑定状态，但注意：重建会重新注册账号导致IP变化）
  const wasDedicated = !!oldMeta.dedicated;

  // 创建目录
  if (!fs.existsSync(dir)) {
    fs.mkdirSync(dir, { recursive: true });
  }

  const meta = {
    dedicated: wasDedicated || undefined,
    dedicated_at: wasDedicated ? (oldMeta.dedicated_at || Date.now()) : undefined,
    createdAt: Date.now(),
  };
  let privateKey = null, ipv6Addr = null, endpoint = null;

  if (isMasque()) {
    // MASQUE 传输: usque 引擎注册+启动（账号=出口IP）
    meta.engine_pid = startMasqueEngine(instanceIndex);
    meta.transport = 'masque';
    logger.info('INSTANCE', `[${name}] MASQUE 引擎已启动 (PID:${meta.engine_pid})`);
  } else {
    // wireguard 传输: 注册WARP账号 + 轮询分配 endpoint（出口IP更分散）
    logger.debug('INSTANCE', `[${name}] 注册WARP账号...`);
    const reg = registerWarpAccount(instanceIndex);
    privateKey = reg.privateKey;
    ipv6Addr = reg.ipv6Addr;
    logger.info('INSTANCE', `[${name}] WARP账号注册成功, IPv6: ${ipv6Addr}`);

    endpoint = getEndpointForInstance(instanceIndex);
    meta.ipv6 = ipv6Addr;
    meta.endpoint = `${endpoint.address}:${endpoint.port}`;
    logger.debug('INSTANCE', `[${name}] sing-box配置已生成 endpoint=${endpoint.address}:${endpoint.port}`);
  }

  // 生成sing-box配置
  const configPath = generateSingboxConfig(instanceIndex, privateKey, ipv6Addr, port, endpoint);

  // 启动sing-box
  const pid = startSingbox(instanceIndex, configPath);
  meta.pid = pid;
  logger.info('INSTANCE', `[${name}] sing-box已启动 (PID:${pid})`);

  // 等待连接建立
  await sleep(8000);

  // 检测出口IP
  const poolKey = `${instanceIndex}`;
  const exit = await checkExitIP(port);
  const ip = exit.ip;

  // L2检测
  const httpCode = await checkOpenAIAccess(port);
  const isHealthy = (httpCode === 200 || httpCode === 302 || httpCode === 403);

  // 保存元数据
  meta.port = port;
  meta.ip = ip;
  fs.writeFileSync(metaPath, JSON.stringify(meta, null, 2));

  pool.set(poolKey, {
    container: name,
    instance_index: instanceIndex,
    host_port: port,
    ip,
    ipv6: exit.v6,
    ipv4: exit.v4,
    health: ip ? (isHealthy ? 'healthy' : 'unhealthy') : 'unknown',
    status: 'available',
    dedicated: wasDedicated || undefined,
    dedicated_at: wasDedicated ? (oldMeta.dedicated_at || Date.now()) : undefined,
    cf_check_count: 1,
    cf_403_count: httpCode === 403 ? 1 : 0,
    cf_last_code: httpCode,
  });

  logger.info('INSTANCE', `[${name}] 加入代理池: ${ip}:${port} health=${ip ? (isHealthy ? 'healthy' : 'unhealthy') : 'unknown'} CF:${httpCode}`);
}

// 删除WARP实例
async function deleteWarpInstance(name) {
  const m = name.match(/warp-(\d+)/);
  if (!m) return;
  const idx = parseInt(m[1]);
  const dir = getInstanceDir(idx);

  // 停止进程
  const metaPath = path.join(dir, 'meta.json');
  if (fs.existsSync(metaPath)) {
    try {
      const meta = JSON.parse(fs.readFileSync(metaPath, 'utf8'));
      stopInstanceProcesses(meta);
    } catch (e) { }
  }

  // 删除目录
  try {
    if (fs.existsSync(dir)) {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  } catch (e) { }

  // 从代理池中移除
  const poolKey = `${idx}`;
  pool.delete(poolKey);
  for (const [sid, sess] of sessions) {
    if (sess.pool_key === poolKey) {
      sessions.delete(sid);
    }
  }
  logger.info('INSTANCE', `[${name}] 已删除，代理池已清理`);
}

// ==================== API: 环境检查 ====================
app.get('/api/check', async (req, res) => {
  try {
    let singboxExists = fs.existsSync(SINGBOX_BIN);
    let wgcfExists = fs.existsSync(WGCF_BIN);
    let singboxVersion = '';
    let wgcfVersion = '';
    try {
      singboxVersion = await runCmdFast(`${SINGBOX_BIN} version 2>&1 | head -1`, 5000);
    } catch (e) { }
    // wgcf 2.2.32 无 version 子命令（--version / version 均报错），仅确认二进制存在
    wgcfVersion = '';
    success(res, {
      singbox: singboxVersion || (singboxExists ? 'found' : 'not found'),
      wgcf: wgcfVersion || (wgcfExists ? 'found' : 'not found'),
      singboxExists,
      wgcfExists,
      instancesDir: INSTANCES_DIR,
      publicIP,
      poolSize: pool.size,
      availableCount: Array.from(pool.values()).filter(e => e.status === 'available' && e.health === 'healthy').length,
    });
  } catch (err) {
    error(res, ERROR_CODES.INSTANCE_ERROR, '环境检查失败: ' + err.message, 500);
  }
});

// ==================== API: 容器/实例列表 ====================
app.get('/api/containers', async (req, res) => {
  try {
    const instances_list = await listWarpInstances();
    const instances = [];

    for (const inst of instances_list) {
      const poolKey = `${inst.index}`;
      const poolEntry = pool.get(poolKey);

      if (poolEntry) {
        const cfCheckCount = poolEntry.cf_check_count || 0;
        const cf403Count = poolEntry.cf_403_count || 0;
        const cf403Rate = cfCheckCount > 0 ? Math.round(cf403Count / cfCheckCount * 100) : null;
        const access = proxyAccessFields(poolEntry);
        instances.push({
          container: inst.name,
          instance_index: poolEntry.instance_index,
          pool_key: poolKey,
          host_port: poolEntry.host_port,
          ip: poolEntry.ip,
          ipv6: poolEntry.ipv6 || null,
          ipv4: poolEntry.ipv4 || null,
          ip_mode: (poolEntry.ipv4 && !poolEntry.ipv6) ? 'ip4' : 'ip6',
          endpoint: inst.endpoint,
          health: poolEntry.health,
          pool_status: poolEntry.status,
          cf_403_rate: cf403Rate,
          cf_403_count: cf403Count,
          cf_check_count: cfCheckCount,
          cf_last_code: poolEntry.cf_last_code || 0,
          ip_risk: poolEntry.ip_risk || null,
          ip_risk_label: poolEntry.ip_risk_label || null,
          dedicated: !!poolEntry.dedicated,
          username: access.username,
          password: access.password,
          proxy_auth: access.proxy_auth,
          proxy_whitelist: access.proxy_whitelist,
          whitelist_port: access.proxy_whitelist ? poolEntry.host_port + config.whitelist_port_offset : null,
        });
      } else {
        instances.push({
          container: inst.name,
          instance_index: inst.index,
          host_port: inst.port,
          ip: inst.ip,
          ipv6: null,
          ipv4: null,
          ip_mode: 'ip6',
          endpoint: inst.endpoint,
          health: inst.running ? 'unknown' : 'stopped',
          pool_status: inst.running ? 'not-in-pool' : 'stopped',
          dedicated: !!inst.dedicated,
        });
      }
    }

    success(res, instances);
  } catch (err) {
    error(res, ERROR_CODES.INSTANCE_ERROR, err.message, 500);
  }
});

// ==================== API: 统计信息 ====================
app.get('/api/stats', async (req, res) => {
  try {
    const instances_list = await listWarpInstances();
    const running = instances_list.filter(i => i.running);
    const totalInstances = pool.size;
    const available = Array.from(pool.values()).filter(e => !e.dedicated && e.status === 'available' && e.health === 'healthy').length;
    const inUse = Array.from(pool.values()).filter(e => e.status === 'in_use').length;
    const unhealthy = Array.from(pool.values()).filter(e => e.health === 'unhealthy').length;
    const dedicatedCount = Array.from(pool.values()).filter(e => e.dedicated).length;
    const uniqueIPs = new Set(Array.from(pool.values()).map(e => e.ip).filter(Boolean));

    success(res, {
      total_instances_dirs: instances_list.length,
      running_processes: running.length,
      stopped_processes: instances_list.filter(i => !i.running).length,
      total_instances: totalInstances,
      available: available,
      in_use: inUse,
      unhealthy: unhealthy,
      dedicated: dedicatedCount,
      unique_ips: uniqueIPs.size,
      active_sessions: sessions.size,
      resident_count: config.resident_count,
    });
  } catch (err) {
    error(res, ERROR_CODES.INSTANCE_ERROR, err.message, 500);
  }
});

// ==================== API: 创建/重建容器 ====================
app.post('/api/create', async (req, res) => {
  const { instance_count } = req.body;
  
  if (instance_count !== undefined) {
    const count = Math.min(Math.max(parseInt(instance_count), 1), config.max_instances);
    if (count !== config.resident_count) {
      config.resident_count = count;
      saveConfig();
      logger.info('CONFIG', `实例数已更新为 ${count}`);
    }
  }

  const instanceCount = config.resident_count;
  const job = createJob('create', instanceCount);
  success(res, { jobId: job.id });

  (async () => {
    try {
      let healthyCount = 0;
      for (let i = 0; i < instanceCount; i++) {
        // 固定IP绑定实例跳过重建（重建会重新注册账号导致绑定IP变化）
        if (isInstanceDedicated(i)) {
          addLog(job, `warp-${i} 固定IP绑定中，跳过重建`, 'info');
          job.results.push({ success: true, name: `warp-${i}`, skipped: 'dedicated' });
          job.current = i + 1;
          continue;
        }
        addLog(job, `创建实例 warp-${i}...`, 'info');
        try {
          await createWarpInstance(i);
          const poolKey = `${i}`;
          const entry = pool.get(poolKey);
          if (entry && entry.health === 'healthy') healthyCount++;
          addLog(job, `实例 warp-${i} 创建完成`, 'success');
          job.results.push({ success: true, name: `warp-${i}` });
        } catch (err) {
          addLog(job, `创建 warp-${i} 失败: ${err.message}`, 'fail');
          job.results.push({ success: false, name: `warp-${i}`, error: err.message });
        }
        job.current = i + 1;
      }
      addLog(job, `创建完成: ${healthyCount}/${instanceCount} 实例健康`, 'success');
    } catch (err) {
      addLog(job, `创建失败: ${err.message}`, 'fail');
      job.results.push({ success: false, error: err.message });
    }
    job.current = instanceCount;
    job.status = 'done';
    addLog(job, `创建完成!`, 'info');
  })().catch(err => {
    job.status = 'error';
    addLog(job, `任务异常: ${err.message}`, 'fail');
  });
});

// ==================== API: 批量删除 ====================
app.post('/api/delete', async (req, res) => {
  const { names = [], deleteAll = false, force } = req.body;
  let toDelete = names;

  if (deleteAll) {
    try {
      const instances_list = await listWarpInstances();
      // 固定IP绑定实例不被"删除全部"清理
      toDelete = instances_list.filter(i => !i.dedicated).map(i => i.name);
    } catch (e) {
      return error(res, ERROR_CODES.INSTANCE_ERROR, e.message, 500);
    }
  }

  // 固定IP绑定实例默认拒绝删除，force=true 强制
  if (!force) {
    for (const n of toDelete) {
      const m = n.match(/warp-(\d+)/);
      if (m && isInstanceDedicated(parseInt(m[1]))) {
        return error(res, ERROR_CODES.DEDICATED_BOUND, `实例 ${n} 已绑定固定IP；如确认删除请传 force=true`);
      }
    }
  }

  const job = createJob('delete', toDelete.length);
  success(res, { jobId: job.id });

  (async () => {
    for (let i = 0; i < toDelete.length; i++) {
      const name = toDelete[i];
      try {
        if (!validateContainerName(name)) {
          addLog(job, `[${name}] 无效的实例名称`, 'fail');
          job.results.push({ success: false, name, error: '无效的实例名称' });
          continue;
        }
        await deleteWarpInstance(name);
        addLog(job, `[${name}] 已删除`, 'success');
        job.results.push({ success: true, name });
      } catch (err) {
        addLog(job, `[${name}] 删除失败: ${err.message}`, 'fail');
        job.results.push({ success: false, name, error: err.message });
      }
      job.current = i + 1;
    }
    job.status = 'done';
    addLog(job, `删除完成!`, 'info');
  })().catch(err => {
    job.status = 'error';
    addLog(job, `任务异常: ${err.message}`, 'fail');
  });
});

// ==================== API: 重建容器 ====================
app.post('/api/rebuild', async (req, res) => {
  const { name, names = [], force } = req.body;
  const targetNames = name ? [name] : names;

  if (targetNames.length === 0) {
    return error(res, ERROR_CODES.INVALID_INPUT, '缺少容器名称');
  }

  for (const n of targetNames) {
    if (!validateContainerName(n)) {
      return error(res, ERROR_CODES.INVALID_INPUT, `无效的实例名称: ${n}`);
    }
  }

  // 固定IP绑定实例默认拒绝重建（重建会重新注册账号导致绑定IP变化），force=true 强制
  if (!force) {
    for (const n of targetNames) {
      const idx = parseInt(n.match(/warp-(\d+)/)[1]);
      if (isInstanceDedicated(idx)) {
        return error(res, ERROR_CODES.DEDICATED_BOUND, `实例 ${n} 已绑定固定IP，重建会导致IP变化；如确认请传 force=true`);
      }
    }
  }

  const job = createJob('rebuild', targetNames.length);
  success(res, { jobId: job.id });

  (async () => {
    for (let i = 0; i < targetNames.length; i++) {
      const cname = targetNames[i];
      try {
        const m = cname.match(/warp-(\d+)/);
        if (!m) {
          addLog(job, `[${cname}] 无法解析实例索引`, 'fail');
          job.results.push({ success: false, name: cname, error: '无法解析实例索引' });
          continue;
        }
        const instanceIndex = parseInt(m[1]);
        addLog(job, `[${cname}] 重建中...`, 'info');
        await createWarpInstance(instanceIndex);
        addLog(job, `[${cname}] 重建完成`, 'success');
        job.results.push({ success: true, name: cname });
      } catch (err) {
        addLog(job, `[${cname}] 重建失败: ${err.message}`, 'fail');
        job.results.push({ success: false, name: cname, error: err.message });
      }
      job.current = i + 1;
    }
    job.status = 'done';
    addLog(job, `重建完成!`, 'info');
  })().catch(err => {
    job.status = 'error';
    addLog(job, `任务异常: ${err.message}`, 'fail');
  });
});

// ==================== API: 启动/停止 ====================
app.post('/api/start', async (req, res) => {
  const { name } = req.body;
  if (!name || !validateContainerName(name)) {
    return error(res, ERROR_CODES.INVALID_INPUT, '无效的实例名称');
  }
  try {
    const m = name.match(/warp-(\d+)/);
    if (!m) return error(res, ERROR_CODES.INVALID_INPUT, '无法解析实例索引');
    const idx = parseInt(m[1]);
    const dir = getInstanceDir(idx);
    const metaPath = path.join(dir, 'meta.json');
    if (!fs.existsSync(metaPath)) {
      return error(res, ERROR_CODES.NOT_FOUND, '实例配置不存在');
    }
    const meta = JSON.parse(fs.readFileSync(metaPath, 'utf8'));
    const configPath = path.join(dir, 'singbox.json');
    if (!fs.existsSync(configPath)) {
      return error(res, ERROR_CODES.NOT_FOUND, 'sing-box配置不存在');
    }
    const pid = startSingbox(idx, configPath);
    meta.pid = pid;
    fs.writeFileSync(metaPath, JSON.stringify(meta, null, 2));
    // 重新入池为可用，交给健康巡检重新检测出口IP
    const startEntry = pool.get(`${idx}`);
    if (startEntry) {
      startEntry.status = 'available';
      startEntry.health = 'unknown';
    }
    logger.info('INSTANCE', `[${name}] sing-box已启动 (PID:${pid})`);
    success(res, { name, pid });
  } catch (err) {
    error(res, ERROR_CODES.INSTANCE_ERROR, err.message, 500);
  }
});

app.post('/api/stop', async (req, res) => {
  const { name } = req.body;
  if (!name || !validateContainerName(name)) {
    return error(res, ERROR_CODES.INVALID_INPUT, '无效的实例名称');
  }
  try {
    const m = name.match(/warp-(\d+)/);
    if (!m) return error(res, ERROR_CODES.INVALID_INPUT, '无法解析实例索引');
    const idx = parseInt(m[1]);
    const dir = getInstanceDir(idx);
    const metaPath = path.join(dir, 'meta.json');
    if (fs.existsSync(metaPath)) {
      const meta = JSON.parse(fs.readFileSync(metaPath, 'utf8'));
      if (meta.pid) stopSingbox(meta.pid);
      meta.pid = null;
      fs.writeFileSync(metaPath, JSON.stringify(meta, null, 2));
    }
    // 标记为已停止：不参与健康巡检与自动重建（此前误标为 available，会导致停止后被自动拉起）
    const poolKey = `${idx}`;
    const entry = pool.get(poolKey);
    if (entry) {
      entry.status = 'stopped';
      entry.health = 'unknown';
    }
    logger.info('INSTANCE', `[${name}] 已停止`);
    success(res, { name });
  } catch (err) {
    error(res, ERROR_CODES.INSTANCE_ERROR, err.message, 500);
  }
});

// ==================== API: 任务状态 ====================
app.get('/api/job/:id', (req, res) => {
  const id = parseInt(req.params.id);
  const job = jobs[id];
  if (!job) {
    return error(res, ERROR_CODES.INVALID_INPUT, '任务不存在');
  }
  success(res, job);
});

// ==================== API: 白名单管理 ====================
// 白名单内的源IP: ① 免密直连代理白名单端口（socks5://ip:wl_port）② 调用全部API免Key
// GET  查询；POST {add:'1.2.3.4'|{ip,remark}} 增 / {remove:'1.2.3.4'} 删 / {update:{ip,remark}} 改备注 / {ips:[...]} 整表替换
app.get('/api/whitelist', (req, res) => {
  success(res, {
    items: Array.isArray(config.ip_whitelist) ? config.ip_whitelist : [],
    ips: wlEntries(),
    port_offset: config.whitelist_port_offset,
    client_ip: getClientIP(req),
  });
});

app.post('/api/whitelist', async (req, res) => {
  const { ips, add, remove, update } = req.body || {};
  let list = Array.isArray(config.ip_whitelist) ? [...config.ip_whitelist] : [];
  const ipRe = /^(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}(\/\d{1,2})?|[0-9a-fA-F:]+(\/\d{1,3})?)$/;
  let structuralChange = false;

  if (update && update.ip) {
    // 仅修改备注/地区（不触发配置热更新）
    const entry = list.find(e => e.ip === update.ip);
    if (!entry) return error(res, ERROR_CODES.NOT_FOUND, `白名单中不存在 ${update.ip}`);
    if (update.remark !== undefined) entry.remark = String(update.remark).slice(0, 100);
    config.ip_whitelist = list;
    saveConfig();
    return success(res, { items: list, ips: wlEntries() });
  }

  if (Array.isArray(ips)) {
    // 整表替换（元素可为字符串或 {ip,remark}）
    const bad = ips.filter(x => !ipRe.test(typeof x === 'string' ? x : (x && x.ip) || ''));
    if (bad.length) {
      return error(res, ERROR_CODES.INVALID_INPUT, `无效的IP/CIDR: ${JSON.stringify(bad).slice(0, 120)}`);
    }
    const seen = new Set();
    list = ips.map(x => {
      const ip = (typeof x === 'string' ? x : x.ip).trim();
      if (seen.has(ip)) return null;
      seen.add(ip);
      const old = list.find(e => e.ip === ip);
      return {
        ip,
        remark: typeof x === 'object' && x.remark ? String(x.remark).slice(0, 100) : (old ? old.remark : ''),
        region: old ? old.region : '',
        added_at: old ? old.added_at : new Date().toISOString(),
      };
    }).filter(Boolean);
    structuralChange = true;
  } else if (add) {
    const ip = typeof add === 'string' ? add.trim() : (add && add.ip || '').trim();
    const remark = typeof add === 'object' && add.remark ? String(add.remark).slice(0, 100) : '';
    if (!ipRe.test(ip)) return error(res, ERROR_CODES.INVALID_INPUT, `无效的IP/CIDR: ${ip}`);
    if (!list.find(e => e.ip === ip)) {
      const region = await lookupIpRegion(ip);
      list.push({ ip, remark, region, added_at: new Date().toISOString() });
      structuralChange = true;
    } else if (remark) {
      list.find(e => e.ip === ip).remark = remark;
    }
  } else if (remove) {
    const n = list.length;
    list = list.filter(e => e.ip !== remove);
    if (list.length !== n) structuralChange = true;
  } else {
    return error(res, ERROR_CODES.INVALID_INPUT, '缺少 ips / add / remove / update 参数');
  }

  config.ip_whitelist = list;
  saveConfig();

  if (structuralChange) {
    // 白名单变化影响 sing-box 路由规则 → 热更新全部实例配置（账号不变、IP不变）
    const job = startReapplyAccessJob(true);
    return success(res, { items: list, ips: wlEntries(), reapply_job_id: job.id });
  }
  success(res, { items: list, ips: wlEntries() });
});

// 修改全局代理账密（账密模式页铅笔编辑）；不传password则随机生成；可同时改username
app.post('/api/creds/set', async (req, res) => {
  const { username, password } = req.body || {};
  const newPass = (password && String(password).trim()) || crypto.randomBytes(9).toString('base64url');
  const newUser = (username && String(username).trim()) || config.proxy_username || 'warpuser';
  if (/[^\w-]/.test(newPass)) return error(res, ERROR_CODES.INVALID_INPUT, '密码仅允许字母数字、下划线、连字符');
  if (!/^[\w-]{3,32}$/.test(newUser)) return error(res, ERROR_CODES.INVALID_INPUT, '用户名需3-32位字母数字/下划线/连字符');

  config.proxy_username = newUser;
  config.proxy_password = newPass;
  saveConfig();

  // 热更新全部实例配置（账号不变、IP不变）
  const job = startReapplyAccessJob(true);
  logger.info('AUTH', `全局代理账密已更新 (user=${newUser})，热更新任务 #${job.id}`);
  success(res, { username: newUser, password: newPass, reapply_job_id: job.id });
});

// ==================== API: 配置管理 ====================
app.get('/api/config', (req, res) => {
  // team_token / api_key 不返回完整值（登录 Key 不外泄）
  const safeConfig = { ...config };
  safeConfig.team_token = config.team_token ? config.team_token.substring(0, 6) + '****' : '';
  safeConfig.api_key = config.api_key ? '****' : '';
  safeConfig.public_ip = publicIP;
  success(res, safeConfig);
});

app.post('/api/config', async (req, res) => {
  try {
    const updates = req.body;
    const allowedKeys = [
      'resident_count', 'sticky_minutes', 'auto_rebuild_on_unhealthy',
      'health_check_interval', 'bind_address', 'mode',
      'max_instances', 'base_port', 'min_available', 'max_concurrent_create',
      'ip_check_url', 'ip_check_url_v6', 'openai_check_url',
      'ip_check_retries', 'ip_check_interval',
      'max_rebuild_attempts',
      'cleanup_on_exit', 'rate_limit_enabled', 'rate_limit_per_minute',
      'auto_rotate_on_high_403', 'cf_403_threshold', 'cf_min_check_count',
      'ip_risk_check', 'ip_risk_threshold', 'ip_risk_ttl', 'ip_risk_check_url',
      'log_level', 'public_ip', 'session_cleanup_interval',
      'team_token', 'proxy_auth_enabled',
    ];

    const oldResidentCount = config.resident_count;
    const oldProxyAuth = config.proxy_auth_enabled;
    const oldWhitelist = JSON.stringify(config.ip_whitelist || []);

    for (const key of allowedKeys) {
      if (updates.hasOwnProperty(key)) {
        config[key] = updates[key];
      }
    }

    saveConfig();
    logger.setLogLevel(config.log_level);

    // 账密开关变化 → 热更新全部实例的 sing-box 配置（白名单走 /api/whitelist，同样触发）
    let reapplyJobId = null;
    if (config.proxy_auth_enabled !== oldProxyAuth || JSON.stringify(config.ip_whitelist || []) !== oldWhitelist) {
      const job = startReapplyAccessJob(true);
      reapplyJobId = job.id;
    }

    // 重启定时器
    if (healthCheckTimer) clearInterval(healthCheckTimer);
    if (sessionCleanupTimer) clearInterval(sessionCleanupTimer);
    startTimers();

    // 如果 resident_count 变化，调整容器数量
    if (updates.resident_count !== undefined && updates.resident_count !== oldResidentCount) {
      try {
        await adjustResidentCount();
      } catch (e) {
        logger.error('CONFIG', '调整常驻容器数量失败: ' + e.message);
      }
    }

    const safeConfig = { ...config };
    safeConfig.team_token = config.team_token ? config.team_token.substring(0, 6) + '****' : '';
    safeConfig.public_ip = publicIP;
    if (reapplyJobId !== null) safeConfig.reapply_job_id = reapplyJobId;
    success(res, safeConfig);
  } catch (e) {
    logger.error('CONFIG', '配置更新失败: ' + e.message);
    error(res, ERROR_CODES.INTERNAL_ERROR, e.message, 500);
  }
});

// ==================== 代理池 - 提取核心（acquire 与 /api/proxies 共用，调用方需持有池锁） ====================
// 返回 { entry, reused, expiresAt }；无可用代理返回 null；粘性过期/复用逻辑内置
function pickProxy(sessionId, stickyMinutes) {
  const useStickySession = !!sessionId;
  const now = Date.now();

  // 粘性：命中未过期会话且实例健康 → 复用并续期
  if (useStickySession && sessions.has(sessionId)) {
    const session = sessions.get(sessionId);
    if (now < session.expires_at) {
      const entry = pool.get(session.pool_key);
      if (entry && entry.health === 'healthy') {
        session.expires_at = now + stickyMinutes * 60 * 1000;
        logger.info('POOL', `acquire 复用会话 ${sessionId.substring(0, 8)}... IP:${entry.ip} 端口:${entry.host_port}`);
        return { entry, reused: true, expiresAt: session.expires_at };
      }
    } else {
      // session过期，归还IP
      const entry = pool.get(session.pool_key);
      if (entry) entry.status = 'available';
      sessions.delete(sessionId);
      rotatePendingIfAvailable(session.pool_key);
      logger.info('POOL', `acquire 惰性清理过期会话 ${sessionId.substring(0, 8)}...`);
    }
  }

  // 查找可用且健康的实例（round-robin；固定IP绑定实例不参与分配）
  const availableKeys = [];
  for (const [key, entry] of pool) {
    // health='unknown' 表示尚未完成出口检测，此前被永久排除；
    // 实测 100 个实例中有 29 个卡在 unknown，等于池容量缩水 30%。
    // 现在只有明确 unhealthy 才排除。
    if (!entry.dedicated && entry.status === 'available' && entry.health !== 'unhealthy' && entry.ip) {
      availableKeys.push(key);
    }
  }
  if (availableKeys.length === 0) {
    logger.warn('POOL', 'acquire 无可用代理');
    return null;
  }

  const poolKey = availableKeys[roundRobinIdx % availableKeys.length];
  roundRobinIdx++;

  const entry = pool.get(poolKey);
  let expiresAt = null;
  if (useStickySession) {
    entry.status = 'in_use';
    expiresAt = now + stickyMinutes * 60 * 1000;
    sessions.set(sessionId, {
      session_id: sessionId,
      pool_key: poolKey,
      acquired_at: now,
      expires_at: expiresAt,
    });
    logger.info('POOL', `acquire 新建粘性会话 ${sessionId.substring(0, 8)}... IP:${entry.ip} 端口:${entry.host_port}`);
  } else {
    logger.info('POOL', `acquire 无会话提取 IP:${entry.ip} 端口:${entry.host_port}`);
  }
  return { entry, reused: false, expiresAt };
}

// ==================== API: 代理池 - 获取代理 ====================
app.get('/api/pool/acquire', async (req, res) => {
  const release = await acquirePoolLock();
  try {
    const stickySessionId = typeof req.query.session_id === 'string' ? req.query.session_id.trim() : '';
    const sessionId = stickySessionId || null;
    // 粘性时长: minutes / sticky_minutes 参数，1~1440 分钟
    const minutesRaw = parseInt(req.query.minutes) || parseInt(req.query.sticky_minutes) || config.sticky_minutes;
    const stickyMinutes = Math.min(Math.max(minutesRaw, 1), 1440);

    const picked = pickProxy(sessionId, stickyMinutes);
    if (!picked) {
      return error(res, ERROR_CODES.NO_AVAILABLE_PROXY, '无可用代理，请稍后重试');
    }

    const { entry, reused, expiresAt } = picked;
    const useStickySession = !!sessionId;
    bumpUsage(useStickySession ? 'sticky' : 'acquires');

    success(res, {
      session_id: sessionId,
      ip: entry.ip,
      expires_at: expiresAt ? new Date(expiresAt).toISOString() : null,
      sticky_minutes: useStickySession ? stickyMinutes : null,
      sticky: useStickySession,
      reused,
      ...proxyAccessFields(entry),
    });
    probeCfOnAcquire(entry ? `${entry.instance_index}` : '', sessionId || 'stateless').catch(e => {
      logger.error('CF_USAGE', `提取后后台探测异常: ${e.message}`);
    });
  } finally {
    release();
  }
});

// ==================== API: 商用格式批量提取（对齐 CliProxy API提取） ====================
// GET /api/proxies?num=5&type=txt|json&format=n|rn&sid=会话ID&time=分钟
// txt 返回 ip:port 每行一条（白名单端口，白名单内免密直连）；json 返回标准响应
app.get('/api/proxies', async (req, res) => {
  const release = await acquirePoolLock();
  try {
    const num = Math.min(Math.max(parseInt(req.query.num) || 1, 1), 20);
    const type = req.query.type === 'json' ? 'json' : 'txt';
    const sep = req.query.format === 'rn' ? '\r\n' : '\n';
    const sid = typeof req.query.sid === 'string' ? req.query.sid.trim() : '';
    const minutesRaw = parseInt(req.query.time) || parseInt(req.query.minutes) || config.sticky_minutes;
    const stickyMinutes = Math.min(Math.max(minutesRaw, 1), 1440);

    const proxies = [];
    for (let i = 0; i < num; i++) {
      // 同一 sid 多次提取复用同一实例（商用粘性语义）
      const picked = pickProxy(sid || null, stickyMinutes);
      if (!picked) break;
      const access = proxyAccessFields(picked.entry);
      proxies.push({
        ip: publicIP,
        port: access.proxy_whitelist ? picked.entry.host_port + config.whitelist_port_offset : picked.entry.host_port,
        exit_ip: picked.entry.ip,
        username: access.username,
        password: access.password,
        proxy: access.proxy,
        proxy_whitelist: access.proxy_whitelist,
        whitelist_mode: !!access.proxy_whitelist,
        session_id: sid || null,
        expires_at: picked.expiresAt ? new Date(picked.expiresAt).toISOString() : null,
      });
      bumpUsage(sid ? 'sticky' : 'acquires');
      probeCfOnAcquire(`${picked.entry.instance_index}`, sid || 'stateless').catch(() => { });
    }

    if (proxies.length === 0) {
      return error(res, ERROR_CODES.NO_AVAILABLE_PROXY, '无可用代理，请稍后重试');
    }

    if (type === 'txt') {
      res.setHeader('Content-Type', 'text/plain; charset=utf-8');
      return res.send(proxies.map(p => `${p.ip}:${p.port}`).join(sep) + sep);
    }
    success(res, { count: proxies.length, proxies });
  } finally {
    release();
  }
});

// ==================== API: 用量统计（数据概览图表/明细） ====================
app.get('/api/usage', (req, res) => {
  const days = Math.min(Math.max(parseInt(req.query.days) || 30, 1), 90);
  const list = [];
  for (let i = days - 1; i >= 0; i--) {
    const d = new Date(Date.now() - i * 86400000).toISOString().slice(0, 10);
    const stat = usage.days[d] || { acquires: 0, sticky: 0, releases: 0, reports: 0, rotates: 0, binds: 0, unbinds: 0 };
    list.push({ date: d, ...stat, total: (stat.acquires || 0) + (stat.sticky || 0) });
  }
  success(res, { days: list });
});

// ==================== API: 代理池 - 释放代理 ====================
app.post('/api/pool/release', async (req, res) => {
  const { session_id, result, failure_type } = req.body;
  if (!session_id) {
    return error(res, ERROR_CODES.INVALID_INPUT, '缺少 session_id');
  }

  const session = sessions.get(session_id);
  if (!session) {
    return error(res, ERROR_CODES.SESSION_NOT_FOUND, 'session_id 不存在或已过期');
  }

  const entry = pool.get(session.pool_key);
  let releasedIP = null;
  if (entry) {
    entry.status = 'available';
    releasedIP = entry.ip;

    // ── 故障释放：客户端带 failure_type（如 proxy_dead）──
    // 立即重启该实例的 MASQUE 引擎（IP 不变，秒级恢复），坏引擎不留给下一个客户端
    if (failure_type && isMasque() && failure_type !== 'normal') {
      logger.warn('POOL', `[${entry.container}:${entry.instance_index}] 故障释放 type:${failure_type}，重启引擎`);
      restartInstanceEngine(entry.instance_index, `释放上报 failure_type=${failure_type}`);
    }

    // 上报结果：累加CF统计
    if (result !== undefined && result !== null) {
      const code = parseInt(result);
      if (!isNaN(code)) {
        // 初始化CF统计
        if (entry.cf_check_count === undefined) {
          entry.cf_check_count = 0;
          entry.cf_403_count = 0;
          entry.cf_last_code = 0;
        }
        entry.cf_check_count++;
        entry.cf_last_code = code;
        if (code === 403) {
          entry.cf_403_count++;
        }

        const cf403Rate = entry.cf_check_count > 0 ? Math.round(entry.cf_403_count / entry.cf_check_count * 100) : 0;
        logger.info('POOL', `release 会话${session_id.substring(0, 8)}... IP:${releasedIP} 上报结果:HTTP ${code} → CF统计 ${entry.cf_403_count}/${entry.cf_check_count}=${cf403Rate}%`);

        // 检查是否触发自动换IP（固定IP绑定实例豁免）
        if (config.auto_rotate_on_high_403
            && !entry.dedicated
            && entry.cf_check_count >= config.cf_min_check_count
            && cf403Rate >= config.cf_403_threshold) {
          logger.warn('POOL', `[${entry.container}:${entry.instance_index}] 上报触发403自动换IP: ${cf403Rate}% >= ${config.cf_403_threshold}% (${entry.cf_403_count}/${entry.cf_check_count})`);
          const ok = await rotateInstanceIP(session.pool_key);
          if (ok) {
            logger.info('POOL', `[${entry.container}:${entry.instance_index}] 上报触发换IP成功`);
          }
        }
      }
    } else {
      logger.info('POOL', `release 会话${session_id.substring(0, 8)}... IP:${releasedIP} (无结果上报)`);
    }
  }
  sessions.delete(session_id);
  rotatePendingIfAvailable(session.pool_key);
  bumpUsage('releases');

  success(res, {
    session_id,
    released_ip: releasedIP,
    cf_403_rate: entry && entry.cf_check_count > 0 ? Math.round((entry.cf_403_count || 0) / entry.cf_check_count * 100) : null,
    cf_403_count: (entry && entry.cf_403_count) || 0,
    cf_check_count: (entry && entry.cf_check_count) || 0,
    ip_risk: (entry && entry.ip_risk) || null,
    ip_risk_label: (entry && entry.ip_risk_label) || null,
  });
});

// ==================== API: 代理池 - 上报结果（不释放） ====================
app.post('/api/pool/report', async (req, res) => {
  const { session_id, result, failure_type } = req.body;
  if (!session_id) {
    return error(res, ERROR_CODES.INVALID_INPUT, '缺少 session_id');
  }
  if ((result === undefined || result === null) && !failure_type) {
    return error(res, ERROR_CODES.INVALID_INPUT, '缺少 result (HTTP状态码) 或 failure_type');
  }

  const session = sessions.get(session_id);
  if (!session) {
    return error(res, ERROR_CODES.SESSION_NOT_FOUND, 'session_id 不存在或已过期');
  }

  const entry = pool.get(session.pool_key);
  if (!entry) {
    return error(res, ERROR_CODES.NOT_FOUND, '代理池条目不存在');
  }

  // ── 故障上报：客户端明确说实例挂了（代理连不上/挂死）──
  // result=0（连接失败）或 failure_type=proxy_dead 都走这里：
  // 立即重启该实例的 MASQUE 引擎（IP 不变，秒级恢复），坏引擎不再留给下一个客户端。
  const code = result === undefined || result === null ? 0 : (parseInt(result) || 0);
  let engineRestarted = false;
  if ((failure_type === 'proxy_dead' || code === 0) && isMasque()) {
    const connFails = (entry.conn_fail_count || 0) + 1;
    entry.conn_fail_count = connFails;
    // 单次失败可能是网络抖动；连续 2 次连接失败基本判定引擎僵死
    if (connFails >= 2) {
      engineRestarted = !!restartInstanceEngine(entry.instance_index, `连续${connFails}次连接失败`);
      entry.conn_fail_count = 0;
    } else {
      logger.warn('POOL', `[${entry.container}:${entry.instance_index}] 连接失败 x${connFails}（再失败一次即重启引擎）`);
    }
  } else {
    entry.conn_fail_count = 0;
  }

  if (isNaN(code)) {
    return error(res, ERROR_CODES.INVALID_INPUT, 'result 必须是数字');
  }

  // 初始化CF统计（result=0 连接失败不计入 403 统计——它是引擎问题不是 CF 问题）
  if (entry.cf_check_count === undefined) {
    entry.cf_check_count = 0;
    entry.cf_403_count = 0;
    entry.cf_last_code = 0;
  }
  if (code !== 0) {
    entry.cf_check_count++;
    entry.cf_last_code = code;
    if (code === 403) {
      entry.cf_403_count++;
    }
  }

  const cf403Rate = entry.cf_check_count > 0 ? Math.round(entry.cf_403_count / entry.cf_check_count * 100) : 0;
  logger.info('POOL', `report 会话${session_id.substring(0, 8)}... IP:${entry.ip} HTTP:${code}${failure_type ? ` type:${failure_type}` : ''}${engineRestarted ? ' → 引擎已重启' : ''} → CF统计 ${entry.cf_403_count}/${entry.cf_check_count}=${cf403Rate}%`);
  bumpUsage('reports');

  let rotated = false;
  // 检查是否触发自动换IP（固定IP绑定实例豁免）
  if (config.auto_rotate_on_high_403
      && !entry.dedicated
      && entry.cf_check_count >= config.cf_min_check_count
      && cf403Rate >= config.cf_403_threshold) {
    logger.warn('POOL', `[${entry.container}:${entry.instance_index}] 上报触发403自动换IP: ${cf403Rate}% >= ${config.cf_403_threshold}% (${entry.cf_403_count}/${entry.cf_check_count})`);
    rotated = await rotateInstanceIP(session.pool_key);
    if (rotated) {
      logger.info('POOL', `[${entry.container}:${entry.instance_index}] 上报触发换IP成功`);
    }
  }

  success(res, {
    session_id,
    ip: entry.ip,
    cf_403_rate: cf403Rate,
    cf_403_count: entry.cf_403_count,
    cf_check_count: entry.cf_check_count,
    rotated,
    engine_restarted: engineRestarted,
  });
});

// ==================== API: 代理池 - 状态 ====================
app.get('/api/pool/status', (req, res) => {
  const entries = Array.from(pool.entries()).map(([key, entry]) => ({
    pool_key: key,
    container: entry.container,
    instance_index: entry.instance_index,
    host_port: entry.host_port,
    ip: entry.ip,
    health: entry.health,
    status: entry.status,
    cf_403_rate: entry.cf_check_count > 0 ? Math.round((entry.cf_403_count || 0) / entry.cf_check_count * 100) : null,
    cf_403_count: entry.cf_403_count || 0,
    cf_check_count: entry.cf_check_count || 0,
    cf_last_code: entry.cf_last_code || 0,
    ip_risk: entry.ip_risk || null,
    ip_risk_label: entry.ip_risk_label || null,
    dedicated: !!entry.dedicated,
    ...proxyAccessFields(entry),
  }));

  const activeSessions = Array.from(sessions.values()).map(s => {
    const entry = pool.get(s.pool_key);
    return {
      session_id: s.session_id,
      pool_key: s.pool_key,
      ip: entry ? entry.ip : null,
      port: entry ? entry.host_port : null,
      acquired_at: new Date(s.acquired_at).toISOString(),
      expires_at: new Date(s.expires_at).toISOString(),
    };
  });

  success(res, {
    total: pool.size,
    available: entries.filter(e => !e.dedicated && e.status === 'available' && e.health === 'healthy').length,
    in_use: entries.filter(e => e.status === 'in_use').length,
    unhealthy: entries.filter(e => e.health === 'unhealthy').length,
    dedicated: entries.filter(e => e.dedicated).length,
    instances: entries,
    sessions: activeSessions,
  });
});

// ==================== API: 代理池 - 强制换IP ====================
app.post('/api/pool/rotate', async (req, res) => {
  const { session_id, pool_key } = req.body;

  if (pool_key) {
    // 通过 pool_key 换指定实例的IP（固定IP绑定实例需 body.force=true 强制）
    const entry = pool.get(pool_key);
    if (!entry) {
      return error(res, ERROR_CODES.SESSION_NOT_FOUND, 'pool_key 不存在: ' + pool_key);
    }
    if (entry.dedicated && !req.body.force) {
      return error(res, ERROR_CODES.DEDICATED_BOUND, `实例 ${entry.container} 已绑定固定IP；确认换IP请传 force=true（换后仍保持绑定）`);
    }

    const oldIP = entry.ip;
    const job = createJob('rotate', 1); bumpUsage('rotates');
    success(res, { jobId: job.id });

    (async () => {
      addLog(job, `[${entry.container}:${entry.instance_index}] 开始换IP (当前: ${oldIP})`, 'info');
      const ok = await rotateInstanceIP(pool_key, { force: !!req.body.force });
      if (ok) {
        addLog(job, `[${entry.container}:${entry.instance_index}] 换IP成功: ${oldIP} -> ${entry.ip}`, 'success');
        job.results.push({ success: true, old_ip: oldIP, new_ip: entry.ip });
      } else {
        addLog(job, `[${entry.container}:${entry.instance_index}] 换IP失败`, 'fail');
        job.results.push({ success: false, error: '换IP失败' });
      }
      job.current = 1;
      job.status = 'done';
    })().catch(err => {
      job.status = 'error';
      addLog(job, `任务异常: ${err.message}`, 'fail');
    });
  } else if (session_id) {
    // 换单个实例的IP
    const session = sessions.get(session_id);
    if (!session) {
      return error(res, ERROR_CODES.SESSION_NOT_FOUND, 'session_id 不存在或已过期');
    }

    const entry = pool.get(session.pool_key);
    if (!entry) {
      return error(res, ERROR_CODES.SESSION_NOT_FOUND, '对应的代理池条目不存在');
    }

    const oldIP = entry.ip;
    const job = createJob('rotate', 1); bumpUsage('rotates');
    success(res, { jobId: job.id });

    (async () => {
      addLog(job, `[${entry.container}:${entry.instance_index}] 开始换IP (当前: ${oldIP})`, 'info');
      const ok = await rotateInstanceIP(session.pool_key);
      if (ok) {
        addLog(job, `[${entry.container}:${entry.instance_index}] 换IP成功: ${oldIP} -> ${entry.ip}`, 'success');
        job.results.push({ success: true, old_ip: oldIP, new_ip: entry.ip });
      } else {
        addLog(job, `[${entry.container}:${entry.instance_index}] 换IP失败`, 'fail');
        job.results.push({ success: false, error: '换IP失败' });
      }
      job.current = 1;
      job.status = 'done';
    })().catch(err => {
      job.status = 'error';
      addLog(job, `任务异常: ${err.message}`, 'fail');
    });
  } else {
    // 换所有实例的IP（固定IP绑定实例排除）
    const keys = Array.from(pool.keys()).filter(k => {
      const e = pool.get(k);
      return e.status === 'available' && !e.dedicated;
    });

    const job = createJob('rotate', keys.length); bumpUsage('rotates', keys.length);
    success(res, { jobId: job.id });

    (async () => {
      for (let i = 0; i < keys.length; i++) {
        const key = keys[i];
        const entry = pool.get(key);
        const oldIP = entry.ip;
        addLog(job, `[${entry.container}:${entry.instance_index}] 换IP中... (当前: ${oldIP})`, 'info');
        const ok = await rotateInstanceIP(key);
        if (ok) {
          addLog(job, `[${entry.container}:${entry.instance_index}] 换IP成功: ${oldIP} -> ${entry.ip}`, 'success');
          job.results.push({ success: true, pool_key: key, old_ip: oldIP, new_ip: entry.ip });
        } else {
          addLog(job, `[${entry.container}:${entry.instance_index}] 换IP失败`, 'fail');
          job.results.push({ success: false, pool_key: key, error: '换IP失败' });
        }
        job.current = i + 1;
        // 实例间间隔，防止API限流
        await sleep(2000);
      }
      job.status = 'done';
      addLog(job, `批量换IP完成!`, 'info');
    })().catch(err => {
      job.status = 'error';
      addLog(job, `任务异常: ${err.message}`, 'fail');
    });
  }
});

// ==================== API: 固定IP（专用实例绑定） ====================
// 绑定后该实例锁定当前 IPv6 出口：不参与轮换、豁免所有自动换IP与自动重建，
// 直到调用 unbind 解绑。绑定状态持久化在实例 meta.json，服务重启自动恢复。
// body: { name?: 指定实例, count?: 批量绑定数量, remark?: 备注 }
app.post('/api/pool/dedicated/bind', async (req, res) => {
  const release = await acquirePoolLock();
  try {
    const { name, remark } = req.body || {};
    const count = Math.min(Math.max(parseInt(req.body && req.body.count) || 1, 1), 20);
    const boundAt = Date.now();
    const items = [];

    const bindEntry = (entry) => {
      entry.dedicated = true;
      entry.dedicated_at = boundAt;
      entry.pending_rotate = false;
      entry.dedicated_remark = remark || entry.dedicated_remark || '';
      setDedicatedFlag(entry.instance_index, true, entry.dedicated_remark);
      logger.info('DEDICATED', `[${entry.container}] 绑定固定IP → ${entry.ip} (端口 ${entry.host_port})`);
      bumpUsage('binds');
      pushBindHistory({
        name: entry.container, ip: entry.ip, port: entry.host_port,
        remark: entry.dedicated_remark, bound_at: new Date(boundAt).toISOString(), unbound_at: null,
      });
      items.push({
        name: entry.container,
        ip: entry.ip,
        bound_at: new Date(boundAt).toISOString(),
        remark: entry.dedicated_remark,
        already_bound: false,
        ...proxyAccessFields(entry),
      });
    };

    if (name) {
      if (!validateContainerName(name)) {
        return error(res, ERROR_CODES.INVALID_INPUT, `无效的实例名称: ${name}`);
      }
      const idx = parseInt(name.match(/warp-(\d+)/)[1]);
      const entry = pool.get(`${idx}`);
      if (!entry) {
        return error(res, ERROR_CODES.NOT_FOUND, `实例 ${name} 不在代理池中（未启动或未创建）`);
      }
      if (entry.dedicated) {
        // 幂等：已绑定直接返回
        return success(res, {
          count: 1,
          items: [{
            name: entry.container,
            ip: entry.ip,
            bound_at: entry.dedicated_at ? new Date(entry.dedicated_at).toISOString() : null,
            remark: entry.dedicated_remark || '',
            already_bound: true,
            ...proxyAccessFields(entry),
          }],
        });
      }
      bindEntry(entry);
    } else {
      // 自动挑选可用且健康的非绑定实例（批量）
      for (let i = 0; i < count; i++) {
        let entry = null;
        for (const [, e] of pool) {
          if (!e.dedicated && e.status === 'available' && e.health !== 'unhealthy' && e.ip) {
            entry = e;
            break;
          }
        }
        if (!entry) break;
        bindEntry(entry);
      }
      if (items.length === 0) {
        return error(res, ERROR_CODES.NO_AVAILABLE_PROXY, '无可绑定的实例（需要 available + healthy + 有出口IP）');
      }
    }

    success(res, { count: items.length, items });
  } finally {
    release();
  }
});

// 解除固定IP绑定，实例归还普通代理池
app.post('/api/pool/dedicated/unbind', async (req, res) => {
  const { name, force } = req.body || {};
  if (!name || !validateContainerName(name)) {
    return error(res, ERROR_CODES.INVALID_INPUT, '缺少或无效的实例名称');
  }
  const idx = parseInt(name.match(/warp-(\d+)/)[1]);
  const poolKey = `${idx}`;
  const entry = pool.get(poolKey);
  if (!entry) {
    return error(res, ERROR_CODES.NOT_FOUND, `实例 ${name} 不在代理池中`);
  }
  if (!entry.dedicated && !isInstanceDedicated(idx)) {
    return success(res, { name, released: false, message: '该实例未处于绑定状态' });
  }

  entry.dedicated = false;
  delete entry.dedicated_at;
  delete entry.dedicated_remark;
  setDedicatedFlag(idx, false);
  closeBindHistory(name);
  logger.info('DEDICATED', `[${entry.container}] 解除固定IP绑定，归还普通代理池`);
  bumpUsage('unbinds');

  // 解绑后可选立即换IP（该IP已被独占使用过）
  if (force) {
    const job = createJob('rotate', 1); bumpUsage('rotates');
    success(res, { name, released: true, rotate_job_id: job.id });
    (async () => {
      addLog(job, `[${entry.container}] 解绑后强制换IP`, 'info');
      const ok = await rotateInstanceIP(poolKey, { force: true });
      addLog(job, ok ? `[${entry.container}] 换IP成功 → ${entry.ip}` : `[${entry.container}] 换IP失败`, ok ? 'success' : 'fail');
      job.current = 1;
      job.status = 'done';
    })().catch(err => {
      job.status = 'error';
      addLog(job, `任务异常: ${err.message}`, 'fail');
    });
    return;
  }

  success(res, { name, released: true });
});

// 固定IP绑定列表
app.get('/api/pool/dedicated', (req, res) => {
  const items = [];
  for (const [key, e] of pool) {
    if (!e.dedicated) continue;
    items.push({
      pool_key: key,
      name: e.container,
      instance_index: e.instance_index,
      host_port: e.host_port,
      ip: e.ip,
      health: e.health,
      ip_risk: e.ip_risk || null,
      ip_risk_label: e.ip_risk_label || null,
      bound_at: e.dedicated_at ? new Date(e.dedicated_at).toISOString() : null,
      remark: e.dedicated_remark || '',
      ...proxyAccessFields(e),
    });
  }
  success(res, { total: items.length, items });
});

// 修改固定IP绑定备注（列表内联编辑）
app.post('/api/pool/dedicated/remark', (req, res) => {
  const { name, remark } = req.body || {};
  if (!name || !validateContainerName(name)) {
    return error(res, ERROR_CODES.INVALID_INPUT, '缺少或无效的实例名称');
  }
  const idx = parseInt(name.match(/warp-(\d+)/)[1]);
  const entry = pool.get(`${idx}`);
  if (!entry || !entry.dedicated) {
    return error(res, ERROR_CODES.NOT_FOUND, `实例 ${name} 未处于绑定状态`);
  }
  const val = String(remark || '').slice(0, 100);
  entry.dedicated_remark = val;
  setDedicatedFlag(idx, true, val);
  // 同步历史记录中未关闭的条目
  if (Array.isArray(usage.bindHistory)) {
    const rec = usage.bindHistory.find(r => r.name === name && !r.unbound_at);
    if (rec) { rec.remark = val; saveUsageSoon(); }
  }
  success(res, { name, remark: val });
});

// 固定IP绑定历史（长效静态IP页「历史记录」Tab）
app.get('/api/pool/dedicated/history', (req, res) => {
  const list = Array.isArray(usage.bindHistory) ? usage.bindHistory : [];
  success(res, { total: list.length, items: list });
});

// 重置全部账密：重新生成所有实例的代理密码并热更新（用户名不变，出口IP不变）
app.post('/api/creds/reset', (req, res) => {
  const job = startReapplyAccessJob(false); // 先建job占位，下面改密码后再真正热更新
  job.total = 0; job.current = 0; job.status = 'running';
  (async () => {
    try {
      // 重置全局密码（用户名不变）
      config.proxy_password = crypto.randomBytes(9).toString('base64url');
      saveConfig();
      addLog(job, `新密码已生成: ${config.proxy_password}`, 'success');
      // 热更新全部实例配置（写进 sing-box 并重启进程，IP不变）
      await reapplyAllAccess(job);
      job.status = 'done';
      addLog(job, '账密重置完成', 'success');
    } catch (e) {
      job.status = 'error';
      addLog(job, `任务异常: ${e.message}`, 'fail');
    }
  })();
  success(res, { password: null, job_id: job.id });
});

// 热更新全部实例接入配置（复用 job 上下文记录日志）
async function reapplyAllAccess(job) {
  const instances_list = await listWarpInstances();
  const base = job.current;
  job.total = Math.max(job.total, base + instances_list.length);
  for (const inst of instances_list) {
    try {
      const ok = await reapplyInstanceAccess(inst.index);
      addLog(job, `warp-${inst.index} 接入配置已${ok ? '热更新' : '跳过（无配置）'}`, ok ? 'success' : 'info');
    } catch (e) {
      addLog(job, `warp-${inst.index} 热更新失败: ${e.message}`, 'fail');
    }
    job.current++;
  }
}

// ==================== 定时器 ====================
function startTimers() {
  // 健康巡检定时器
  healthCheckTimer = setInterval(async () => {
    logger.info('HEALTH_CHECK', '开始定时健康巡检...');
    const keys = Array.from(pool.keys());
    for (const key of keys) {
      const entry = pool.get(key);
      // 含 health=unknown 的实例（初次出口检测失败）也参与巡检，否则会永久卡死不再重试
      if (entry.status === 'available') {
        await checkInstanceHealth(key);
      }
    }
    const healthy = Array.from(pool.values()).filter(e => e.health === 'healthy').length;
    const unhealthy = Array.from(pool.values()).filter(e => e.health === 'unhealthy').length;
    logger.info('HEALTH_CHECK', `巡检完成: healthy=${healthy} unhealthy=${unhealthy}`);
  }, config.health_check_interval * 1000);

  // session清理定时器
  sessionCleanupTimer = setInterval(() => {
    const now = Date.now();
    let cleaned = 0;
    for (const [sessionId, session] of sessions) {
      if (now > session.expires_at) {
        const entry = pool.get(session.pool_key);
        if (entry) {
          entry.status = 'available';
        }
        sessions.delete(sessionId);
        rotatePendingIfAvailable(session.pool_key);
        cleaned++;
      }
    }
    if (cleaned > 0) {
      logger.info('SESSION', `清理 ${cleaned} 个过期 session，归还 ${cleaned} 个 IP`);
    }
  }, config.session_cleanup_interval * 1000);

  logger.info('TIMER', `定时器已启动: 健康巡检=${config.health_check_interval}s, session清理=${config.session_cleanup_interval}s`);
}

// ==================== 服务启动恢复 ====================
async function recoverFromExistingContainers() {
  logger.info('RECOVERY', '开始扫描已有WARP实例...');
  recoveryInProgress = true;
  try {
    const instances_list = await listWarpInstances();
    const running = instances_list.filter(i => i.running);

    let recoveredInstances = 0;
    let availableIPs = 0;

    for (const inst of running) {
      const poolKey = `${inst.index}`;
      if (pool.has(poolKey)) continue;

      const exit = await checkExitIP(inst.port);
      const ip = exit.ip;
      pool.set(poolKey, {
        container: inst.name,
        instance_index: inst.index,
        host_port: inst.port,
        ip,
        ipv6: exit.v6,
        ipv4: exit.v4,
        health: ip ? 'healthy' : 'unhealthy',
        status: 'available',
        dedicated: inst.dedicated || undefined,
        dedicated_remark: inst.dedicated_remark || '',
      });
      if (inst.dedicated) {
        logger.info('DEDICATED', `[${inst.name}] 恢复固定IP绑定状态 (IP:${ip || '未知'})`);
      }
      recoveredInstances++;
      if (ip) availableIPs++;
    }

    logger.info('RECOVERY', `恢复 ${running.length} 个运行实例，${recoveredInstances} 个加入代理池，${availableIPs} 个可用IP`);

    await adjustResidentCount();
  } finally {
    recoveryInProgress = false;
  }
}

async function adjustResidentCount() {
  const instances_list = await listWarpInstances();
  const runningIndices = new Set();
  for (const inst of instances_list) {
    if (inst.running) runningIndices.add(inst.index);
  }

  // 创建缺失的实例
  let tunnelFailStreak = 0; // 连续隧道失败计数（复用失败+新注册也失败 = WARP对端疑似被限流）
  for (let i = 0; i < config.resident_count; i++) {
    if (runningIndices.has(i)) continue;
    if (tunnelFailStreak >= 3) {
      logger.error('RECOVERY', `连续${tunnelFailStreak}个实例隧道检测失败，WARP对端疑似被限流/封锁，中止本次恢复（剩余实例保留磁盘配置，重启服务后再试）`);
      break;
    }
    const singboxPath = path.join(getInstanceDir(i), 'singbox.json');

    // 已有配置 → 复用启动（服务重启/容器重建后不重新注册账号，秒级恢复）
    if (fs.existsSync(singboxPath)) {
      try {
        const ok = await reuseExistingInstance(i);
        if (ok) { tunnelFailStreak = 0; continue; }
        logger.warn('RECOVERY', `warp-${i} 复用配置启动失败，转为重新注册`);
      } catch (e) {
        logger.error('RECOVERY', `warp-${i} 复用配置异常: ${e.message}`);
      }
    }

    logger.info('RECOVERY', `创建实例 warp-${i}`);
    try {
      await createWarpInstance(i);
      tunnelFailStreak = 0;
    } catch (e) {
      tunnelFailStreak++;
      logger.error('RECOVERY', `创建实例 warp-${i} 失败: ${e.message}（连续隧道失败${tunnelFailStreak}次）`);
    }
  }

  // 删除超出 resident_count 的多余实例（固定IP绑定实例保留，直到解绑）
  for (const inst of instances_list) {
    if (inst.index >= config.resident_count && !inst.dedicated) {
      try {
        await deleteWarpInstance(inst.name);
        logger.info('RECOVERY', `[${inst.name}] 多余实例已删除`);
      } catch (e) {
        logger.error('RECOVERY', `[${inst.name}] 删除失败: ${e.message}`);
      }
    } else if (inst.index >= config.resident_count && inst.dedicated) {
      logger.info('DEDICATED', `[${inst.name}] 固定IP绑定实例超出 resident_count，保留不删除`);
    }
  }
}

// 复用已有 singbox.json 配置启动实例（不重新注册账号，供服务重启/容器重建后快速恢复）
async function reuseExistingInstance(instanceIndex) {
  const name = getInstanceName(instanceIndex);
  const port = getInstancePort(instanceIndex);
  const dir = getInstanceDir(instanceIndex);
  const configPath = path.join(dir, 'singbox.json');
  const metaPath = path.join(dir, 'meta.json');

  logger.info('RECOVERY', `复用已有配置启动 ${name} (端口:${port})`);

  // 清理可能残留的旧进程
  let oldMeta = {};
  try {
    if (fs.existsSync(metaPath)) oldMeta = JSON.parse(fs.readFileSync(metaPath, 'utf8'));
  } catch (e) { }
  if (oldMeta.pid) stopSingbox(oldMeta.pid);
  if (oldMeta.engine_pid) stopEngine(oldMeta.engine_pid);

  // 按当前接入模式/传输重写配置（应用账密/白名单；账号不变 → IP不变），失败则沿用原配置
  try {
    const { username, password } = getProxyCreds();
    const wlCidrs = whitelistToCidrs();
    if (isMasque()) {
      // MASQUE: 引擎账号持久化在实例目录；已有账号则原样复用（IP不变），否则自动注册新账号
      if (!fs.existsSync(engineConfigPath(instanceIndex))) {
        logger.warn('RECOVERY', `[${name}] 缺少 MASQUE 账号，将注册新账号（出口IP会变化）`);
      }
      const enginePid = startMasqueEngine(instanceIndex);
      oldMeta.engine_pid = enginePid;
      oldMeta.transport = 'masque';
      const newCfg = buildSingboxConfig({
        port, wlPort: wlCidrs.length ? port + config.whitelist_port_offset : null,
        wlCidrs, authUsers: config.proxy_auth_enabled ? [{ username, password }] : null,
        transport: 'masque', enginePort: enginePort(instanceIndex),
      });
      fs.writeFileSync(configPath, JSON.stringify(newCfg, null, 2));
      logger.info('RECOVERY', `[${name}] 已按当前接入模式重写配置 (auth=${config.proxy_auth_enabled ? '开' : '关'}, whitelist=${wlCidrs.length}条, engine PID:${enginePid})`);
    } else {
      const old = JSON.parse(fs.readFileSync(configPath, 'utf8'));
      const ep0 = old.endpoints && old.endpoints[0];
      const peer = ep0 && ep0.peers && ep0.peers[0];
      if (ep0 && peer && ep0.private_key) {
        const ipv6Addr = (ep0.address.find(a => a.includes(':')) || '').replace(/\/128$/, '');
        if (ipv6Addr) {
          const newCfg = buildSingboxConfig({
            privateKey: ep0.private_key,
            ipv6Addr,
            port,
            wlPort: wlCidrs.length ? port + config.whitelist_port_offset : null,
            wlCidrs,
            authUsers: config.proxy_auth_enabled ? [{ username, password }] : null,
            endpoint: { address: peer.address, port: peer.port },
          });
          fs.writeFileSync(configPath, JSON.stringify(newCfg, null, 2));
          logger.info('RECOVERY', `[${name}] 已按当前接入模式重写配置 (auth=${config.proxy_auth_enabled ? '开' : '关'}, whitelist=${wlCidrs.length}条)`);
        }
      }
    }
  } catch (e) {
    logger.warn('RECOVERY', `[${name}] 配置重写失败，沿用原配置: ${e.message}`);
  }

  // 启动 sing-box
  const pid = startSingbox(instanceIndex, configPath);
  logger.info('INSTANCE', `[${name}] sing-box已启动 (PID:${pid})`);

  // 等待隧道建立
  await sleep(8000);

  // 检测出口 IP（v6 优先重试）
  const poolKey = `${instanceIndex}`;
  const exit = await checkExitIP(port);
  const ip = exit.ip;

  // L2 检测
  const httpCode = await checkOpenAIAccess(port);
  const isHealthy = (httpCode === 200 || httpCode === 302 || httpCode === 403);

  // 更新元数据
  const meta = { ...oldMeta, pid, port, ip, endpoint: oldMeta.endpoint || null, updatedAt: Date.now() };
  fs.writeFileSync(metaPath, JSON.stringify(meta, null, 2));

  pool.set(poolKey, {
    container: name,
    instance_index: instanceIndex,
    host_port: port,
    ip,
    ipv6: exit.v6,
    ipv4: exit.v4,
    health: ip ? (isHealthy ? 'healthy' : 'unhealthy') : 'unknown',
    status: 'available',
    dedicated: oldMeta.dedicated || undefined,
    dedicated_at: oldMeta.dedicated ? (oldMeta.dedicated_at || null) : undefined,
    dedicated_remark: oldMeta.dedicated ? (oldMeta.dedicated_remark || '') : undefined,
    cf_check_count: 1,
    cf_403_count: httpCode === 403 ? 1 : 0,
    cf_last_code: httpCode,
  });

  if (oldMeta.dedicated) {
    logger.info('DEDICATED', `[${name}] 复用启动后保持固定IP绑定 (IP:${ip || '未知'})`);
  }

  // 出口检测失败：清理进程、移出代理池，由调用方 fallback 重新注册
  if (!ip) {
    try { stopSingbox(pid); } catch (e) { }
    try { if (oldMeta.engine_pid) stopEngine(oldMeta.engine_pid); } catch (e) { }
    pool.delete(poolKey);
    logger.warn('RECOVERY', `[${name}] 出口检测失败，已停止实例进程并移出代理池`);
    return false;
  }

  logger.info('RECOVERY', `[${name}] 复用启动完成, IP:${ip} health=${ip ? (isHealthy ? 'healthy' : 'unhealthy') : 'unknown'} CF:${httpCode}`);
  return true;
}

// ==================== 优雅关闭 ====================
async function gracefulShutdown(signal) {
  logger.info('SERVER', `收到 ${signal} 信号，开始优雅关闭...`);

  if (healthCheckTimer) clearInterval(healthCheckTimer);
  if (sessionCleanupTimer) clearInterval(sessionCleanupTimer);

  if (config.cleanup_on_exit) {
    try {
      const instances = await listWarpInstances();
      for (const inst of instances) {
        if (inst.pid || inst.enginePid) {
          try {
            stopSingbox(inst.pid);
            stopEngine(inst.enginePid);
            logger.info('SERVER', `[${inst.name}] 实例进程已停止 (pid:${inst.pid}, engine:${inst.enginePid})`);
          } catch (e) {
            logger.error('SERVER', `[${inst.name}] 停止失败: ${e.message}`);
          }
        }
      }
    } catch (e) {
      logger.error('SERVER', '清理实例失败: ' + e.message);
    }
  }

  logger.info('SERVER', '服务已关闭');
  process.exit(0);
}

process.on('SIGINT', () => gracefulShutdown('SIGINT'));
process.on('SIGTERM', () => gracefulShutdown('SIGTERM'));
process.on('SIGBREAK', () => gracefulShutdown('SIGBREAK'));
process.on('unhandledRejection', (reason, promise) => {
  logger.error('SERVER', '未处理的Promise拒绝: ' + (reason && reason.message ? reason.message : reason));
});
process.on('uncaughtException', (err) => {
  logger.error('SERVER', '未捕获的异常: ' + (err && err.message ? err.message : err));
});

// ==================== 启动服务 ====================
const server = app.listen(config.port, config.bind_address, async () => {
  await detectPublicIP();

  console.log('');
  console.log('========================================');
  console.log('  WARP 代理池管理系统已启动');
  console.log(`  访问地址: http://${config.bind_address === '0.0.0.0' ? 'localhost' : config.bind_address}:${config.port}`);
  console.log(`  API 文档: http://${config.bind_address === '0.0.0.0' ? 'localhost' : config.bind_address}:${config.port}/docs`);
  console.log(`  公网IP: ${publicIP}`);
  console.log(`  接入模式: 账密=${config.proxy_auth_enabled ? '开' : '关'} | 白名单=${(config.ip_whitelist || []).length}条（API与代理端口共用）`);
  console.log(`  实例数: ${config.resident_count} (最大 ${config.max_instances})`);
  console.log(`  基础端口: ${config.base_port}`);
  console.log(`  模式: sing-box + wgcf`);
  console.log(`  绑定地址: ${config.bind_address}`);
  console.log('========================================');
  console.log('');

  logger.info('SERVER', `服务启动 端口:${config.port} 绑定:${config.bind_address}`);
  logger.info('SERVER', `接入模式: 账密=${config.proxy_auth_enabled ? '开' : '关'} 白名单=${(config.ip_whitelist || []).join(',') || '无'}`);

  // 启动定时器
  startTimers();

  // 探测WARP对端可用性（异步，不阻塞启动）
  probeEndpoints().catch(e => logger.warn('ENDPOINT', 'endpoint探测失败: ' + e.message));

  // 恢复已有实例
  try {
    await recoverFromExistingContainers();
  } catch (e) {
    logger.error('SERVER', '实例恢复失败: ' + e.message);
  }
});

module.exports = app;
