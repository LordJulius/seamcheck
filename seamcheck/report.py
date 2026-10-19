"""A self-contained HTML map (no network): modules as nodes (sized by lines,
red when they have findings), calls between them as edges; click a module
for its functions, who calls them and what they call; the rules panel lists
every contract and what broke it."""
from __future__ import annotations

import html
import json
from collections import Counter

from .core import Finding, Project


def write_html(p: Project, findings: list[Finding], out_path: str) -> None:
    mods = sorted(p.modules.values(), key=lambda m: m.file)
    idx = {m.name: i for i, m in enumerate(mods)}
    medges: Counter = Counter()
    for caller, lst in p.edges.items():
        cm = p.funcs[caller].module
        for callee, _, exact in lst:
            tm = p.funcs[callee].module
            if tm != cm:
                medges[(idx[cm], idx[tm])] += 1
    fcount = Counter(f.file for f in findings)
    nodes = []
    for m in mods:
        funcs = []
        for f in m.funcs:
            if f.kind == "top":
                continue
            funcs.append({
                "n": f.name, "l": f.start,
                "out": sorted({c for c, _, _ in p.edges.get(f.id, [])}),
                "in": sorted({c for c, _, _ in p.redges.get(f.id, [])}),
            })
        nodes.append({"id": m.name, "file": m.file, "lines": m.lines, "dir": m.file.rsplit("/", 1)[0] if "/" in m.file else ".",
                      "bad": fcount.get(m.file, 0), "funcs": funcs})
    data = {
        "name": p.config.get("project", {}).get("name", p.root),
        "nodes": nodes,
        "edges": [[a, b, n] for (a, b), n in medges.items()],
        "findings": [f.__dict__ for f in findings],
        "rules": [{"id": r["id"], "kind": r.get("kind", "forbid"), "why": r.get("why", ""), "sev": r.get("severity", "error")}
                  for r in p.config.get("rule", [])],
    }
    with open(out_path, "w", encoding="utf-8") as fh:
        fh.write(_PAGE.replace("__TITLE__", html.escape(data["name"])).replace("__DATA__", json.dumps(data)))


