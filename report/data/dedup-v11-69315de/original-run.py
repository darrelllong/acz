from pathlib import Path
import subprocess,json,hashlib,time,sys
root=Path(__file__).parent
exe=root/'source/target/release/acz'
inputs=json.loads(Path('/Users/darrell/coding/acz-bench/dedup-ablation/2026-11-07-pilot/inputs.json').read_text())
level=int(sys.argv[1]); out=root/('containers-'+str(level)); out.mkdir(exist_ok=True)
rows=[]
for x in inputs:
 src=Path(x['path']); b=src.read_bytes()
 assert len(b)==x['bytes'] and hashlib.sha256(b).hexdigest()==x['sha256']
 packed=out/(src.name+'.acz'); decoded=out/(src.name+'.decoded')
 enc=[str(exe),'-'+str(level),'--dedup=256M','--block=16M','-T1','--memory=4G','-c',str(src)]
 with packed.open('wb') as f:
  r=subprocess.run(enc,stdout=f,stderr=subprocess.PIPE); assert r.returncode==0,r.stderr
 dec=[str(exe),'-d','-T1','--memory=4G','-c',str(packed)]
 with decoded.open('wb') as f:
  r=subprocess.run(dec,stdout=f,stderr=subprocess.PIPE); assert r.returncode==0,r.stderr
 assert decoded.read_bytes()==b
 row=dict(file=src.name,raw_bytes=len(b),raw_sha256=x['sha256'],packed_bytes=packed.stat().st_size,packed_sha256=hashlib.sha256(packed.read_bytes()).hexdigest(),roundtrip=True,encode=enc,decode=dec)
 rows.append(row);(out/'rows.json').write_text(json.dumps(rows,indent=2)+'\n')
 decoded.unlink(); print(level,src.name,row['packed_bytes'],flush=True)
result=dict(commit='69315dee2e8f9068e3ab52c4cc0cebe5ea2311b0',format=11,model=level,files=len(rows),input_bytes=sum(r['raw_bytes'] for r in rows),packed_bytes=sum(r['packed_bytes'] for r in rows),all_roundtrips=True,binary_sha256=hashlib.sha256(exe.read_bytes()).hexdigest(),purpose='Container-size and correctness validation only; not a timing experiment')
(out/'validation.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result),flush=True)
