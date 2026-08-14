const express = require('express');
const { exec, execSync, spawn } = require('child_process');
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
const WARP_ENDPOINT_IP = '188.114.96.1';
const WARP_ENDPOINT_PORT = 2408;
const WARP_PUBLIC_KEY = 'bmXOC+F1FxEMF9dyiK2H5/1SUtzH0JuVo51h2wPfgyo=';
// 候选WARP对端（endpoint），同一账号连接不同endpoint会得到不同出口IP
// 实测可用: 162.159.192.1 / 188.114.96.1 / 188.114.97.1 / 188.114.96.2 / 188.114.96.3
const WARP_ENDPOINTS = [
  { address: '188.114.96.1', port: 2408 },
  { address: '188.114.97.1', port: 2408 },
  { address: '188.114.96.2', port: 2408 },
  { address: '188.114.96.3', port: 2408 },
  { address: '162.159.192.1', port: 2408 },
];

const DEFAULT_CONFIG = {
  port: 4433,
  max_containers: 1,
  resident_count: 5,
  sticky_minutes: 10,
  auto_rebuild_on_unhealthy: true,
  health_check_interval: 600,
  api_key: crypto.randomUUID(),
  bind_address: '0.0.0.0',
  mode: 'resident',
  base_port: 10010,
  max_instances: 200,
  min_available: 5,
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
  log_level: 'info',
  public_ip: 'auto-detect',
  session_cleanup_interval: 30,
  // WARP Teams (Zero Trust) 无限流量: 填写团队Token后注册的账号为Teams账户(无限流量)
  // 获取地址: https://web--public--warp-team-api--coia-mfs4.code.run/
  team_token: '',
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

const config = loadConfig();
logger.setLogLevel(config.log_level);

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

// ==================== 统一响应 ====================
function success(res, data) {
  res.json({ success: true, data });
}

function error(res, code, message, httpStatus = 400) {
  res.status(httpStatus).json({
    success: false,
    error: { code, message },
  });
}

const ERROR_CODES = {
  UNAUTHORIZED: { status: 401, message: 'API Key 缺失或错误' },
  RATE_LIMITED: { status: 429, message: '请求频率超限' },
  NO_AVAILABLE_PROXY: { status: 503, message: '无可用代理，请稍后重试' },
  SESSION_NOT_FOUND: { status: 404, message: 'session_id 不存在或已过期' },
  INVALID_INPUT: { status: 400, message: '参数校验失败' },
  INSTANCE_ERROR: { status: 500, message: '实例操作失败' },
  IP_CHECK_FAILED: { status: 503, message: 'IP 检测失败' },
  INTERNAL_ERROR: { status: 500, message: '未知错误' },
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
    logger.info('REQUEST', `${req.method} ${req.path} ${res.statusCode} ${ms}ms`);
  });
  next();
});

// ==================== API Key 鉴权（已禁用） ====================
function apiKeyAuth(req, res, next) {
  next();
}

// ==================== 限流 ====================
const rateLimitMap = new Map();

function rateLimit(req, res, next) {
  if (!config.rate_limit_enabled) return next();
  const key = req.headers['x-api-key'] || req.ip || 'anonymous';
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
    return error(res, 'RATE_LIMITED', '请求频率超限', 429);
  }
  record.count++;
  next();
}

