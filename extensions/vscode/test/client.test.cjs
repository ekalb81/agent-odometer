const { test } = require('node:test');
const assert = require('node:assert/strict');
const { runCli, parseReport, executablePath } = require('../src/client.cjs');
const { status, usage, envelope } = require('./fixtures.cjs');
const path = require('node:path');
const executable = path.resolve('local odometer', process.platform === 'win32' ? 'agent-odometer.exe' : 'agent-odometer');
test('only absolute executable paths and two fixed read-only commands are launched without a shell', async () => {
  let calls = 0;
  const execute = (file, args, options, callback) => {
    calls++; assert.equal(file, executable); assert.deepEqual(args, ['integration-status','--schema-version','2','--format','json']);
    assert.equal(options.shell, false); assert.equal(options.windowsHide, true); assert.equal(options.timeout,12000); assert.equal(options.maxBuffer, 8*1024*1024);
    callback(null, envelope('integration-status', status()));
  };
  assert.equal((await runCli(executable, 'integration-status', undefined, execute)).ledger_available, true);
  await assert.rejects(runCli(executable,'report; upload',undefined,execute));
  for (const value of ['', './agent-odometer', executable+'.cmd', executable+'\0', path.resolve('powershell.exe'), '//server/share/agent-odometer']) assert.throws(() => executablePath(value));
  assert.equal(calls,1);
});
test('invalid, mismatched, unsafe numeric, and future schemas fail closed', () => {
  for (const value of ['{', JSON.stringify({schema_version:3,report:'statusline',data:usage()}), envelope('integration-status',usage())]) assert.throws(() => parseReport(value,'statusline'));
  const bad = usage(); bad.tokens.total_tokens = Number.MAX_SAFE_INTEGER + 1;
  assert.throws(() => parseReport(envelope('statusline',bad),'statusline'));
  bad.tokens.total_tokens = 1; bad.providers=[null]; assert.throws(() => parseReport(envelope('statusline',bad),'statusline'));
  const malformed = usage(); malformed.providers[0].pricing.current.api_estimate.by_model = {};
  assert.throws(() => parseReport(envelope('statusline',malformed),'statusline'));
  assert.equal(parseReport(envelope('statusline',usage()),'statusline').tokens.total_tokens,1100);
});
test('stderr, paths, and arbitrary exception details never become UI errors', async () => {
  for (const [error, code] of [[{code:'ENOENT'},'missing'],[{killed:true},'timeout'],[{code:2,message:'PRIVATE_PATH token=secret'},'query']]) {
    await assert.rejects(runCli(executable,'statusline',undefined,(_f,_a,_o,done)=>done(error,'PRIVATE_STDOUT')), value => value.code===code && !value.message.includes('PRIVATE'));
  }
});
test('a real missing executable follows the bounded failure path', async () => {
  await assert.rejects(runCli(path.resolve('odometer263-missing',process.platform==='win32'?'agent-odometer.exe':'agent-odometer'),'integration-status'),error=>error.code==='missing');
});
