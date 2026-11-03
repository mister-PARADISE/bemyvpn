// node shoot.js OUT.png 'query-string' [port]
const { chromium } = require('playwright');
(async () => {
  const [out, qs, port] = [process.argv[2], process.argv[3] || '', process.argv[4] || '8765'];
  const b = await chromium.launch({ args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'] });
  const p = await b.newPage({ viewport: { width: 1280, height: 720 } });
  p.on('console', (m) => console.log('console:', m.text()));
  p.on('pageerror', (e) => console.log('pageerror:', e.message));
  await p.goto(`http://127.0.0.1:${port}/tools/preview/preview.html?${qs}`);
  await p.waitForFunction(() => window.__done, null, { timeout: 240000 });
  console.log(await p.evaluate(() => window.__done));
  await p.screenshot({ path: out });
  await b.close();
})();