// 对代理池API和管理API加鉴权+限流
app.use('/api/pool', apiKeyAuth, rateLimit);
app.use('/api/create', apiKeyAuth, rateLimit);
app.use('/api/delete', apiKeyAuth, rateLimit);
app.use('/api/rebuild', apiKeyAuth, rateLimit);
app.use('/api/start', apiKeyAuth, rateLimit);
app.use('/api/stop', apiKeyAuth, rateLimit);
app.use('/api/config', (req, res, next) => {
  // GET 不需要鉴权（前端获取配置），POST 需要
  if (req.method === 'POST') return apiKeyAuth(req, res, next);
  next();
});

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
// L1: 连通性检测 - 强制IPv6出口（每个WARP账号有独立IPv6地址，返回不同IP），回退IPv4
async function checkExitIP(port) {
  const tag = `L1[端口${port}]`;
  logger.debug('IP_CHECK', `${tag} 开始IP检测 (最多${config.ip_check_retries}次)`);
  
  for (let i = 0; i < config.ip_check_retries; i++) {
    const attempt = i + 1;
    const t0 = Date.now();
    try {
      // 强制IPv6: 每个WARP账号分配独立IPv6地址，出口IP必然不同
      // 注意: --socks5-hostname 下DNS由代理解析(api6.ipify.org仅AAAA→走IPv6出口)，无需 -6
      //       新版curl(7.88+)加 -6 会因代理地址127.0.0.1是IPv4字面量而立即连接失败
      const ipPrimary = await runCmdFast(
        `curl -s --max-time 10 --socks5-hostname 127.0.0.1:${port} ${config.ip_check_url_v6}`, 12000
      ).catch(() => '');
      // 回退: IPv4出口（共享NAT，仅用于连通性确认）
      const ipFallback = await runCmdFast(
        `curl -s --max-time 10 --socks5-hostname 127.0.0.1:${port} ${config.ip_check_url}`, 12000
      ).catch(() => '');
      const elapsed = Date.now() - t0;
      const primaryTrim = ipPrimary.trim();
      const fallbackTrim = ipFallback.trim();
      
      // 优先使用IPv6结果（每个WARP账号唯一IPv6地址）
      if (primaryTrim && (primaryTrim.includes(':') || /^\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(primaryTrim))) {
        logger.info('IP_CHECK', `${tag} 第${attempt}次成功 → 出口IP:${primaryTrim} (api4:${fallbackTrim}) (${elapsed}ms)`);
        return primaryTrim;
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
        return fallbackTrim;
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
  return null;
}

// L2: CF可访问性检测（返回HTTP状态码，0=连接失败）
async function checkOpenAIAccess(port) {
  const tag = `L2[端口${port}]`;
  const url = config.openai_check_url;
  const t0 = Date.now();
  
  try {
    logger.debug('IP_CHECK', `${tag} 开始CF检测 → ${url} (超时15s)`);
    
    const result = await runCmd(
      `curl -s -o /dev/null -w "%{http_code}" --max-time 15 --socks5-hostname 127.0.0.1:${port} ${url}`,
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

async function checkIpRiskScore(ip) {
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
    const html = await runCmdFast(
      `curl -s --max-time 12 -A "${BROWSER_UA}" ${url}`, 15000
    ).catch(() => '');

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
        && entry.cf_check_count >= config.cf_min_check_count
        && rate >= config.cf_403_threshold) {
      entry.pending_rotate = true;
      logger.warn('CF_USAGE', `${tag} 403率${rate}%达到阈值${config.cf_403_threshold}%，标记为自动换IP`);
      rotatePendingIfAvailable(poolKey);
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
  const ip = await checkExitIP(entry.host_port);
  
  if (!ip) {
    const elapsed = Date.now() - t0;
    entry.health = 'unhealthy';
    logger.warn('HEALTH_CHECK', `${tag} 判定: 不健康 ❌ (L1失败-IP不可达, ${elapsed}ms) → 当前IP:${entry.ip} 健康度:${entry.health}`);
    if (config.auto_rebuild_on_unhealthy && entry.status === 'available') {
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
    const risk = await checkIpRiskScore(ip);
    entry.ip_risk = risk ? risk.score : null;
    entry.ip_risk_label = risk ? getRiskLabel(risk.risk) : null;

    if (risk && risk.score >= config.ip_risk_threshold) {
      logger.warn('HEALTH_CHECK', `${tag} 出口IP风控值${risk.score}(${getRiskLabel(risk.risk)}) ≥ 阈值${config.ip_risk_threshold}，判定为风控IP，触发自动换IP`);
      if (entry.status === 'available' && !entry.rotate_running) {
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
    if (config.auto_rebuild_on_unhealthy && entry.status === 'available') {
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
    if (config.auto_rebuild_on_unhealthy && entry.status === 'available') {
      logger.info('HEALTH_CHECK', `${tag} 触发自动重建 (auto_rebuild_on_unhealthy=true)`);
      await rotateInstanceIP(poolKey);
    }
    return;
  }

  // ---- 403率自动换IP ----
  if (config.auto_rotate_on_high_403
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
async function rotateInstanceIP(poolKey) {
  const entry = pool.get(poolKey);
  if (!entry) return false;

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

      // 停止旧sing-box进程
      const metaPath = path.join(getInstanceDir(entry.instance_index), 'meta.json');
      let oldMeta = {};
      try {
        if (fs.existsSync(metaPath)) {
          oldMeta = JSON.parse(fs.readFileSync(metaPath, 'utf8'));
          if (oldMeta.pid) stopSingbox(oldMeta.pid);
        }
      } catch (e) { }

      // 重新注册WARP账号
      const { privateKey, ipv6Addr } = registerWarpAccount(entry.instance_index);

      // 换IP时轮换到下一个endpoint（同一账号不同endpoint → 不同出口IP）
      const newEndpoint = nextEndpointForInstance(entry.instance_index);

      // 生成新配置
      const configPath = generateSingboxConfig(entry.instance_index, privateKey, ipv6Addr, entry.host_port, newEndpoint);

      // 启动新sing-box
      const newPid = startSingbox(entry.instance_index, configPath);

      logger.debug('ROTATE', `${tag} 等待8秒sing-box启动...`);
      await sleep(8000);

      // 保存新元数据
      oldMeta.pid = newPid;
      oldMeta.ipv6 = ipv6Addr;
      oldMeta.endpoint = `${newEndpoint.address}:${newEndpoint.port}`;
      fs.writeFileSync(metaPath, JSON.stringify(oldMeta, null, 2));

      logger.debug('ROTATE', `${tag} 检测新IP...`);
      const newIP = await checkExitIP(entry.host_port);
      const attemptElapsed = Date.now() - attemptT0;

      if (!newIP) {
        logger.warn('ROTATE', `${tag} 第${attemptNum}次失败: 新IP检测失败 (${attemptElapsed}ms)`);
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
        newRisk = await checkIpRiskScore(newIP);
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

// 生成sing-box配置文件
// endpoint: {address, port} 可选，默认使用轮询分配的对端
function generateSingboxConfig(instanceIndex, privateKey, ipv6Addr, port, endpoint) {
  const configDir = getInstanceDir(instanceIndex);
  const configPath = path.join(configDir, 'singbox.json');

  const ep = endpoint || getEndpointForInstance(instanceIndex);

  const singboxConfig = {
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
      rules: [
        { inbound: 'mixed-in', action: 'sniff' },
        { protocol: 'dns', action: 'hijack-dns' },
        { action: 'resolve', strategy: 'prefer_ipv6' },
        { ip_is_private: true, outbound: 'direct-out' }
      ],
      auto_detect_interface: true,
      final: 'WARP'
    },
    inbounds: [
      { type: 'mixed', tag: 'mixed-in', listen: '0.0.0.0', listen_port: port }
    ],
    endpoints: [
      {
        tag: 'WARP',
        type: 'wireguard',
        address: ['172.16.0.2/32', `${ipv6Addr}/128`],
        private_key: privateKey,
        peers: [
          {
            address: ep.address,
            port: ep.port,
            public_key: WARP_PUBLIC_KEY,
            allowed_ips: ['0.0.0.0/0', '::/0'],
            persistent_keepalive_interval: 30
          }
        ],
        mtu: 1408,
        udp_fragment: true
      }
    ],
    outbounds: [
      { tag: 'direct-out', type: 'direct', udp_fragment: true }
    ]
  };

  fs.writeFileSync(configPath, JSON.stringify(singboxConfig, null, 2));
  return configPath;
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
  const logFd = fs.openSync(logPath, 'a');
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

    instances.push({
      name: dir,
      index: idx,
      port: getInstancePort(idx),
      running,
      pid: meta.pid || null,
      configExists: fs.existsSync(configPath),
      ip: meta.ip || null,
      endpoint: meta.endpoint || null,
      ipv6: meta.ipv6 || null,
    });
  }
  return instances.sort((a, b) => a.index - b.index);
}

// 创建单个WARP实例
async function createWarpInstance(instanceIndex) {
  const name = getInstanceName(instanceIndex);
  const port = getInstancePort(instanceIndex);
  const dir = getInstanceDir(instanceIndex);

  logger.info('INSTANCE', `[${name}] 开始创建 (端口:${port})`);

  // 停止旧进程
  const metaPath = path.join(dir, 'meta.json');
  if (fs.existsSync(metaPath)) {
    try {
      const oldMeta = JSON.parse(fs.readFileSync(metaPath, 'utf8'));
      if (oldMeta.pid) stopSingbox(oldMeta.pid);
    } catch (e) { }
  }

  // 创建目录
  if (!fs.existsSync(dir)) {
    fs.mkdirSync(dir, { recursive: true });
  }

  // 注册WARP账号
  logger.debug('INSTANCE', `[${name}] 注册WARP账号...`);
  const { privateKey, ipv6Addr } = registerWarpAccount(instanceIndex);
  logger.info('INSTANCE', `[${name}] WARP账号注册成功, IPv6: ${ipv6Addr}`);

  // 生成sing-box配置（轮询分配不同endpoint → 出口IP更分散）
  const endpoint = getEndpointForInstance(instanceIndex);
  const configPath = generateSingboxConfig(instanceIndex, privateKey, ipv6Addr, port, endpoint);
  logger.debug('INSTANCE', `[${name}] sing-box配置已生成 endpoint=${endpoint.address}:${endpoint.port}`);

  // 启动sing-box
  const pid = startSingbox(instanceIndex, configPath);
  logger.info('INSTANCE', `[${name}] sing-box已启动 (PID:${pid})`);

  // 等待连接建立
  await sleep(8000);

  // 检测出口IP
  const poolKey = `${instanceIndex}`;
  const ip = await checkExitIP(port);

  // L2检测
  const httpCode = await checkOpenAIAccess(port);
  const isHealthy = (httpCode === 200 || httpCode === 302 || httpCode === 403);

  // 保存元数据
  const meta = { pid, port, ip, ipv6: ipv6Addr, endpoint: `${endpoint.address}:${endpoint.port}`, createdAt: Date.now() };
  fs.writeFileSync(metaPath, JSON.stringify(meta, null, 2));

  pool.set(poolKey, {
    container: name,
    instance_index: instanceIndex,
    host_port: port,
    ip,
    health: ip ? (isHealthy ? 'healthy' : 'unhealthy') : 'unknown',
    status: 'available',
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
      if (meta.pid) stopSingbox(meta.pid);
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
    try {
      wgcfVersion = await runCmdFast(`${WGCF_BIN} --version 2>&1 | head -1`, 5000);
    } catch (e) { }
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
    error(res, 'INSTANCE_ERROR', '环境检查失败: ' + err.message, 500);
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
        instances.push({
          container: inst.name,
          instance_index: poolEntry.instance_index,
          host_port: poolEntry.host_port,
          ip: poolEntry.ip,
          endpoint: inst.endpoint,
          health: poolEntry.health,
          pool_status: poolEntry.status,
          cf_403_rate: cf403Rate,
          cf_403_count: cf403Count,
          cf_check_count: cfCheckCount,
          cf_last_code: poolEntry.cf_last_code || 0,
          ip_risk: poolEntry.ip_risk || null,
          ip_risk_label: poolEntry.ip_risk_label || null,
        });
      } else {
        instances.push({
          container: inst.name,
          instance_index: inst.index,
          host_port: inst.port,
          ip: inst.ip,
          endpoint: inst.endpoint,
          health: inst.running ? 'unknown' : 'stopped',
          pool_status: inst.running ? 'not-in-pool' : 'stopped',
        });
      }
    }

    success(res, instances);
  } catch (err) {
    error(res, 'INSTANCE_ERROR', err.message, 500);
  }
});

// ==================== API: 统计信息 ====================
app.get('/api/stats', async (req, res) => {
  try {
    const instances_list = await listWarpInstances();
    const running = instances_list.filter(i => i.running);
    const totalInstances = pool.size;
    const available = Array.from(pool.values()).filter(e => e.status === 'available' && e.health === 'healthy').length;
    const inUse = Array.from(pool.values()).filter(e => e.status === 'in_use').length;
    const unhealthy = Array.from(pool.values()).filter(e => e.health === 'unhealthy').length;
    const uniqueIPs = new Set(Array.from(pool.values()).map(e => e.ip).filter(Boolean));

    success(res, {
      total_instances_dirs: instances_list.length,
      running_processes: running.length,
      stopped_processes: instances_list.filter(i => !i.running).length,
      total_instances: totalInstances,
      available: available,
      in_use: inUse,
      unhealthy: unhealthy,
      unique_ips: uniqueIPs.size,
      active_sessions: sessions.size,
      resident_count: config.resident_count,
    });
  } catch (err) {
    error(res, 'INSTANCE_ERROR', err.message, 500);
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
  const { names = [], deleteAll = false } = req.body;
  let toDelete = names;

  if (deleteAll) {
    try {
      const instances_list = await listWarpInstances();
      toDelete = instances_list.map(i => i.name);
    } catch (e) {
      return error(res, 'INSTANCE_ERROR', e.message, 500);
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
  const { name, names = [] } = req.body;
  const targetNames = name ? [name] : names;

  if (targetNames.length === 0) {
    return error(res, 'INVALID_INPUT', '缺少容器名称');
  }

  for (const n of targetNames) {
    if (!validateContainerName(n)) {
      return error(res, 'INVALID_INPUT', `无效的实例名称: ${n}`);
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
    return error(res, 'INVALID_INPUT', '无效的实例名称');
  }
  try {
    const m = name.match(/warp-(\d+)/);
    if (!m) return error(res, 'INVALID_INPUT', '无法解析实例索引');
    const idx = parseInt(m[1]);
    const dir = getInstanceDir(idx);
    const metaPath = path.join(dir, 'meta.json');
    if (!fs.existsSync(metaPath)) {
      return error(res, 'NOT_FOUND', '实例配置不存在');
    }
    const meta = JSON.parse(fs.readFileSync(metaPath, 'utf8'));
    const configPath = path.join(dir, 'singbox.json');
    if (!fs.existsSync(configPath)) {
      return error(res, 'NOT_FOUND', 'sing-box配置不存在');
    }
    const pid = startSingbox(idx, configPath);
    meta.pid = pid;
    fs.writeFileSync(metaPath, JSON.stringify(meta, null, 2));
    logger.info('INSTANCE', `[${name}] sing-box已启动 (PID:${pid})`);
    success(res, { name, pid });
  } catch (err) {
    error(res, 'INSTANCE_ERROR', err.message, 500);
  }
});

app.post('/api/stop', async (req, res) => {
  const { name } = req.body;
  if (!name || !validateContainerName(name)) {
    return error(res, 'INVALID_INPUT', '无效的实例名称');
  }
  try {
    const m = name.match(/warp-(\d+)/);
    if (!m) return error(res, 'INVALID_INPUT', '无法解析实例索引');
    const idx = parseInt(m[1]);
    const dir = getInstanceDir(idx);
    const metaPath = path.join(dir, 'meta.json');
    if (fs.existsSync(metaPath)) {
      const meta = JSON.parse(fs.readFileSync(metaPath, 'utf8'));
      if (meta.pid) stopSingbox(meta.pid);
      meta.pid = null;
      fs.writeFileSync(metaPath, JSON.stringify(meta, null, 2));
    }
    // 将该实例从池中标记为unavailable
    const poolKey = `${idx}`;
    const entry = pool.get(poolKey);
    if (entry) {
      entry.status = 'available';
      entry.health = 'unknown';
    }
    logger.info('INSTANCE', `[${name}] 已停止`);
    success(res, { name });
  } catch (err) {
    error(res, 'INSTANCE_ERROR', err.message, 500);
  }
});

// ==================== API: 任务状态 ====================
app.get('/api/job/:id', (req, res) => {
  const id = parseInt(req.params.id);
  const job = jobs[id];
  if (!job) {
    return error(res, 'INVALID_INPUT', '任务不存在', 404);
  }
  success(res, job);
});

// ==================== API: 配置管理 ====================
app.get('/api/config', (req, res) => {
  // 不返回 api_key / team_token 完整值
  const safeConfig = { ...config };
  safeConfig.api_key = config.api_key.substring(0, 8) + '****';
  safeConfig.team_token = config.team_token ? config.team_token.substring(0, 6) + '****' : '';
  safeConfig.public_ip = publicIP;
  success(res, safeConfig);
});

app.post('/api/config', apiKeyAuth, async (req, res) => {
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
      'team_token',
    ];

    const oldResidentCount = config.resident_count;

    for (const key of allowedKeys) {
      if (updates.hasOwnProperty(key)) {
        config[key] = updates[key];
      }
    }

    saveConfig();
    logger.setLogLevel(config.log_level);

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
    safeConfig.api_key = config.api_key.substring(0, 8) + '****';
    safeConfig.team_token = config.team_token ? config.team_token.substring(0, 6) + '****' : '';
    safeConfig.public_ip = publicIP;
    success(res, safeConfig);
  } catch (e) {
    logger.error('CONFIG', '配置更新失败: ' + e.message);
    error(res, 'INTERNAL_ERROR', e.message, 500);
  }
});

// ==================== API: 代理池 - 获取代理 ====================
app.get('/api/pool/acquire', async (req, res) => {
  const release = await acquirePoolLock();
  try {
    const stickySessionId = typeof req.query.session_id === 'string' ? req.query.session_id.trim() : '';
    const useStickySession = stickySessionId.length > 0;
    const sessionId = useStickySession ? stickySessionId : null;
    const stickyMinutes = parseInt(req.query.sticky_minutes) || config.sticky_minutes;
    const now = Date.now();

    // 仅显式传入 session_id 时启用粘性会话
    if (useStickySession && sessions.has(sessionId)) {
      const session = sessions.get(sessionId);
      if (now < session.expires_at) {
        const entry = pool.get(session.pool_key);
        if (entry && entry.health === 'healthy') {
          // 延长会话
          session.expires_at = now + stickyMinutes * 60 * 1000;
          logger.info('POOL', `acquire 复用会话 ${sessionId.substring(0, 8)}... IP:${entry.ip} 端口:${entry.host_port}`);
          success(res, {
            session_id: sessionId,
            proxy: `socks5://${publicIP}:${entry.host_port}`,
            ip: entry.ip,
            expires_at: new Date(session.expires_at).toISOString(),
            sticky_minutes: stickyMinutes,
          });
          probeCfOnAcquire(session.pool_key, sessionId).catch(e => {
            logger.error('CF_USAGE', `复用会话后台探测异常: ${e.message}`);
          });
          return;
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

    // 查找可用且健康的实例（round-robin）
    const availableKeys = [];
    for (const [key, entry] of pool) {
      if (entry.status === 'available' && entry.health === 'healthy' && entry.ip) {
        availableKeys.push(key);
      }
    }

    if (availableKeys.length === 0) {
      logger.warn('POOL', 'acquire 无可用代理');
      return error(res, 'NO_AVAILABLE_PROXY', '无可用代理，请稍后重试', 503);
    }

    // round-robin 分配
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

    success(res, {
      session_id: sessionId,
      proxy: `socks5://${publicIP}:${entry.host_port}`,
      ip: entry.ip,
      expires_at: expiresAt ? new Date(expiresAt).toISOString() : null,
      sticky_minutes: useStickySession ? stickyMinutes : null,
      sticky: useStickySession,
    });
    probeCfOnAcquire(poolKey, sessionId || 'stateless').catch(e => {
      logger.error('CF_USAGE', `提取后后台探测异常: ${e.message}`);
    });
  } finally {
    release();
  }
});

// ==================== API: 代理池 - 释放代理 ====================
app.post('/api/pool/release', async (req, res) => {
  const { session_id, result } = req.body;
  if (!session_id) {
    return error(res, 'INVALID_INPUT', '缺少 session_id');
  }

  const session = sessions.get(session_id);
  if (!session) {
    return error(res, 'SESSION_NOT_FOUND', 'session_id 不存在或已过期', 404);
  }

  const entry = pool.get(session.pool_key);
  let releasedIP = null;
  if (entry) {
    entry.status = 'available';
    releasedIP = entry.ip;

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

        // 检查是否触发自动换IP
        if (config.auto_rotate_on_high_403
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
  const { session_id, result } = req.body;
  if (!session_id) {
    return error(res, 'INVALID_INPUT', '缺少 session_id');
  }
  if (result === undefined || result === null) {
    return error(res, 'INVALID_INPUT', '缺少 result (HTTP状态码)');
  }

  const session = sessions.get(session_id);
  if (!session) {
    return error(res, 'SESSION_NOT_FOUND', 'session_id 不存在或已过期', 404);
  }

  const entry = pool.get(session.pool_key);
  if (!entry) {
    return error(res, 'NOT_FOUND', '代理池条目不存在', 404);
  }

  const code = parseInt(result);
  if (isNaN(code)) {
    return error(res, 'INVALID_INPUT', 'result 必须是数字');
  }

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

  const cf403Rate = Math.round(entry.cf_403_count / entry.cf_check_count * 100);
  logger.info('POOL', `report 会话${session_id.substring(0, 8)}... IP:${entry.ip} HTTP:${code} → CF统计 ${entry.cf_403_count}/${entry.cf_check_count}=${cf403Rate}%`);

  let rotated = false;
  // 检查是否触发自动换IP
  if (config.auto_rotate_on_high_403
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
    available: entries.filter(e => e.status === 'available' && e.health === 'healthy').length,
    in_use: entries.filter(e => e.status === 'in_use').length,
    unhealthy: entries.filter(e => e.health === 'unhealthy').length,
    instances: entries,
    sessions: activeSessions,
  });
});

// ==================== API: 代理池 - 强制换IP ====================
app.post('/api/pool/rotate', async (req, res) => {
  const { session_id, pool_key } = req.body;

  if (pool_key) {
    // 通过 pool_key 换指定实例的IP
    const entry = pool.get(pool_key);
    if (!entry) {
      return error(res, 'SESSION_NOT_FOUND', 'pool_key 不存在: ' + pool_key, 404);
    }

    const oldIP = entry.ip;
    const job = createJob('rotate', 1);
    success(res, { jobId: job.id });

    (async () => {
      addLog(job, `[${entry.container}:${entry.instance_index}] 开始换IP (当前: ${oldIP})`, 'info');
      const ok = await rotateInstanceIP(pool_key);
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
      return error(res, 'SESSION_NOT_FOUND', 'session_id 不存在或已过期', 404);
    }

    const entry = pool.get(session.pool_key);
    if (!entry) {
      return error(res, 'SESSION_NOT_FOUND', '对应的代理池条目不存在', 404);
    }

    const oldIP = entry.ip;
    const job = createJob('rotate', 1);
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
    // 换所有实例的IP
    const keys = Array.from(pool.keys()).filter(k => {
      const e = pool.get(k);
      return e.status === 'available';
    });

    const job = createJob('rotate', keys.length);
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

// ==================== 定时器 ====================
function startTimers() {
  // 健康巡检定时器
  healthCheckTimer = setInterval(async () => {
    logger.info('HEALTH_CHECK', '开始定时健康巡检...');
    const keys = Array.from(pool.keys());
    for (const key of keys) {
      const entry = pool.get(key);
      if (entry.status === 'available' && entry.health !== 'unknown') {
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
  const instances_list = await listWarpInstances();
  const running = instances_list.filter(i => i.running);

  let recoveredInstances = 0;
  let availableIPs = 0;

  for (const inst of running) {
    const poolKey = `${inst.index}`;
    if (pool.has(poolKey)) continue;

    const ip = await checkExitIP(inst.port);
    pool.set(poolKey, {
      container: inst.name,
      instance_index: inst.index,
      host_port: inst.port,
      ip,
      health: ip ? 'healthy' : 'unhealthy',
      status: 'available',
    });
    recoveredInstances++;
    if (ip) availableIPs++;
  }

  logger.info('RECOVERY', `恢复 ${running.length} 个运行实例，${recoveredInstances} 个加入代理池，${availableIPs} 个可用IP`);

  await adjustResidentCount();
}

async function adjustResidentCount() {
  const instances_list = await listWarpInstances();
  const runningIndices = new Set();
  for (const inst of instances_list) {
    if (inst.running) runningIndices.add(inst.index);
  }

  // 创建缺失的实例
  for (let i = 0; i < config.resident_count; i++) {
    if (!runningIndices.has(i)) {
      logger.info('RECOVERY', `创建实例 warp-${i}`);
      try {
        await createWarpInstance(i);
      } catch (e) {
        logger.error('RECOVERY', `创建实例 warp-${i} 失败: ${e.message}`);
      }
    }
  }

  // 删除超出 resident_count 的多余实例
  for (const inst of instances_list) {
    if (inst.index >= config.resident_count) {
      try {
        await deleteWarpInstance(inst.name);
        logger.info('RECOVERY', `[${inst.name}] 多余实例已删除`);
      } catch (e) {
        logger.error('RECOVERY', `[${inst.name}] 删除失败: ${e.message}`);
      }
    }
  }
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
        if (inst.pid) {
          try {
            stopSingbox(inst.pid);
            logger.info('SERVER', `[${inst.name}] sing-box已停止 (PID:${inst.pid})`);
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
  console.log(`  公网IP: ${publicIP}`);
  console.log(`  API Key: ${config.api_key}`);
  console.log(`  实例数: ${config.resident_count} (最大 ${config.max_instances})`);
  console.log(`  基础端口: ${config.base_port}`);
  console.log(`  模式: sing-box + wgcf`);
  console.log(`  绑定地址: ${config.bind_address}`);
  console.log('========================================');
  console.log('');

  logger.info('SERVER', `服务启动 端口:${config.port} 绑定:${config.bind_address}`);
  logger.info('SERVER', `API Key: ${config.api_key}`);

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
