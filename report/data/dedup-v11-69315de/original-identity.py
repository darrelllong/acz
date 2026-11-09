from pathlib import Path
import hashlib,subprocess,json,random
r=Path(__file__).parent
old=r/'v10-source/target/release/acz';new=r/'source/target/release/acz'
rng=random.Random(91749)
noise=bytes(rng.randrange(256) for _ in range(8193))
inputs={'empty':b'','one':b'x','alphabet':bytes(range(256)),'noise':noise,'periodic':bytes(range(251))*262,'cross_block':noise*16+b'end','text':('Arithmetic coding and the memory of Glen Langdon.\n'*1024).encode()}
rows=[]
for model in range(5):
 for name,data in inputs.items():
  for block in ('64K','128K'):
   args=['-'+str(model),'--no-dedup','--block='+block,'-T2','--memory=4G','-c']
   a=subprocess.run([str(old)]+args,input=data,capture_output=True,check=True).stdout
   b=subprocess.run([str(new)]+args,input=data,capture_output=True,check=True).stdout
   assert a[:4]==b'ACZ\x0a' and b[:4]==b'ACZ\x0b'
   assert a[16:]==b[16:] and a[4:8]==b[4:8],(model,name,block)
   decoded=subprocess.run([str(new),'-d','-T2','--memory=4G','-c'],input=b,capture_output=True,check=True).stdout
   assert decoded==data
   rows.append(dict(model=model,input=name,block=block,raw_bytes=len(data),raw_sha256=hashlib.sha256(data).hexdigest(),v10_bytes=len(a),v11_bytes=len(b),v10_sha256=hashlib.sha256(a).hexdigest(),v11_sha256=hashlib.sha256(b).hexdigest(),payload_equal=True,roundtrip=True))
 (r/'identity-rows.json').write_text(json.dumps(rows,indent=2)+'\n');print('identity model',model,'cases',len(rows),flush=True)
print('PASS',len(rows),'cases',flush=True)
