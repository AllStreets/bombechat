// A small static server for the mockups. The pages load JavaScript modules,
// which browsers won't do from file://, so open them through this instead:
//   npm run serve   then   http://localhost:4810/mockups/bombechat.html
const http = require('http'), fs = require('fs'), path = require('path');
const ROOT = path.resolve(__dirname, '..');
const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.cjs': 'text/javascript', '.css': 'text/css', '.png': 'image/png', '.json': 'application/json', '.svg': 'image/svg+xml' };
function start(port = 4810) {
  const server = http.createServer((req, res) => {
    const u = decodeURIComponent(new URL(req.url, 'http://x').pathname);
    if (u === '/favicon.ico') { res.writeHead(204); return res.end(); }
    const file = path.join(ROOT, u === '/' ? 'mockups/bombechat.html' : u);
    if (!file.startsWith(ROOT)) { res.writeHead(403); return res.end(); }
    fs.readFile(file, (err, buf) => {
      if (err) { res.writeHead(404); return res.end('not found'); }
      res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] || 'application/octet-stream', 'Cache-Control': 'no-store' });
      res.end(buf);
    });
  });
  return new Promise(r => server.listen(port, '127.0.0.1', () => r(server)));
}
module.exports = { start };
if (require.main === module) start(+process.env.PORT || 4810).then(() => console.log('http://localhost:' + (process.env.PORT || 4810) + '/mockups/bombechat.html'));
