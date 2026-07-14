/**
 * 文件职责：验证临时收集固定入口、只读时间流、日期分组和响应式边界。
 * 主要内容：使用隔离 Edge DevTools 会话驱动正式单文件前端，并保存一张临时视觉证据。
 * 重要约束：浏览器配置和截图只写入项目 `.local`，不读取个人配置或真实命令仓库。
 */

import fs from "node:fs/promises";
import path from "node:path";
import { spawn } from "node:child_process";
import { pathToFileURL } from "node:url";

/** 项目根目录，由脚本位置稳定反推。 */
const repositoryRoot = path.resolve(import.meta.dirname, "..");
/** 正式前端文件地址；浏览器预览分支提供不落盘的临时收集样例。 */
const frontendUrl = pathToFileURL(path.join(repositoryRoot, "frontend", "index.html")).href;
/** 本轮测试隔离目录，避免接触用户浏览器配置。 */
const evidenceDirectory = path.join(repositoryRoot, ".local", "inbox-regression");
/** 每次运行使用唯一配置目录，防止并发回归互相占用锁文件。 */
const browserProfile = path.join(evidenceDirectory, `profile-${process.pid}-${Date.now()}`);
/** 高位随机端口降低与本机调试会话冲突的概率。 */
const debuggingPort = 9500 + Math.floor(Math.random() * 300);

/** 短暂等待异步页面状态，不使用阻塞休眠。 */
function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

/** 查找 Windows 自带 Edge，作为当前环境没有浏览器连接器时的隔离回归载体。 */
async function findEdge() {
  const candidates = [
    path.join(process.env["ProgramFiles(x86)"] || "", "Microsoft", "Edge", "Application", "msedge.exe"),
    path.join(process.env.ProgramFiles || "", "Microsoft", "Edge", "Application", "msedge.exe"),
  ];
  for (const candidate of candidates) {
    try {
      await fs.access(candidate);
      return candidate;
    } catch {
      // 当前标准位置不存在时继续检查另一个安装目录。
    }
  }
  throw new Error("未找到 Microsoft Edge，无法执行临时收集前端回归");
}

/** 轮询 DevTools 页面列表，直到隔离浏览器完成启动。 */
async function waitForPageTarget() {
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`http://127.0.0.1:${debuggingPort}/json/list`);
      const targets = await response.json();
      const page = targets.find((target) => target.type === "page" && target.webSocketDebuggerUrl);
      if (page) return page;
    } catch {
      // 调试端口尚未监听时继续短暂轮询。
    }
    await delay(100);
  }
  throw new Error("15 秒内未发现临时收集回归页面");
}

/** 建立最小 CDP 客户端，并记录页面脚本异常作为验收门禁。 */
async function connectCdp(webSocketUrl) {
  const socket = new WebSocket(webSocketUrl);
  await new Promise((resolve, reject) => {
    socket.addEventListener("open", resolve, { once: true });
    socket.addEventListener("error", reject, { once: true });
  });
  let nextRequestId = 1;
  const pendingRequests = new Map();
  const exceptions = [];
  socket.addEventListener("message", (event) => {
    const message = JSON.parse(String(event.data));
    if (message.method === "Runtime.exceptionThrown") exceptions.push(message.params);
    const pending = pendingRequests.get(message.id);
    if (!pending) return;
    pendingRequests.delete(message.id);
    if (message.error) pending.reject(new Error(message.error.message));
    else pending.resolve(message.result);
  });
  return {
    send(method, params = {}) {
      const id = nextRequestId++;
      return new Promise((resolve, reject) => {
        pendingRequests.set(id, { resolve, reject });
        socket.send(JSON.stringify({ id, method, params }));
      });
    },
    getExceptions() {
      return [...exceptions];
    },
    close() {
      socket.close();
    },
  };
}

/** 在页面主世界执行表达式，并把浏览器异常转换为明确测试失败。 */
async function evaluate(send, expression) {
  const result = await send("Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
  });
  if (result.exceptionDetails) {
    throw new Error(result.exceptionDetails.exception?.description || "页面表达式执行失败");
  }
  return result.result?.value;
}

/** 等待页面条件成立；超时信息包含业务场景。 */
async function waitForCondition(send, expression, description) {
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    if (await evaluate(send, expression)) return;
    await delay(100);
  }
  throw new Error(`等待超时：${description}`);
}

