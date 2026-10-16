import os,re,sys,subprocess,random,collections,tempfile
from concurrent.futures import ThreadPoolExecutor
ROOT=os.path.dirname(os.path.dirname(os.path.abspath(__file__))); T=f'{ROOT}/test262'; CLOISON=f'{ROOT}/target/release/cloison'
sample=int(sys.argv[1]); random.seed(int(sys.argv[2]) if len(sys.argv)>2 else 1); only=sys.argv[3] if len(sys.argv)>3 else ''
files=[]
for d in ['built-ins','language','annexB']:
    for r,_,fs in os.walk(f'{T}/test/{d}'):
        for f in fs:
            if f.endswith('.js') and '_FIXTURE' not in f: files.append(f'{r}/{f}')
def meta(src):
    m=re.search(r'/\*---(.*?)---\*/',src,re.S); y=m.group(1) if m else ''
    def lst(k):
        m=re.search(k+r':\s*\[(.*?)\]',y,re.S)
        if m: return [x.strip() for x in m.group(1).split(',') if x.strip()]
        m=re.search(k+r':\s*\n((?:\s+-.*\n?)+)',y)
        return [x.strip()[1:].strip() for x in m.group(1).splitlines()] if m else []
    return lst('features'),lst('flags'),lst('includes'),'negative:' in y
cands=[]
for p in files:
    s=open(p,encoding='utf8',errors='replace').read()
    ft,fl,inc,neg=meta(s)
    if ft or neg or set(fl)&{'onlyStrict','module','async','raw'}: continue
    if only and only not in p: continue
    cands.append((p,s,inc))
print('ES5-ish candidates',len(cands),'of',len(files))
random.shuffle(cands); cands=cands[:sample] if sample else cands
H=lambda n:open(f'{T}/harness/{n}').read()
def run(c):
    p,s,inc=c
    pre=H('assert.js')+H('sta.js')+''.join(H(i) for i in inc if os.path.exists(f'{T}/harness/{i}'))
    with tempfile.NamedTemporaryFile('w',suffix='.js',delete=False) as f: f.write(pre+'\n'+s); n=f.name
    try:
        r=subprocess.run([CLOISON,n],capture_output=True,text=True,timeout=10); ok=r.returncode==0; msg=(r.stderr or r.stdout).strip()[:100]
        if r.returncode<0: msg='CRASH '+msg
    except subprocess.TimeoutExpired: ok=False; msg='TIMEOUT'
    os.unlink(n); return p,ok,msg
with ThreadPoolExecutor(8) as ex: res=list(ex.map(run,cands))
bd=collections.defaultdict(lambda:[0,0]); er=collections.Counter()
for p,ok,m in res:
    k='/'.join(p[len(T)+6:].split('/')[:2]); bd[k][0]+=ok; bd[k][1]+=1
    if not ok: er[re.sub(r'\d+','N',m)[:70]]+=1
print('PASS',sum(o for _,o,_ in res),'/',len(res))
for k,(a,b) in sorted(bd.items(),key=lambda x:-x[1][1])[:25]: print(f'{k:40} {a}/{b} {100*a//b}%')
print('top failures'); [print(c,m) for m,c in er.most_common(40)]
with open('fails.txt','w') as f:
    for p,ok,m in sorted(res):
        if not ok: f.write(p[len(T)+6:]+'\t'+m.replace('\n',' ')+'\n')
with open('passes.txt','w') as f:
    for p,ok,m in sorted(res):
        if ok: f.write(p[len(T)+6:]+'\n')
