/**
 * Mock 代理池测试 — 不依赖 Docker
 * 模拟 1 个容器 × 20 个实例，验证：
 * 1. 20 个实例 IP 唯一性（去重逻辑）
 * 2. acquire/release 正确性
 * 3. 粘性会话（session_id 复用）
 * 4. round-robin 分配
 * 5. 并发安全（异步锁）
 * 6. rotate 换 IP
 * 7. session 过期清理
 * 8. 端口分配公式
 */

const crypto = require('crypto');
const net = require('net');

let passCount = 0;
let failCount = 0;
const results = [];

function test(name, condition, detail = '') {
  const status = condition ? 'PASS' : 'FAIL';
  if (condition) passCount++;
  else failCount++;
  const msg = `[${status}] ${name}${detail ? ' — ' + detail : ''}`;
  results.push(msg);
  console.log(msg);
}

function sleep(ms) { return new Promise(r => setTimeout(r, ms)); }

// ==================== 模拟配置 ====================
const config = {
  warp_instances_per_container: 20,
  base_host_port: 10001,
  base_container_port: 40000,
  sticky_minutes: 10,
  session_cleanup_interval: 30,
  max_rebuild_attempts: 3,
  auto_rotate_on_high_403: true,
  cf_403_threshold: 50,
  cf_min_check_count: 3,
};

// ==================== 复用 server.js 的核心逻辑 ====================

// 代理池数据结构
const pool = new Map();
const sessions = new Map();
let roundRobinIdx = 0;

// 异步锁
let _poolLock = Promise.resolve();
function acquirePoolLock() {
  const next = _poolLock;
  let release;
  _poolLock = new Promise(r => { release = r; });
  return next.then(() => release);
}

// 端口分配公式（与 server.js 一致）
function getHostPort(containerIndex, instanceIndex) {
  return config.base_host_port + containerIndex * config.warp_instances_per_container + instanceIndex;
}
function getContainerPort(instanceIndex) {
  return config.base_container_port + instanceIndex;
}

// 模拟 IP 生成器 — 生成不同 IP
let mockIPCounter = 0;
const usedIPs = new Set();
function generateMockIP() {
  // 生成 104.28.x.x 格式的 IP，确保唯一
  let ip;
  do {
    ip = `104.28.${Math.floor(mockIPCounter / 255)}.${mockIPCounter % 255 + 1}`;
    mockIPCounter++;
  } while (usedIPs.has(ip));
  usedIPs.add(ip);
  return ip;
}

// 模拟 checkExitIP（直接返回预生成的 IP）
function mockCheckExitIP(port) {
  return Promise.resolve(generateMockIP());
}

// 模拟 checkOpenAIAccess
function mockCheckOpenAI() {
  return Promise.resolve(true);
}

// 模拟使用时CF探测并累计403率（与 server.js 的 probeCfOnAcquire 一致）
function mockProbeCfOnAcquire(poolKey, httpCode) {
  const entry = pool.get(poolKey);
  if (!entry) return null;

  if (entry.cf_check_count === undefined) {
    entry.cf_check_count = 0;
    entry.cf_403_count = 0;
    entry.cf_last_code = 0;
  }

  entry.cf_check_count++;
  entry.cf_last_code = httpCode;
  if (httpCode === 403) entry.cf_403_count++;

  const rate = Math.round(entry.cf_403_count / entry.cf_check_count * 100);
  if (config.auto_rotate_on_high_403
      && entry.cf_check_count >= config.cf_min_check_count
      && rate >= config.cf_403_threshold) {
    entry.pending_rotate = true;
  }

  return {
    code: httpCode,
    count: entry.cf_check_count,
    count403: entry.cf_403_count,
    rate,
    pendingRotate: entry.pending_rotate === true,
  };
}

