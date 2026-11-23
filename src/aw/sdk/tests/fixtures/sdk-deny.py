import json
from pathlib import Path
import sys
request=json.load(sys.stdin)
reply={'api_version':'aw-provider/v1alpha1','request_id':request['request_id'],'status':'ok'}
if request['method']=='describe':
 reply['operations']=[
  {'name':'check','events':['input.submit','tool.before'],'effects':['observe','block']},
  {'name':'start','events':['session.start'],'effects':['observe']},
 ]
elif request['method']=='invoke':
 reply['input_digest']=request['input_digest']
 if request['operation']=='start':
  Path(request['config']['start_file']).write_text(request['event']['session_id'])
  reply['effects']=[]
 else:
  reply['effects']=[{'type':'block','reason_code':'fixture_deny','text':'Blocked by AW fixture. Stop without retry.'}] if Path(request['config']['deny_file']).exists() else []
print(json.dumps(reply))
