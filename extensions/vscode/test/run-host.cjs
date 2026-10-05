const path = require('node:path');
const fs = require('node:fs');
const os = require('node:os');
const { runTests } = require('@vscode/test-electron');
(async () => {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'odometer-vscode-test-'));
  const userData = path.join(profile, 'user'); fs.mkdirSync(path.join(userData, 'User'), {recursive:true});
  fs.writeFileSync(path.join(userData,'User','settings.json'), JSON.stringify({'telemetry.telemetryLevel':'off','update.mode':'none','extensions.autoUpdate':false,'extensions.autoCheckUpdates':false,'workbench.startupEditor':'none','workbench.enableExperiments':false}));
  await runTests({ version: process.env.VSCODE_TEST_VERSION || 'stable', extensionDevelopmentPath:path.resolve(__dirname,'..'), extensionTestsPath:path.resolve(__dirname,'host.cjs'), launchArgs:[...(process.env.ODOMETER_TEST_EVIDENCE ? ['--remote-debugging-port=9223'] : []),'--disable-gpu','--disable-extensions','--skip-welcome','--user-data-dir='+userData,'--extensions-dir='+path.join(profile,'extensions')], extensionTestsEnv:{ ODOMETER_TEST_EXECUTABLE:process.env.ODOMETER_TEST_EXECUTABLE || '', ODOMETER_TEST_EXPECT_TOKENS:process.env.ODOMETER_TEST_EXPECT_TOKENS || '', ODOMETER_TEST_EVIDENCE:process.env.ODOMETER_TEST_EVIDENCE || '' }});
})().catch(error=>{ console.error(error.message);process.exitCode=1;});
