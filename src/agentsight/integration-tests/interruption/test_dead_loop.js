const https = require('https');
const crypto = require('crypto');
const API_KEY = process.argv[2] || '';
const ROUNDS = parseInt(process.argv[3] || '5', 10);
const INTERVAL = parseInt(process.argv[4] || '3', 10) * 1000;
const USER_MESSAGE = 'Please read the file /etc/hosts and show me its content. Use the read_file tool.';
const TOOLS = [{type:'function',function:{name:'read_file',description:'Read file content',parameters:{type:'object',properties:{path:{type:'string',description:'File path'}},required:['path']}}}];

function sendRequest(){
  return new Promise((resolve,reject)=>{
    const payload=JSON.stringify({model:'qwen-max',messages:[{role:'user',content:USER_MESSAGE}],tools:TOOLS,tool_choice:'auto',max_tokens:100});
    // agent: false disables keep-alive, forces new TCP+SSL per request
    const options={hostname:'dashscope.aliyuncs.com',port:443,path:'/compatible-mode/v1/chat/completions',method:'POST',agent:false,headers:{'Content-Type':'application/json','Authorization':'Bearer '+API_KEY,'Content-Length':Buffer.byteLength(payload),'Connection':'close'}};
    const req=https.request(options,(res)=>{let body='';res.on('data',chunk=>body+=chunk);res.on('end',()=>resolve({status:res.statusCode,body}));});
    req.on('error',e=>reject(e));
    req.write(payload);
    req.end();
  });
}

function sleep(ms){return new Promise(r=>setTimeout(r,ms))}

async function main(){
  if(API_KEY.length === 0){console.error('Usage: node test_dead_loop.js <API_KEY> [rounds] [interval]');process.exit(1)}
  const convId=crypto.createHash('sha256').update(USER_MESSAGE).digest('hex').slice(0,32);
  console.log('DeadLoop Test | conv_id:'+convId+' | rounds:'+ROUNDS);
  // Wait 5s for agentsight to discover and attach SSL probes to this process
  console.log('  Waiting 5s for agentsight to attach...');
  await sleep(5000);
  for(let i=0;i<ROUNDS;i++){
    process.stdout.write('  ['+(i+1)+'/'+ROUNDS+'] ');
    try{
      const{status,body}=await sendRequest();
      let d='';
      try{
        const data=JSON.parse(body);
        const msg=data.choices && data.choices[0] && data.choices[0].message;
        if(msg && msg.tool_calls && msg.tool_calls.length){
          d='tools:['+msg.tool_calls.map(t=>t.function && t.function.name).join(',')+']';
        }else if(msg && msg.content){
          d='text:'+msg.content.slice(0,50);
        }else{d='empty'}
      }catch(e){d='parse_err'}
      console.log('status='+status+' '+d);
    }catch(e){console.log('ERR:'+e.message)}
    if(i<ROUNDS-1)await sleep(INTERVAL);
  }
  console.log('All done. Waiting 20s for detection...');
  await sleep(20000);
  console.log('Check: agentsight interruption list');
}

main().catch(e=>{console.error(e);process.exit(1)});
