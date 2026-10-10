// A stand-in for the yard's NPort: the indicator's frames, 20 a second, as a
// truck rolls on, settles, sits, and leaves. Loops.
const net = require('net');
const port = Number(process.argv[2] || 9950);
const frame = (w, status = ' ') => `\x02${String(Math.round(w)).padStart(7, ' ')}LG${status}\r\n`;
const script = [];
for (let i = 0; i < 60; i++) script.push(frame(140));                         // empty, 3 s
for (let i = 0; i < 40; i++) script.push(frame(2000 + i * 1160, 'M'));         // rolling on, 2 s
for (let i = 0; i < 200; i++) script.push(frame(48320 + (i % 3) * 10));        // settled, 10 s
for (let i = 0; i < 40; i++) script.push(frame(48000 - i * 1200, 'M'));        // rolling off
net.createServer((socket) => {
  let i = 0;
  const t = setInterval(() => { socket.write(script[i++ % script.length]); }, 50);
  socket.on('close', () => clearInterval(t)); socket.on('error', () => {});
}).listen(port, '127.0.0.1', () => console.log(`indicator on ${port}`));