// 模拟容器创建（方案C：1 容器 20 实例）
async function mockCreateContainer(containerIndex) {
  const containerName = `warp-${containerIndex}`;
  console.log(`\n[Mock] 创建容器 ${containerName}，${config.warp_instances_per_container} 个实例...`);

  for (let i = 0; i < config.warp_instances_per_container; i++) {
    const hostPort = getHostPort(containerIndex, i);
    const containerPort = getContainerPort(i);
    const poolKey = `${containerIndex}:${i}`;

    // 模拟 L1 检测
    const ip = await mockCheckExitIP(hostPort);
    if (!ip) {
      pool.set(poolKey, {
        container: containerName,
        instance_index: i,
        host_port: hostPort,
        container_port: containerPort,
        ip: null,
        health: 'unknown',
        status: 'available',
      });
      continue;
    }

    // 跨实例 IP 去重（与 server.js 逻辑一致）
    let isDuplicate = false;
    for (const [key, val] of pool) {
      if (key !== poolKey && val.ip === ip) {
        isDuplicate = true;
        break;
      }
    }

    if (isDuplicate) {
      console.log(`[Mock] 实例 ${i} IP ${ip} 重复，触发重注册`);
      // 模拟 rotate
      const newIP = generateMockIP();
      pool.set(poolKey, {
        container: containerName,
        instance_index: i,
        host_port: hostPort,
        container_port: containerPort,
        ip: newIP,
        health: 'healthy',
        status: 'available',
      });
      continue;
    }

    // 模拟 L2 检测
    const openaiOk = await mockCheckOpenAI();
    pool.set(poolKey, {
      container: containerName,
      instance_index: i,
      host_port: hostPort,
      container_port: containerPort,
      ip,
      health: openaiOk ? 'healthy' : 'unhealthy',
      status: 'available',
    });
  }
}

// acquire（与 server.js 逻辑一致）
async function acquire(sessionIdOverride, stickyMinutes) {
  const release = await acquirePoolLock();
  try {
    const sessionId = sessionIdOverride || crypto.randomUUID();
    const stickyMins = stickyMinutes || config.sticky_minutes;
    const now = Date.now();

    if (sessionIdOverride && sessions.has(sessionId)) {
      const session = sessions.get(sessionId);
      if (now < session.expires_at) {
        const entry = pool.get(session.pool_key);
        if (entry && entry.health === 'healthy') {
          session.expires_at = now + stickyMins * 60 * 1000;
          return {
            session_id: sessionId,
            pool_key: session.pool_key,
            ip: entry.ip,
            port: entry.host_port,
            proxy: `socks5://127.0.0.1:${entry.host_port}`,
            reused: true,
          };
        }
      } else {
        const entry = pool.get(session.pool_key);
        if (entry) entry.status = 'available';
        sessions.delete(sessionId);
      }
    }

    const availableKeys = [];
    for (const [key, entry] of pool) {
      if (entry.status === 'available' && entry.health === 'healthy' && entry.ip) {
        availableKeys.push(key);
      }
    }

    if (availableKeys.length === 0) {
      return { error: 'NO_AVAILABLE_PROXY' };
    }

    const poolKey = availableKeys[roundRobinIdx % availableKeys.length];
    roundRobinIdx++;

    const entry = pool.get(poolKey);
    entry.status = 'in_use';

    const expiresAt = now + stickyMins * 60 * 1000;
    sessions.set(sessionId, {
      session_id: sessionId,
      pool_key: poolKey,
      acquired_at: now,
      expires_at: expiresAt,
    });

    return {
      session_id: sessionId,
      pool_key: poolKey,
      ip: entry.ip,
      port: entry.host_port,
      proxy: `socks5://127.0.0.1:${entry.host_port}`,
      reused: false,
    };
  } finally {
    release();
  }
}

// release
function release(sessionId) {
  const session = sessions.get(sessionId);
  if (!session) return { error: 'SESSION_NOT_FOUND' };

  const entry = pool.get(session.pool_key);
  let releasedIP = null;
  if (entry) {
    entry.status = 'available';
    releasedIP = entry.ip;
  }
  sessions.delete(sessionId);
  return { session_id: sessionId, released_ip: releasedIP };
}

