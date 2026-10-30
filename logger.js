const fs = require('fs');
const path = require('path');

const LOG_LEVELS = { debug: 0, info: 1, warn: 2, error: 3 };
const MAX_LOG_FILES = 7;
const LOG_DIR = path.join(__dirname, 'logs');

if (!fs.existsSync(LOG_DIR)) {
  fs.mkdirSync(LOG_DIR, { recursive: true });
}

let currentLogLevel = LOG_LEVELS['info'];

function getDateStr() {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

function getTimestamp() {
  const d = new Date();
  return `${getDateStr()} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}:${String(d.getSeconds()).padStart(2, '0')}`;
}

function getLogFilePath() {
  return path.join(LOG_DIR, `app-${getDateStr()}.log`);
}

function cleanOldLogs() {
  try {
    const files = fs.readdirSync(LOG_DIR)
      .filter(f => f.startsWith('app-') && f.endsWith('.log'))
      .sort()
      .reverse();
    for (let i = MAX_LOG_FILES; i < files.length; i++) {
      fs.unlinkSync(path.join(LOG_DIR, files[i]));
    }
  } catch (e) {
    // 忽略清理失败
  }
}

function writeLog(level, module, message) {
  if (LOG_LEVELS[level] < currentLogLevel) return;

  const line = `[${getTimestamp()}] [${level.toUpperCase()}] [${module}] ${message}`;

  // 控制台输出
  const colors = {
    debug: '\x1b[36m',
    info: '\x1b[32m',
    warn: '\x1b[33m',
    error: '\x1b[31m',
  };
  const reset = '\x1b[0m';
  console.log(`${colors[level] || ''}${line}${reset}`);

  // 文件输出
  try {
    fs.appendFileSync(getLogFilePath(), line + '\n');
  } catch (e) {
    // 忽略文件写入失败
  }
}

function setLogLevel(level) {
  if (LOG_LEVELS.hasOwnProperty(level)) {
    currentLogLevel = LOG_LEVELS[level];
  }
}

cleanOldLogs();

module.exports = {
  debug: (module, message) => writeLog('debug', module, message),
  info: (module, message) => writeLog('info', module, message),
  warn: (module, message) => writeLog('warn', module, message),
  error: (module, message) => writeLog('error', module, message),
  setLogLevel,
};
