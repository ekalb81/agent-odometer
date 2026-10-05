const assert = require('node:assert/strict');
const path = require('node:path');
const vscode = require('vscode');
const { status, usage } = require('./fixtures.cjs');
exports.run = async function() {
  console.log('ODOMETER_HOST_START');
  const extension=vscode.extensions.getExtension('ekalb81.odometer-local');assert.ok(extension);
  const actual=await extension.activate();assert.ok(actual.provider);
  async function refreshAndWait() {
    await actual.refresh(); const until=Date.now()+26000;
    while(actual.data.state.busy || actual.data.active) {
      if(Date.now()>until)throw Error('bounded refresh did not finish');
      await new Promise(resolve=>setTimeout(resolve,20));
    }
  }
  console.log('ODOMETER_HOST_ACTIVATED');
  await vscode.commands.executeCommand('odometer.open');
  await refreshAndWait();assert.equal(actual.data.state.error,'setup');
  assert.ok(actual.provider.getChildren().some(row=>row.id==='setup'));
  const config=vscode.workspace.getConfiguration('odometer');
  await config.update('executablePath',path.resolve('/odometer-test-missing',process.platform==='win32'?'agent-odometer.exe':'agent-odometer'),vscode.ConfigurationTarget.Global);
  await new Promise(resolve=>setTimeout(resolve,150));await refreshAndWait();assert.equal(actual.data.state.error,'missing');
  await vscode.commands.executeCommand('odometer.setup');assert.equal(path.basename(vscode.window.activeTextEditor.document.fileName),'README.md');
  // Real editor APIs and synthetic responses exercise the actual tree items without reading user sessions.
  const originalRunner=actual.data.runner;actual.data.reset();actual.data.runner=async(_p,k)=>k==='integration-status'?status():usage();
  await refreshAndWait();const roots=actual.provider.getChildren();const today=roots.find(row=>row.id==='today');assert.ok(today);
  assert.equal(actual.provider.getTreeItem(today).label,'Today · recorded usage');
  assert.equal(actual.provider.getChildren(today).find(row=>row.id==='tokens').description,'1,100');
  actual.data.reset();actual.data.runner=originalRunner;
  if(process.env.ODOMETER_TEST_EXECUTABLE){
    await config.update('executablePath',process.env.ODOMETER_TEST_EXECUTABLE,vscode.ConfigurationTarget.Global);
    await new Promise(resolve=>setTimeout(resolve,150));await refreshAndWait();
    assert.equal(actual.data.state.error,null);assert.ok(actual.data.state.snapshot);
    if(process.env.ODOMETER_TEST_EXPECT_TOKENS) { assert.equal(actual.data.state.snapshot.status.ledger_available,true);assert.ok(actual.data.state.snapshot.usage);assert.equal(actual.data.state.snapshot.usage.tokens.total_tokens,Number(process.env.ODOMETER_TEST_EXPECT_TOKENS)); }
  }
  if(process.env.ODOMETER_TEST_EVIDENCE){
    const fs=require('node:fs');const directory=process.env.ODOMETER_TEST_EVIDENCE;
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
    await vscode.commands.executeCommand('odometer.dashboard.focus');
    if ((await vscode.commands.getCommands()).includes('notifications.clearAll')) await vscode.commands.executeCommand('notifications.clearAll');
    fs.writeFileSync(path.join(directory,'host-ready.json'),JSON.stringify({version:vscode.version,state:actual.data.state,rows:actual.provider.getChildren()},null,2));
    const until=Date.now()+45000;while(!fs.existsSync(path.join(directory,'host-continue'))&&Date.now()<until)await new Promise(resolve=>setTimeout(resolve,100));
  }
  console.log('ODOMETER_HOST_PASS '+vscode.version+' registration/setup/missing/backend/tree/privacy');
};
