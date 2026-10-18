export function createExtensionsPage({ invoke, esc, toast, modal, htmlModal, getView }) {
  const $ = (selector) => document.querySelector(selector);

  function bindActions(root) {
    root.querySelectorAll('[data-ext-action]').forEach((button) => {
      if (button.dataset.extBound === 'true') return;
      button.dataset.extBound = 'true';
      button.addEventListener('click', async () => handleAction(button));
    });
  }

  async function renderExtensions() {
    const host = $('#view-extensions');
    host.innerHTML = `<div class="plugins-head"><div><h2>Extensions</h2><p class="mu">Run applications, background services, and one-shot commands independently of automation workflows.</p></div><div class="plugins-actions"><button class="btn" data-ext-action="folder">Open extension folder</button><button class="btn" data-ext-action="refresh">Refresh</button><button class="btn primary" data-ext-action="install">+ Install ZIP extension…</button></div></div>
      <div class="ext-notice card glass"><b>Execution permissions</b><p class="mu">External extensions can run any interpreter or executable available on your machine. They run as your Windows user and are not sandboxed. Inspect the source and runtime command before starting a service or running a command.</p></div>
      <div id="extension-list"><p class="mu">Loading extensions…</p></div>
      <div id="extension-errors"></div>
      <div class="card glass ext-roadmap"><h3>Native Windows mods</h3><p class="mu">DLL-based Explorer, Start, taskbar, and shell modifications are managed separately in Native Mods. This view only manages external processes and bounded one-shot commands.</p></div>`;
    bindActions(host);
    await refreshList();
  }

  async function refreshList() {
    try {
      const info = await invoke('list_extensions');
      if (getView() !== 'extensions') return;
      const list = $('#extension-list');
      if (!info.packages.length) {
        list.innerHTML = `<div class="card glass plugin-empty"><div class="plugin-icon">＋</div><b>No independent extensions installed</b><p class="mu">Install a ZIP package with a root manifest.json to add an application, service, or one-shot command.</p><button class="btn primary" data-ext-action="install">Install your first extension</button></div>`;
      } else {
        list.innerHTML = `<div class="ext-packages">${info.packages.map((pkg) => `<article class="card glass ext-package">
          <div class="ext-package-head"><div><h3>${esc(pkg.name)} <small class="mu">v${esc(pkg.version)}</small></h3><p class="mu">${esc(pkg.description || 'No description provided.')}</p><code>${esc(pkg.id)}</code></div><button class="btn danger" data-ext-action="uninstall" data-package-id="${esc(pkg.id)}">Remove</button></div>
          <div class="ext-components">${pkg.components.map((component) => `<div class="ext-component">
            <div class="ext-component-info"><b>${esc(component.name)}</b><small class="mu">${esc(component.kind)} · ${esc(component.status)}</small>${component.description ? `<p class="mu">${esc(component.description)}</p>` : ''}<code>${esc(component.command)}</code></div>
            <div class="ext-component-actions">${component.kind === 'command'
              ? `<button class="btn primary" data-ext-action="run-command" data-package-id="${esc(pkg.id)}" data-component-id="${esc(component.id)}">Run</button>`
              : component.running
                ? `<button class="btn" data-ext-action="restart" data-package-id="${esc(pkg.id)}" data-component-id="${esc(component.id)}">Restart</button><button class="btn danger" data-ext-action="stop" data-package-id="${esc(pkg.id)}" data-component-id="${esc(component.id)}">Stop</button>`
                : `<button class="btn primary" data-ext-action="start" data-package-id="${esc(pkg.id)}" data-component-id="${esc(component.id)}">Start</button>`}
            </div>
          </div>`).join('')}</div></article>`).join('')}</div>`;
      }
      const errHost = $('#extension-errors');
      errHost.innerHTML = info.errors.length
        ? `<div class="plugin-errors"><b>Extension loading warnings</b><ul>${info.errors.map((error) => `<li>${esc(error)}</li>`).join('')}</ul></div>`
        : '';
      bindActions(list);
    } catch (error) {
      $('#extension-list').innerHTML = `<p class="er">Could not load extensions: ${esc(error)}</p><button class="btn" data-ext-action="refresh">Retry</button>`;
      bindActions($('#extension-list'));
    }
  }

  async function handleAction(button) {
    const action = button.dataset.extAction;
    const packageId = button.dataset.packageId;
    const componentId = button.dataset.componentId;
    try {
      if (action === 'folder') {
        await invoke('open_extensions_folder');
      } else if (action === 'refresh') {
        await renderExtensions();
        return;
      } else if (action === 'install') {
        await installPackage();
      } else if (action === 'uninstall') {
        const answer = await modal({
          title: 'Remove extension?',
          body: `This removes "${packageId}" from 00047 and stops its managed processes. Files outside the package folder are not removed.`,
          buttons: [['cancel', 'Cancel'], ['go', 'Remove', 'danger']]
        });
        if (answer !== 'go') return;
        await invoke('extension_uninstall', { packageId });
        toast('Extension removed');
      } else if (action === 'start') {
        await invoke('extension_start', { packageId, componentId });
        toast('Extension started');
      } else if (action === 'run-command') {
        const result = await invoke('extension_run_command', { packageId, componentId });
        const outcome = result.timedOut
          ? 'Timed out (configured limit reached)'
          : result.success
            ? `Succeeded (exit code ${result.exitCode ?? 'unknown'})`
            : `Failed (exit code ${result.exitCode ?? 'unknown'})`;
        const clipForDisplay = (value) => {
          const text = String(value || '');
          return text.length > 12000 ? `${text.slice(0, 12000)}\\n[display clipped; full capture was bounded by 00047]` : (text || '(no output)');
        };
        await htmlModal(
          `Command result: ${componentId}`,
          `<p><b>${esc(outcome)}</b> · ${esc(result.durationMs)} ms</p>${result.outputTruncated ? '<p class="er">Output exceeded the capture limit and was truncated.</p>' : ''}<h4>Standard output</h4><pre>${esc(clipForDisplay(result.stdout))}</pre><h4>Standard error</h4><pre>${esc(clipForDisplay(result.stderr))}</pre>`,
          [['close', 'Close']]
        );
      } else if (action === 'stop') {
        await invoke('extension_stop', { packageId, componentId });
        toast('Extension stopped');
      } else if (action === 'restart') {
        await invoke('extension_restart', { packageId, componentId });
        toast('Extension restarted');
      }
    } catch (error) {
      toast(`Extension operation failed: ${error}`, 'err');
    }
    if (getView() === 'extensions') await renderExtensions();
  }

  async function installPackage() {
    try {
      const preview = await invoke('extension_pack_preview');
      if (!preview) return;
      const components = preview.components.map((component) => `<li><b>${esc(component.name)}</b> <small>${esc(component.kind)} · ${esc(component.id)}${component.kind === 'command' ? ` · timeout ${esc(component.timeoutMs ?? 15000)} ms` : ''}</small><pre>${esc(component.command)}</pre>${component.description ? `<p class="mu">${esc(component.description)}</p>` : ''}</li>`).join('');
      const answer = await htmlModal(`Install "${preview.name}"?`,
        `<p>Package <code>${esc(preview.id)}</code> · version ${esc(preview.version)}</p><p>${esc(preview.description || '')}</p><p>The following components may execute code as your Windows user:</p><ul class="bp">${components}</ul><p class="er">Install only packages you trust. Code is not sandboxed.</p>`,
        [['cancel', 'Cancel'], ['go', 'Install package', 'primary']]);
      if (answer !== 'go') return;
      await invoke('extension_pack_install', { path: preview.path });
      toast('Extension package installed');
    } catch (error) {
      toast(`Extension install failed: ${error}`, 'err');
    }
  }

  return renderExtensions;
}
