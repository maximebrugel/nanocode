import json,sys,collections,subprocess,os
here=os.path.dirname(os.path.abspath(__file__))
src=open(os.path.join(here,'matrix.py')).read()
ns={'__name__':'x'}; sys.argv=['m','-','x']
import io,contextlib
# load O by exec of matrix up to O definition
code=src.split('PRE_LIB')[0]; exec(code,ns); O=ns['O']
m=json.load(open(sys.argv_[0] if False else os.environ['M']))
def key(o):
    st=o.get('style','decimal'); k=[st]
    if st=='currency': k.append(o.get('currencyDisplay','symbol')); k.append(o.get('currencySign',''))
    if st=='unit': k.append(o.get('unitDisplay','short'))
    if o.get('notation','standard')!='standard': k.append(o['notation']+o.get('compactDisplay',''))
    return '/'.join(x for x in k if x)
c=collections.Counter(); ex={}
for x in m:
    if x['cat']!='nf': continue
    p=x['node'].split('|'); i=int(p[1]); sub=p[2]
    sub = sub if sub in ('R','str','big','rng','C') else 'v'
    k=(x['loc'] if os.environ.get('BYLOC') else '')+' '+key(O[i])+' '+sub
    c[k]+=1; ex.setdefault(k,[]).append((O[i],x['node'],x['djs']))
for k,v in c.most_common(int(os.environ.get('N','40'))):
    print(v,k); 
    for e in ex[k][:int(os.environ.get('S','1'))]: print('   ',json.dumps(e[0]),'\n     N',e[1][:220],'\n     D',e[2][:220])