_PAGE = r"""<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>__TITLE__ seam map</title>
<style>
:root{--bg:#f6f5f2;--panel:#fff;--ink:#1d1d1f;--dim:#6b6b70;--line:#d9d7d0;--ok:#3b7d5a;--bad:#c0392b;--warn:#b7791f;--node:#5b6fb4;--hi:#e0a526}
@media (prefers-color-scheme:dark){:root:not([data-theme=light]){--bg:#15161a;--panel:#1e2026;--ink:#e8e6e1;--dim:#9a9aa3;--line:#33353d;--node:#7d8fd6}}
:root[data-theme=dark]{--bg:#15161a;--panel:#1e2026;--ink:#e8e6e1;--dim:#9a9aa3;--line:#33353d;--node:#7d8fd6}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:14px/1.45 system-ui,sans-serif;display:grid;grid-template-columns:1fr 380px;height:100vh}
#g{width:100%;height:100vh;display:block;cursor:grab}
aside{background:var(--panel);border-left:1px solid var(--line);overflow:auto;padding:14px 16px}
h1{font-size:16px;margin:0 0 4px}h2{font-size:13px;text-transform:uppercase;letter-spacing:.06em;color:var(--dim);margin:18px 0 6px}
input{width:100%;padding:7px 9px;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--ink)}
.f{padding:6px 0;border-bottom:1px solid var(--line);font-size:12.5px}.f b{font-family:ui-monospace,monospace}
.sev-error{color:var(--bad)}.sev-warn{color:var(--warn)}.dim{color:var(--dim)}
.fn{font-family:ui-monospace,monospace;font-size:12px;padding:3px 0;cursor:pointer}.fn:hover{color:var(--hi)}
.tag{display:inline-block;padding:0 6px;border-radius:9px;font-size:11px;border:1px solid var(--line);margin-left:4px}
details{margin:2px 0}summary{cursor:pointer}
@media (max-width:760px){body{grid-template-columns:1fr;grid-template-rows:55vh 1fr}#g{height:55vh}aside{border-left:0;border-top:1px solid var(--line)}}
</style></head><body>
<canvas id="g"></canvas>
<aside>
<h1>__TITLE__</h1><div class="dim" id="sum"></div>
<input id="q" placeholder="find a module or function…" autocomplete="off">
<div id="detail"></div>
<h2>Findings</h2><div id="finds"></div>
<h2>Contracts</h2><div id="rules"></div>
</aside>
<script>
const D=__DATA__;const cv=document.getElementById('g'),cx=cv.getContext('2d');
const N=D.nodes.map((n,i)=>({...n,i,x:Math.cos(i)*300+Math.random()*50,y:Math.sin(i*1.7)*300+Math.random()*50,vx:0,vy:0,r:4+Math.sqrt(n.lines)/3}));
var heat=1;const E=D.edges;let sel=null,hot=new Set(),view={x:0,y:0,k:1};
const css=v=>getComputedStyle(document.documentElement).getPropertyValue(v).trim();
function size(){cv.width=cv.clientWidth*devicePixelRatio;cv.height=cv.clientHeight*devicePixelRatio}addEventListener('resize',size);size();
// group by directory so related modules cluster
// deterministic layout: folders on a ring (bigger folders get more arc),
// each folder's modules packed in a small spiral around its spot
const dirs=[...new Set(N.map(n=>n.dir))].sort();const cnt=Object.fromEntries(dirs.map(d=>[d,N.filter(n=>n.dir===d).length]));
const total=N.length;let acc=0;const R=Math.max(260,Math.sqrt(total)*38);const dpos={};
for(const d of dirs){const mid=(acc+cnt[d]/2)/total*6.283;acc+=cnt[d];dpos[d]=[Math.cos(mid)*R,Math.sin(mid)*R,0]}
N.forEach(n=>{const p=dpos[n.dir];const k=p[2]++;const a=k*2.4,r=9*Math.sqrt(k)+2;n.gx=p[0]+Math.cos(a)*r*2.2;n.gy=p[1]+Math.sin(a)*r*2.2;n.x=n.gx;n.y=n.gy});
heat=0.25;
var heat;function step(){for(const n of N){n.vx+=(n.gx-n.x)*0.0025;n.vy+=(n.gy-n.y)*0.0025}
for(let a=0;a<N.length;a++)for(let b=a+1;b<N.length;b++){const p=N[a],q=N[b];if(p.dir!==q.dir&&N.length>400)continue;let dx=q.x-p.x,dy=q.y-p.y,d2=dx*dx+dy*dy+0.01;if(d2>90000)continue;const f=900/d2;p.vx-=dx*f*0.02;p.vy-=dy*f*0.02;q.vx+=dx*f*0.02;q.vy+=dy*f*0.02}
for(const [a,b,w] of E){const p=N[a],q=N[b];const dx=q.x-p.x,dy=q.y-p.y;const f=0.0004*Math.min(w,8);p.vx+=dx*f;p.vy+=dy*f;q.vx-=dx*f;q.vy-=dy*f}
for(const n of N){n.x+=n.vx*heat;n.y+=n.vy*heat;n.vx*=0.6;n.vy*=0.6}heat=Math.max(0.02,heat*0.995)}
function draw(){const W=cv.width,H=cv.height;cx.setTransform(1,0,0,1,0,0);cx.fillStyle=css('--bg');cx.fillRect(0,0,W,H);
cx.setTransform(view.k*devicePixelRatio,0,0,view.k*devicePixelRatio,W/2+view.x*devicePixelRatio,H/2+view.y*devicePixelRatio);
for(const [a,b,w] of E){const on=sel&&(a===sel.i||b===sel.i);if(sel&&!on)continue;cx.strokeStyle=on?(a===sel.i?css('--hi'):css('--node')):css('--line');cx.globalAlpha=on?0.9:0.35;cx.lineWidth=Math.min(4,0.5+Math.log2(w+1)*0.5)/view.k;cx.beginPath();cx.moveTo(N[a].x,N[a].y);cx.lineTo(N[b].x,N[b].y);cx.stroke()}
cx.globalAlpha=1;for(const n of N){cx.beginPath();cx.arc(n.x,n.y,n.r,0,6.283);cx.fillStyle=n.bad?css('--bad'):(hot.has(n.i)?css('--hi'):css('--node'));cx.globalAlpha=sel&&sel!==n&&!hot.has(n.i)?0.35:1;cx.fill();
if(view.k>0.6||n.r>12||sel===n||hot.has(n.i)){cx.fillStyle=css('--ink');cx.font=`${11/view.k}px system-ui`;cx.fillText(n.id,n.x+n.r+2,n.y+3)}}cx.globalAlpha=1}
function loop(){if(heat>0.03)step();draw();requestAnimationFrame(loop)}loop();
function at(ev){const r=cv.getBoundingClientRect();const x=(ev.clientX-r.left-cv.clientWidth/2-view.x)/view.k,y=(ev.clientY-r.top-cv.clientHeight/2-view.y)/view.k;let best=null,bd=1e9;for(const n of N){const d=(n.x-x)**2+(n.y-y)**2;if(d<bd&&d<(n.r+6)**2){bd=d;best=n}}return best}
let drag=null;cv.onmousedown=e=>{drag={x:e.clientX,y:e.clientY,vx:view.x,vy:view.y,moved:false}};onmousemove=e=>{if(!drag)return;const dx=e.clientX-drag.x,dy=e.clientY-drag.y;if(Math.abs(dx)+Math.abs(dy)>3)drag.moved=true;view.x=drag.vx+dx;view.y=drag.vy+dy};
onmouseup=e=>{if(drag&&!drag.moved){const n=at(e);pick(n)}drag=null};cv.onwheel=e=>{e.preventDefault();view.k*=e.deltaY<0?1.12:0.89;view.k=Math.min(6,Math.max(0.15,view.k))};
const byId=Object.fromEntries(N.map(n=>[n.id,n]));const esc=s=>String(s).replace(/[&<>]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;'})[c]);
function pick(n){sel=n;hot=new Set();const el=document.getElementById('detail');if(!n){el.innerHTML='';return}
for(const [a,b] of E){if(a===n.i)hot.add(b);if(b===n.i)hot.add(a)}
const fs=D.findings.filter(f=>f.file===n.file);
el.innerHTML=`<h2>${esc(n.id)}</h2><div class="dim">${esc(n.file)} · ${n.lines} lines · ${n.funcs.length} functions</div>`+
(fs.length?`<div>${fs.map(f=>`<div class="f"><span class="sev-${f.severity}">${f.rule}</span> :${f.line} ${esc(f.message)}</div>`).join('')}</div>`:'')+
n.funcs.map(f=>`<details><summary class="fn">${esc(f.n)} <span class="dim">:${f.l}</span><span class="tag">in ${f.in.length}</span><span class="tag">out ${f.out.length}</span></summary>
<div class="dim" style="font-size:12px;margin-left:12px">${f.in.length?'called by: '+f.in.map(x=>`<span class="fn" data-m="${esc(x.split('.').slice(0,-1).join('.'))}">${esc(x)}</span>`).join(', '):'called by nothing'}<br>${f.out.length?'calls: '+f.out.map(x=>`<span class="fn" data-m="${esc(x.split('.').slice(0,-1).join('.'))}">${esc(x)}</span>`).join(', '):''}</div></details>`).join('');
el.querySelectorAll('[data-m]').forEach(s=>s.onclick=()=>{const m=byId[s.dataset.m];if(m){pick(m);view.x=-m.x*view.k;view.y=-m.y*view.k}})}
document.getElementById('q').oninput=e=>{const q=e.target.value.toLowerCase();if(!q){hot=new Set();return}hot=new Set(N.filter(n=>n.id.toLowerCase().includes(q)||n.funcs.some(f=>f.n.toLowerCase().includes(q))).map(n=>n.i))};
const errs=D.findings.filter(f=>f.severity==='error').length;
document.getElementById('sum').textContent=`${N.length} modules · ${N.reduce((a,n)=>a+n.funcs.length,0)} functions · ${E.length} links · ${errs} errors, ${D.findings.length-errs} warnings`;
document.getElementById('finds').innerHTML=D.findings.length?D.findings.map(f=>`<div class="f"><span class="sev-${f.severity}">${f.severity}</span> <b>${esc(f.rule)}</b><br><span class="fn" data-f="${esc(f.file)}">${esc(f.file)}:${f.line}</span> ${esc(f.message)}</div>`).join(''):'<div class="dim">None. Every contract holds.</div>';
document.querySelectorAll('[data-f]').forEach(s=>s.onclick=()=>{const m=N.find(n=>n.file===s.dataset.f);if(m){pick(m);view.x=-m.x*view.k;view.y=-m.y*view.k}});
document.getElementById('rules').innerHTML=D.rules.map(r=>{const c=D.findings.filter(f=>f.rule===r.id).length;return `<div class="f"><b>${esc(r.id)}</b> <span class="tag">${r.kind}</span> ${c?`<span class="sev-${r.sev}">${c} broken</span>`:'<span style="color:var(--ok)">holds</span>'}<br><span class="dim">${esc(r.why)}</span></div>`}).join('');
</script></body></html>"""
