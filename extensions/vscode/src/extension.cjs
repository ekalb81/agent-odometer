const { Dashboard, rows } = require('./dashboard.cjs');
function createExtension(vscode, context, runner) {
  const events = new vscode.EventEmitter();
  const status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 10);
  status.command = 'odometer.open'; status.name = 'Odometer local data';
  const data = new Dashboard(update, runner);
  const provider = {
    onDidChangeTreeData: events.event,
    getChildren: item => item ? item.children : rows(data.state),
    getTreeItem: item => {
      const tree = new vscode.TreeItem(item.label, item.children.length ? vscode.TreeItemCollapsibleState.Expanded : vscode.TreeItemCollapsibleState.None);
      tree.id = item.id; tree.description = item.description; tree.tooltip = item.label + ': ' + item.description;
      tree.accessibilityInformation = { label: item.label + ': ' + item.description };
      if (!item.children.length) tree.command = { command: 'odometer.inspect', title: 'Show full value', arguments: [item.label + ': ' + item.description] };
      if (item.command) tree.command = { command: item.command, title: item.label };
      return tree;
    },
  };
  const view = vscode.window.createTreeView('odometer.dashboard', { treeDataProvider: provider });
  let timer;
  const settings = () => {
    const config = vscode.workspace.getConfiguration('odometer');
    // Explicit global/default reads prevent a repository from choosing an executable.
    return { executable: config.inspect('executablePath')?.globalValue ?? '', seconds: config.inspect('refreshSeconds')?.globalValue ?? 0 };
  };
  function update() {
    const state = data.state;
    const usage = state.snapshot?.usage;
    const old = state.snapshot && (Date.now() - state.snapshot.fetchedAt > 300000 || Date.now() < state.snapshot.fetchedAt);
    status.text = state.busy ? '$(sync~spin) Odometer' : state.error || old || state.snapshot?.usageError || state.snapshot?.status.ledger_available === false ? '$(warning) Odometer: stale/unavailable' : usage ? '$(dashboard) Odometer: ' + usage.tokens.total_tokens.toLocaleString('en-US') + ' tokens' : '$(dashboard) Odometer: local data';
    status.tooltip = state.snapshot ? 'Recorded usage only. Open for observation age, incomplete history, pricing provenance, and setup.' : 'Open the local dashboard to select Odometer and read its ledger.';
    status.show(); events.fire();
  }
  async function refresh() {
    if (!vscode.workspace.isTrusted) { data.reset('setup'); return; }
    await data.refresh(settings().executable);
  }
  function schedule() {
    clearInterval(timer);
    const seconds = settings().seconds;
    // A cheap UI clock keeps snapshot age honest even when automatic queries are off.
    let elapsed = 0;
    timer = setInterval(() => {
      update();
      if (!view.visible) { elapsed = 0; return; }
      elapsed += 15;
      if (Number.isInteger(seconds) && seconds > 0 && seconds <= 3600 && elapsed >= Math.max(30, seconds)) { elapsed = 0; void refresh(); }
    }, 15000);
  }
  const subscriptions = [events, status, view,
    vscode.commands.registerCommand('odometer.refresh', refresh),
    vscode.commands.registerCommand('odometer.inspect', value => { if (typeof value === 'string' && value.length <= 4096) void vscode.window.showInformationMessage(value); }),
    vscode.commands.registerCommand('odometer.open', async () => { await vscode.commands.executeCommand('odometer.dashboard.focus'); await refresh(); }),
    vscode.commands.registerCommand('odometer.setup', async () => {
      const document = await vscode.workspace.openTextDocument(vscode.Uri.joinPath(context.extensionUri, 'README.md'));
      await vscode.window.showTextDocument(document, { preview: true });
    }),
    vscode.commands.registerCommand('odometer.configure', async () => {
      if (!vscode.workspace.isTrusted) return;
      const selected = await vscode.window.showOpenDialog({ canSelectFiles: true, canSelectFolders: false, canSelectMany: false, openLabel: 'Use Odometer executable', title: 'Select agent-odometer (agent-odometer.exe on Windows)' });
      if (selected?.[0]) await vscode.workspace.getConfiguration('odometer').update('executablePath', selected[0].fsPath, vscode.ConfigurationTarget.Global);
    }),
    vscode.workspace.onDidChangeConfiguration(event => {
      if (event.affectsConfiguration('odometer')) { data.reset(); schedule(); if (view.visible) void refresh(); }
    }),
    view.onDidChangeVisibility(event => { if (event.visible) void refresh(); }),
    { dispose: () => { clearInterval(timer); data.dispose(); } },
  ];
  context.subscriptions.push(...subscriptions); update(); schedule();
  return { data, provider, refresh, view };
}
function activate(context) { return createExtension(require('vscode'), context); }
module.exports = { activate, createExtension };