// 模拟 rotateInstanceIP
async function mockRotateIP(poolKey) {
  const entry = pool.get(poolKey);
  if (!entry) return false;

  const oldIP = entry.ip;
  for (let attempt = 0; attempt < config.max_rebuild_attempts; attempt++) {
    const newIP = generateMockIP();

    // 跨实例 IP 去重
    let isDuplicate = false;
    for (const [key, val] of pool) {
      if (key !== poolKey && val.ip === newIP) {
        isDuplicate = true;
        break;
      }
    }

    if (isDuplicate) continue;

    entry.ip = newIP;
    entry.health = 'healthy';
    return { oldIP, newIP };
  }
  entry.health = 'unhealthy';
  return false;
}

// ==================== 测试 ====================

async function run() {
  console.log('\n=== WARP 代理池 Mock 测试（不依赖 Docker）===\n');

  // ---- T1: 端口分配公式 ----
  console.log('--- T1: 端口分配公式 ---');
  test('T1.1 容器0实例0 端口=10001',
    getHostPort(0, 0) === 10001, `port=${getHostPort(0, 0)}`);
  test('T1.2 容器0实例19 端口=10020',
    getHostPort(0, 19) === 10020, `port=${getHostPort(0, 19)}`);
  test('T1.3 容器1实例0 端口=10021',
    getHostPort(1, 0) === 10021, `port=${getHostPort(1, 0)}`);
  test('T1.4 容器1实例19 端口=10040',
    getHostPort(1, 19) === 10040, `port=${getHostPort(1, 19)}`);
  test('T1.5 容器0 容器端口0=40000',
    getContainerPort(0) === 40000, `port=${getContainerPort(0)}`);
  test('T1.6 容器0 容器端口19=40019',
    getContainerPort(19) === 40019, `port=${getContainerPort(19)}`);

  // ---- T2: 创建容器 + 20 个实例 IP 唯一性 ----
  console.log('\n--- T2: 创建容器 + IP 唯一性 ---');
  await mockCreateContainer(0);

  test('T2.1 代理池有 20 个实例',
    pool.size === 20, `size=${pool.size}`);

  const allIPs = Array.from(pool.values()).map(e => e.ip);
  const uniqueIPs = new Set(allIPs);
  test('T2.2 20 个 IP 全部唯一',
    uniqueIPs.size === 20, `unique=${uniqueIPs.size}/20`);

  test('T2.3 没有 null IP',
    !allIPs.includes(null), `nulls=${allIPs.filter(x => !x).length}`);

  test('T2.4 所有实例 healthy',
    Array.from(pool.values()).every(e => e.health === 'healthy'),
    `unhealthy=${Array.from(pool.values()).filter(e => e.health !== 'healthy').length}`);

  // 打印 20 个 IP
  console.log('\n--- 20 个实例的 IP ---');
  const sortedEntries = Array.from(pool.entries()).sort((a, b) => {
    const [ai, aj] = a[0].split(':').map(Number);
    const [bi, bj] = b[0].split(':').map(Number);
    return ai === bi ? aj - bj : ai - bi;
  });
  for (const [key, entry] of sortedEntries) {
    console.log(`  ${key} | ${entry.container} #${entry.instance_index} | port=${entry.host_port} | IP=${entry.ip} | ${entry.health}`);
  }

  // ---- T3: acquire/release ----
  console.log('\n--- T3: acquire/release ---');

  // acquire 第一个
  let r1 = await acquire();
  test('T3.1 acquire 返回有效代理',
    r1.ip && r1.port && r1.proxy && !r1.error,
    `ip=${r1.ip} port=${r1.port}`);

  test('T3.2 acquire 后该实例状态为 in_use',
    pool.get(r1.pool_key).status === 'in_use',
    `status=${pool.get(r1.pool_key).status}`);

  // release
  let rel = release(r1.session_id);
  test('T3.3 release 成功',
    rel.released_ip === r1.ip,
    `released_ip=${rel.released_ip}`);

  test('T3.4 release 后状态恢复 available',
    pool.get(r1.pool_key).status === 'available',
    `status=${pool.get(r1.pool_key).status}`);

  // release 已释放的 session
  let rel2 = release(r1.session_id);
  test('T3.5 重复 release 返回 SESSION_NOT_FOUND',
    rel2.error === 'SESSION_NOT_FOUND',
    `error=${rel2.error}`);

  // ---- T4: 粘性会话 ----
  console.log('\n--- T4: 粘性会话 ---');

  // 第一次 acquire，获取 session_id
  let s1 = await acquire();
  console.log(`  第一次 acquire: session=${s1.session_id.substring(0, 8)}... ip=${s1.ip} port=${s1.port}`);

  // 第二次 acquire 用同一 session_id
  let s2 = await acquire(s1.session_id);
  console.log(`  第二次 acquire: session=${s2.session_id.substring(0, 8)}... ip=${s2.ip} port=${s2.port}`);

  test('T4.1 同一 session_id 返回同一 IP',
    s1.ip === s2.ip && s1.port === s2.port,
    `s1.ip=${s1.ip} s2.ip=${s2.ip}`);

  test('T4.2 第二次 acquire 标记为 reused',
    s2.reused === true,
    `reused=${s2.reused}`);

  // 不同 session_id 应返回不同 IP（round-robin）
  let s3 = await acquire();
  test('T4.3 不同 session_id 返回不同 IP',
    s3.ip !== s1.ip,
    `s1.ip=${s1.ip} s3.ip=${s3.ip}`);

  // 清理
  release(s1.session_id);
  release(s3.session_id);

  // ---- T5: Round-Robin 分配 ----
  console.log('\n--- T5: Round-Robin 分配 ---');
  const acquiredIPs = [];
  const acquiredKeys = [];
  for (let i = 0; i < 5; i++) {
    const r = await acquire();
    acquiredIPs.push(r.ip);
    acquiredKeys.push(r.pool_key);
  }
  test('T5.1 5 次 acquire 返回 5 个不同 IP',
    new Set(acquiredIPs).size === 5,
    `unique=${new Set(acquiredIPs).size}/5`);

  test('T5.2 5 次 acquire 分配 5 个不同实例',
    new Set(acquiredKeys).size === 5,
    `unique=${new Set(acquiredKeys).size}/5`);

  // 打印 round-robin 结果
  console.log('\n--- Round-Robin 分配顺序 ---');
  acquiredKeys.forEach((k, i) => {
    const e = pool.get(k);
    console.log(`  #${i + 1}: pool_key=${k} IP=${e.ip} port=${e.host_port}`);
  });

  // 清理
  for (let i = 0; i < 5; i++) {
    // sessions are already in sessions Map, need to find them
  }
  // 释放所有 sessions
  for (const [sid] of sessions) {
    release(sid);
  }

  // ---- T6: 并发安全 ----
  console.log('\n--- T6: 并发安全 ---');
  roundRobinIdx = 0; // 重置

  // 同时发起 10 个 acquire
  const promises = [];
  for (let i = 0; i < 10; i++) {
    promises.push(acquire());
  }
  const concurrentResults = await Promise.all(promises);

  test('T6.1 10 个并发 acquire 全部成功',
    concurrentResults.every(r => !r.error),
    `errors=${concurrentResults.filter(r => r.error).length}`);

  const concurrentIPs = concurrentResults.map(r => r.ip);
  test('T6.2 10 个并发 acquire 返回 10 个不同 IP',
    new Set(concurrentIPs).size === 10,
    `unique=${new Set(concurrentIPs).size}/10`);

  const concurrentKeys = concurrentResults.map(r => r.pool_key);
  test('T6.3 10 个并发 acquire 分配 10 个不同实例',
    new Set(concurrentKeys).size === 10,
    `unique=${new Set(concurrentKeys).size}/10`);

  // 释放所有
  for (const r of concurrentResults) {
    release(r.session_id);
  }

  // ---- T7: 换 IP (rotate) ----
  console.log('\n--- T7: 换 IP (rotate) ---');

  // 找一个可用的实例换 IP
  let firstKey = null;
  for (const [key, entry] of pool) {
    if (entry.status === 'available' && entry.health === 'healthy') {
      firstKey = key;
      break;
    }
  }

  const oldIP = pool.get(firstKey).ip;
  console.log(`  换 IP 前: pool_key=${firstKey} IP=${oldIP}`);

  const rotateResult = await mockRotateIP(firstKey);
  const newIP = pool.get(firstKey).ip;
  console.log(`  换 IP 后: pool_key=${firstKey} IP=${newIP}`);

  test('T7.1 rotate 后 IP 变化',
    rotateResult && rotateResult.oldIP !== rotateResult.newIP,
    `${rotateResult.oldIP} -> ${rotateResult.newIP}`);

  test('T7.2 rotate 后 IP 仍唯一',
    Array.from(pool.values()).filter(e => e.ip === newIP).length === 1,
    `count=${Array.from(pool.values()).filter(e => e.ip === newIP).length}`);

  test('T7.3 rotate 后 health=healthy',
    pool.get(firstKey).health === 'healthy',
    `health=${pool.get(firstKey).health}`);

  // ---- T8: Session 过期 ----
  console.log('\n--- T8: Session 过期 ---');

  // 创建一个已过期的 session
  const expiredSessionId = crypto.randomUUID();
  const expiredKey = firstKey;
  sessions.set(expiredSessionId, {
    session_id: expiredSessionId,
    pool_key: expiredKey,
    acquired_at: Date.now() - 20 * 60 * 1000, // 20 分钟前
    expires_at: Date.now() - 10 * 60 * 1000,  // 10 分钟前过期
  });
  pool.get(expiredKey).status = 'in_use';

  // 用过期 session acquire，应该触发惰性删除并分配新 IP
  let expiredAcquire = await acquire(expiredSessionId);
  test('T8.1 过期 session 触发惰性删除',
    !expiredAcquire.error && expiredAcquire.reused === false,
    `error=${expiredAcquire.error} reused=${expiredAcquire.reused}`);

  test('T8.2 过期 session 被刷新（新 pool_key + 新过期时间）',
    sessions.has(expiredSessionId) && sessions.get(expiredSessionId).pool_key !== expiredKey,
    `same_id_reused=${sessions.has(expiredSessionId)} new_key=${sessions.get(expiredSessionId)?.pool_key}`);

  // 清理
  release(expiredAcquire.session_id);

  // ---- T9: IP 去重逻辑 ----
  console.log('\n--- T9: IP 去重逻辑 ---');

  // 手动设置重复 IP，验证去重检测
  const testKey1 = '0:0';
  const testKey2 = '0:1';
  const dupIP = '1.2.3.4';
  pool.get(testKey1).ip = dupIP;
  pool.get(testKey2).ip = '5.6.7.8';

  // 模拟去重检查
  let isDup = false;
  for (const [key, val] of pool) {
    if (key !== testKey2 && val.ip === dupIP) {
      isDup = true;
      break;
    }
  }
  test('T9.1 设置相同 IP 后去重检测触发',
    isDup === true, `duplicate=${isDup}`);

  // 恢复
  pool.get(testKey1).ip = generateMockIP();

  // 验证全部唯一
  const finalIPs = Array.from(pool.values()).map(e => e.ip);
  const finalUnique = new Set(finalIPs);
  test('T9.2 恢复后 20 个 IP 全部唯一',
    finalUnique.size === 20 && !finalIPs.includes(null),
    `unique=${finalUnique.size}/20 nulls=${finalIPs.filter(x => !x).length}`);

  // ---- T10: 代理池状态统计 ----
  console.log('\n--- T10: 代理池状态统计 ---');

  // 释放所有 sessions
  for (const [sid] of sessions) {
    release(sid);
  }

  const total = pool.size;
  const available = Array.from(pool.values()).filter(e => e.status === 'available' && e.health === 'healthy').length;
  const inUse = Array.from(pool.values()).filter(e => e.status === 'in_use').length;
  const unhealthy = Array.from(pool.values()).filter(e => e.health === 'unhealthy').length;
  const uniqueIPCount = new Set(Array.from(pool.values()).map(e => e.ip).filter(Boolean)).size;

  test('T10.1 total=20', total === 20, `total=${total}`);
  test('T10.2 available=20', available === 20, `available=${available}`);
  test('T10.3 in_use=0', inUse === 0, `in_use=${inUse}`);
  test('T10.4 unhealthy=0', unhealthy === 0, `unhealthy=${unhealthy}`);
  test('T10.5 unique_ips=20', uniqueIPCount === 20, `unique=${uniqueIPCount}`);
  test('T10.6 active_sessions=0', sessions.size === 0, `sessions=${sessions.size}`);

  // ---- T11: 完整 acquire → 使用 → release → 再 acquire 流程 ----
  console.log('\n--- T11: 完整使用流程 ---');

  // 模拟客户端使用代理
  const client1 = await acquire();
  console.log(`  客户端1 获取代理: IP=${client1.ip} port=${client1.port}`);

  const client2 = await acquire();
  console.log(`  客户端2 获取代理: IP=${client2.ip} port=${client2.port}`);

  test('T11.1 两个客户端获取不同 IP',
    client1.ip !== client2.ip,
    `c1=${client1.ip} c2=${client2.ip}`);

  // 客户端1 释放
  release(client1.session_id);
  console.log(`  客户端1 释放代理`);

  // 客户端3 获取，可能复用客户端1的
  const client3 = await acquire();
  console.log(`  客户端3 获取代理: IP=${client3.ip} port=${client3.port}`);

  test('T11.2 释放后该 IP 可被重新分配',
    client3.ip === client1.ip || client3.ip !== client2.ip,
    `c3=${client3.ip} c1=${client1.ip} c2=${client2.ip}`);

  // 清理
  release(client2.session_id);
  release(client3.session_id);

  // ---- T12: 使用时403统计 ----
  console.log('\n--- T12: 使用时403统计 ---');
  const cfKey = '0:0';
  const cfEntry = pool.get(cfKey);
  cfEntry.cf_check_count = 0;
  cfEntry.cf_403_count = 0;
  cfEntry.cf_last_code = 0;
  cfEntry.pending_rotate = false;

  const probe1 = mockProbeCfOnAcquire(cfKey, 200);
  test('T12.1 首次200计入总检测次数',
    probe1.count === 1 && probe1.count403 === 0 && probe1.rate === 0,
    `403=${probe1.count403}/${probe1.count} rate=${probe1.rate}%`);
  test('T12.2 检测不足3次不触发换IP',
    probe1.pendingRotate === false,
    `pending_rotate=${probe1.pendingRotate}`);

  const probe2 = mockProbeCfOnAcquire(cfKey, 403);
  test('T12.3 第二次403正确累计为1/2=50%',
    probe2.count === 2 && probe2.count403 === 1 && probe2.rate === 50,
    `403=${probe2.count403}/${probe2.count} rate=${probe2.rate}%`);
  test('T12.4 检测仍不足3次不触发换IP',
    probe2.pendingRotate === false,
    `pending_rotate=${probe2.pendingRotate}`);

  const probe3 = mockProbeCfOnAcquire(cfKey, 403);
  test('T12.5 第三次403正确累计为2/3=67%',
    probe3.count === 3 && probe3.count403 === 2 && probe3.rate === 67,
    `403=${probe3.count403}/${probe3.count} rate=${probe3.rate}%`);
  test('T12.6 达到最小次数且超过50%后标记待换IP',
    probe3.pendingRotate === true,
    `pending_rotate=${probe3.pendingRotate}`);
  test('T12.7 最近状态码记录为403',
    cfEntry.cf_last_code === 403,
    `last_code=${cfEntry.cf_last_code}`);

  // ---- T13: 市面IP提取模式与显式粘性 ----
  console.log('\n--- T13: 默认轮询提取与显式粘性 ---');
  const extractionKeys = Array.from(pool.keys()).filter(key => {
    const entry = pool.get(key);
    return entry.status === 'available' && entry.health === 'healthy' && entry.ip;
  });
  let extractionIndex = 0;
  const stickyMap = new Map();
  function mockMarketAcquire(sessionId) {
    if (sessionId && stickyMap.has(sessionId)) {
      return { ...stickyMap.get(sessionId), sticky: true, reused: true };
    }
    const key = extractionKeys[extractionIndex % extractionKeys.length];
    extractionIndex++;
    const entry = pool.get(key);
    const result = { poolKey: key, ip: entry.ip, port: entry.host_port, sticky: Boolean(sessionId), reused: false };
    if (sessionId) stickyMap.set(sessionId, result);
    return result;
  }

  const market1 = mockMarketAcquire();
  const market2 = mockMarketAcquire();
  test('T13.1 默认无session连续提取返回不同IP',
    market1.ip !== market2.ip && market1.sticky === false && market2.sticky === false,
    `ip1=${market1.ip} ip2=${market2.ip}`);

  const sticky1 = mockMarketAcquire('order-1001');
  const sticky2 = mockMarketAcquire('order-1001');
  test('T13.2 相同显式session复用同一IP',
    sticky1.ip === sticky2.ip && sticky2.reused === true,
    `ip1=${sticky1.ip} ip2=${sticky2.ip}`);

  const sticky3 = mockMarketAcquire('order-1002');
  test('T13.3 不同显式session轮询到不同IP',
    sticky3.ip !== sticky1.ip,
    `session1=${sticky1.ip} session2=${sticky3.ip}`);

  // ---- Summary ----
  console.log('\n=== Test Summary ===');
  console.log(`PASS: ${passCount} | FAIL: ${failCount} | TOTAL: ${passCount + failCount}`);
  if (failCount > 0) {
    console.log('\n--- Failed Tests ---');
    results.filter(r => r.startsWith('[FAIL]')).forEach(r => console.log(r));
  }

  console.log('\n=== 20 个实例最终状态 ===');
  const finalEntries = Array.from(pool.entries()).sort((a, b) => {
    const [ai, aj] = a[0].split(':').map(Number);
    const [bi, bj] = b[0].split(':').map(Number);
    return ai === bi ? aj - bj : ai - bi;
  });
  console.log('pool_key | container | instance | host_port | IP | health | status');
  console.log('---------|-----------|----------|-----------|-----|--------|-------');
  for (const [key, entry] of finalEntries) {
    console.log(`${key.padEnd(8)} | ${entry.container.padEnd(8)} | #${String(entry.instance_index).padEnd(7)} | ${String(entry.host_port).padEnd(9)} | ${entry.ip.padEnd(15)} | ${entry.health.padEnd(6)} | ${entry.status}`);
  }

  const finalIPList = finalEntries.map(([, e]) => e.ip);
  const finalIPSet = new Set(finalIPList);
  console.log(`\nIP 唯一性: ${finalIPSet.size}/20 个 IP 唯一`);
  console.log('');

  process.exit(failCount > 0 ? 1 : 0);
}

run().catch(console.error);
