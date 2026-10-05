const { test } = require('node:test');
const assert = require('node:assert/strict');
const { Dashboard, rows } = require('../src/dashboard.cjs');
const { now, status, usage } = require('./fixtures.cjs');
const flatten = list => list.flatMap(item => [item, ...flatten(item.children)]);
test('two read-only responses preserve separate exact prices, coverage, and observation age', async () => {
  const s=status(); s.coverage_complete=false;
  const data=new Dashboard(()=>{},async(_path,kind)=>kind==='integration-status'?s:usage(),()=>Date.parse(now));
  await data.refresh('/selected'); const output=flatten(rows(data.state,Date.parse(now)+360000));
  assert.match(output.find(r=>r.id==='coverage').description,/Partial/);
  assert.match(output.find(r=>r.id==='observed').description,/older than five minutes/);
  assert.equal(output.find(r=>r.id==='provider.0.api_estimate').description,'0.0071 USD');
  assert.equal(output.find(r=>r.id==='provider.0.purchased_credits').description,'0.1775 credits');
  assert.equal(output.find(r=>r.id==='fetched').label,'Stale query snapshot');
  assert.match(output.find(r=>r.id==='scan').description,/Unknown/);
});
test('missing history skips usage; partial query failure never fabricates zero',async()=>{
  let calls=0;
  const data=new Dashboard(()=>{},async()=>{calls++;return {...status(),ledger_available:false,sessions:null};});
  await data.refresh('/selected'); assert.equal(calls,1); assert.equal(data.state.snapshot.usage,null);
  assert.equal(flatten(rows(data.state)).find(r=>r.id==='sessions').description,'Unavailable');
  data.runner=async(_p,kind)=>{if(kind==='statusline') throw Object.assign(new Error('PRIVATE'),{code:'timeout'});return status();};
  await data.refresh('/selected'); assert.equal(data.state.snapshot.usage,null); assert.equal(data.state.snapshot.usageError,'timeout');
  assert.ok(!JSON.stringify(rows(data.state)).includes('PRIVATE'));
});
test('failure keeps the prior snapshot explicitly stale; reset removes it and cancels superseded replies',async()=>{
  let fail=false;const data=new Dashboard(()=>{},async(_p,k)=>{if(fail)throw {code:'query'};return k==='integration-status'?status():usage();});
  await data.refresh('/one');fail=true;await data.refresh('/one');assert.match(rows(data.state)[0].label,/previous snapshot/);
  let resolve; data.runner=()=>new Promise(done=>{resolve=done;});
  const pending=data.refresh('/old');data.reset();resolve(status());await pending;
  assert.equal(data.state.snapshot,null); assert.equal(data.state.busy,false);
});
test('one query runs at a time and disposal aborts it',async()=>{
  let signal,calls=0,resolve;const data=new Dashboard(()=>{},async(_p,_k,s)=>{calls++;signal=s;return new Promise(done=>resolve=done);});
  const first=data.refresh('/one'); await data.refresh('/one');assert.equal(calls,1);data.dispose();assert.ok(signal.aborted);resolve({...status(),ledger_available:false});await first;assert.equal(data.state.snapshot,null);
});
test('unavailable prices and provider identity remain explicit without leaking extra response fields',async()=>{
  const u=usage();u.providers[0].pricing.current.api_estimate.total=null;u.providers[0].harness='PRIVATE_NAME';u.providers[0].pricing.current.api_estimate.missing_models=['PRIVATE_MODEL'];u.PRIVATE='secret';
  const data=new Dashboard(()=>{},async(_p,k)=>k==='integration-status'?status():u);await data.refresh('/one');
  const output=flatten(rows(data.state));assert.equal(output.find(r=>r.id==='provider.0').label,'Unknown provider');assert.match(output.find(r=>r.id==='provider.0.api_estimate').description,/Unavailable/);assert.ok(!JSON.stringify(output).includes('PRIVATE'));
});
