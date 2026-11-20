import readline from "node:readline";
import fs from "node:fs";
const log = process.argv[2];
const send = v => console.log(JSON.stringify(v));
const reply = (id, result) => send({jsonrpc:"2.0",id,result});
const update = v => send({jsonrpc:"2.0",method:"session/update",params:{sessionId:"sg39-session",update:v}});
const chunk = (messageId,text) => update({sessionUpdate:"agent_message_chunk",messageId,content:{type:"text",text}});
for await (const line of readline.createInterface({input:process.stdin})) {
  const m=JSON.parse(line),p=m.params??{};
  if(m.method==="initialize") reply(m.id,{protocolVersion:1,agentCapabilities:{loadSession:true}});
  else if(m.method==="session/new"||m.method==="session/load") {
    fs.writeFileSync(log+".session",JSON.stringify(p));
    fs.writeFileSync(log+".cwd",process.cwd());
    fs.writeFileSync(log+".tmp",process.env.TMPDIR??"");
    reply(m.id,{sessionId:"sg39-session",modes:{currentModeId:"plan",availableModes:[{id:"plan"},{id:"default"},{id:"full_access"}]}});
  } else if(m.method==="session/prompt") {
    fs.writeFileSync(log+".prompt",JSON.stringify(p.prompt));
    const text=p.prompt.filter(v=>v.type==="text").map(v=>v.text).join("\n");
    chunk("first","first");chunk("first"," draft");
    update({sessionUpdate:"usage_update",used:7,size:100,cost:{amount:0.01,currency:"USD"}});
    if(text.includes("hold")) continue;
    if(text.includes("error")) {
      send({jsonrpc:"2.0",id:m.id,error:{code:-32603,message:"API Error: 503 overloaded"}});
      continue;
    }
    chunk("second","answer");
    if(text.includes("read")) for(let i=0;i<2;i++) update({sessionUpdate:"tool_call",toolCallId:`read-${i}`,kind:"read",status:"completed",title:"Read",locations:[{path:"src/data"}],rawInput:{path:"src/data"}});
    reply(m.id,{stopReason:text.includes("budget")?"max_tokens":"end_turn"});
  } else if(m.id!==undefined) reply(m.id,{});
}
