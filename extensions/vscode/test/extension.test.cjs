const { test }=require('node:test');
const assert=require('node:assert/strict');
const { createExtension }=require('../src/extension.cjs');
const { status,usage }=require('./fixtures.cjs');
function mock(trusted=true){
  const commands=new Map(),listeners={};const subscriptions=[];const messages=[];
  class Emitter{constructor(){this.event=()=>({dispose(){}});}fire(){}dispose(){}}
  const config={inspect:key=>({globalValue:key==='executablePath'?'/user/odometer':0,workspaceValue:'/repo/malicious'}),update:async()=>{}};
  const view={visible:true,onDidChangeVisibility:callback=>{listeners.visibility=callback;return {dispose(){}};},dispose(){}};
  const vscode={EventEmitter:Emitter,TreeItem:class{constructor(label){this.label=label;}},TreeItemCollapsibleState:{Expanded:2,None:0},StatusBarAlignment:{Right:2},ConfigurationTarget:{Global:1},Uri:{joinPath:()=>''},
    window:{showInformationMessage:value=>messages.push(value),createStatusBarItem:()=>({show(){},dispose(){}}),createTreeView:()=>view},
    workspace:{isTrusted:trusted,getConfiguration:()=>config,onDidChangeConfiguration:callback=>{listeners.configuration=callback;return {dispose(){}};}},
    commands:{registerCommand:(name,callback)=>{commands.set(name,callback);return {dispose(){commands.delete(name);}};},executeCommand:async()=>{}}};
  return {vscode,context:{subscriptions,extensionUri:{}},commands,listeners,messages,dispose:()=>subscriptions.reverse().forEach(item=>item.dispose())};
}
test('untrusted workspace cannot launch a configured executable',async()=>{
  const m=mock(false);let calls=0;const extension=createExtension(m.vscode,m.context,async()=>{calls++;return status();});
  try{await extension.refresh();assert.equal(calls,0);assert.equal(extension.data.state.error,'setup');}finally{m.dispose();}
});
test('workspace executable overrides are ignored and config changes clear prior data',async()=>{
  const m=mock();const paths=[];const extension=createExtension(m.vscode,m.context,async(file,kind)=>{paths.push(file);return kind==='integration-status'?status():usage();});
  try{await extension.refresh();assert.deepEqual(paths,['/user/odometer','/user/odometer']);m.listeners.configuration({affectsConfiguration:()=>true});assert.equal(extension.data.state.snapshot,null);}finally{m.dispose();}
});
test('deactivation cancels in-flight work without registering more background queries',async()=>{
  const m=mock();let signal,resolve,calls=0;const extension=createExtension(m.vscode,m.context,async(_p,_k,s)=>{calls++;signal=s;return new Promise(done=>resolve=done);});
  const pending=extension.refresh();m.dispose();assert.ok(signal.aborted);resolve(status());await pending;assert.equal(calls,1);assert.equal(extension.data.state.snapshot,null);
});

test('narrow rows expose complete values through accessibility and keyboard activation',async()=>{
  const m=mock();const extension=createExtension(m.vscode,m.context,async(_p,k)=>k==='integration-status'?status():usage());
  try{await extension.refresh();const row=extension.provider.getChildren().find(item=>item.id==='coverage');const item=extension.provider.getTreeItem(row);assert.match(item.accessibilityInformation.label,/scan completeness unknown/);m.commands.get(item.command.command)(...item.command.arguments);assert.equal(m.messages[0],item.tooltip);}finally{m.dispose();}
});