/** 切换视口并返回横向溢出证据。 */
async function inspectViewport(send, width, height) {
  await send("Emulation.setDeviceMetricsOverride", {
    width,
    height,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await delay(120);
  return evaluate(send, `({
    width: ${width},
    height: ${height},
    documentWidth: document.documentElement.scrollWidth,
    clientWidth: document.documentElement.clientWidth,
    hasHorizontalOverflow: document.documentElement.scrollWidth > document.documentElement.clientWidth
  })`);
}

let browserProcess;
let cdp;
try {
  await fs.mkdir(evidenceDirectory, { recursive: true });
  const edge = await findEdge();
  browserProcess = spawn(edge, [
    "--headless=new",
    "--disable-gpu",
    "--no-first-run",
    "--no-default-browser-check",
    `--remote-debugging-port=${debuggingPort}`,
    `--user-data-dir=${browserProfile}`,
    "--window-size=1440,1024",
    frontendUrl,
  ], { stdio: "ignore", windowsHide: true });

  const target = await waitForPageTarget();
  cdp = await connectCdp(target.webSocketDebuggerUrl);
  await cdp.send("Runtime.enable");
  await cdp.send("Page.enable");
  await waitForCondition(cdp.send, "document.readyState === 'complete' && Boolean(document.querySelector('#inbox-nav-button'))", "正式前端完成加载");

  await evaluate(cdp.send, "document.querySelector('#inbox-nav-button').click()");
  await waitForCondition(cdp.send, "document.querySelector('#category-title')?.textContent === '临时收集'", "切换到临时收集页");

  const readOnlyEvidence = await evaluate(cdp.send, `(() => {
    const inboxButton = document.querySelector('#inbox-nav-button');
    const categoryNav = document.querySelector('.category-nav');
    const groups = [...document.querySelectorAll('[data-inbox-group]')];
    return {
      fixedEntryBeforeCategories: Boolean(inboxButton.compareDocumentPosition(categoryNav) & Node.DOCUMENT_POSITION_FOLLOWING),
      activeEntry: inboxButton.getAttribute('aria-current'),
      count: document.querySelector('#inbox-nav-count')?.textContent,
      title: document.querySelector('#category-title')?.textContent,
      description: document.querySelector('#category-description')?.textContent,
      itemIds: [...document.querySelectorAll('[data-inbox-id]')].map((item) => item.dataset.inboxId),
      groupLabels: groups.map((group) => group.querySelector('h2')?.textContent),
      firstContent: document.querySelector('[data-inbox-id="preview-inbox-1"] .inbox-content')?.textContent,
      linkHref: document.querySelector('[data-inbox-id="preview-inbox-1"] a')?.href,
      commandListHidden: getComputedStyle(document.querySelector('#command-list')).display === 'none',
      commandActionsHidden: ['ask-codex-button', 'copy-sort-button', 'add-command-button'].every((id) => getComputedStyle(document.getElementById(id)).display === 'none'),
      categoryCount: document.querySelectorAll('[data-category-id]').length
    };
  })()`);

  const compactViewport = await inspectViewport(cdp.send, 1024, 768);
  const largeViewport = await inspectViewport(cdp.send, 1440, 1024);
  const screenshot = await cdp.send("Page.captureScreenshot", { format: "png", captureBeyondViewport: false });
  const screenshotPath = path.join(evidenceDirectory, "临时收集页-1440x1024.png");
  await fs.writeFile(screenshotPath, Buffer.from(screenshot.data, "base64"));

  const navigationEvidence = await evaluate(cdp.send, `(() => {
    document.querySelector('[data-category-id="linux"]').click();
    const categoryTitle = document.querySelector('#category-title')?.textContent;
    const actionsVisible = ['ask-codex-button', 'copy-sort-button', 'add-command-button'].every((id) => getComputedStyle(document.getElementById(id)).display !== 'none');
    document.querySelector('#inbox-nav-button').click();
    return { categoryTitle, actionsVisible, returnedTitle: document.querySelector('#category-title')?.textContent };
  })()`);

  const failures = [];
  if (!readOnlyEvidence.fixedEntryBeforeCategories) failures.push("临时收集入口不在分类目录之前");
  if (readOnlyEvidence.activeEntry !== "page") failures.push("临时收集入口没有选中语义");
  if (readOnlyEvidence.itemIds.length !== 5) failures.push("时间流没有按样例完整渲染");
  if (!readOnlyEvidence.groupLabels.includes("今天") || !readOnlyEvidence.groupLabels.includes("昨天")) failures.push("今天或昨天分组缺失");
  if (!readOnlyEvidence.linkHref?.startsWith("https://github.com/")) failures.push("内容链接没有转换为安全链接");
  if (!readOnlyEvidence.commandListHidden || !readOnlyEvidence.commandActionsHidden) failures.push("分类页控件仍显示在临时收集页");
  if (compactViewport.hasHorizontalOverflow || largeViewport.hasHorizontalOverflow) failures.push("目标视口出现横向溢出");
  if (navigationEvidence.categoryTitle !== "Linux" || !navigationEvidence.actionsVisible || navigationEvidence.returnedTitle !== "临时收集") failures.push("分类与临时收集往返失败");
  if (cdp.getExceptions().length > 0) failures.push("页面运行期间出现 JavaScript 异常");
  if (failures.length > 0) throw new Error(failures.join("；"));

  console.log(JSON.stringify({
    status: "passed",
    readOnlyEvidence,
    navigationEvidence,
    viewports: [compactViewport, largeViewport],
    screenshotPath,
    runtimeExceptions: cdp.getExceptions().length,
  }));
} finally {
  cdp?.close();
  if (browserProcess && !browserProcess.killed) browserProcess.kill();
  await fs.rm(browserProfile, { recursive: true, force: true }).catch(() => {});
}
